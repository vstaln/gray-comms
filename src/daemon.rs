//! The comms daemon: a TCP listener holding rooms, presence, and durable
//! history. Clients speak NDJSON (proto.rs); listeners (`join` with
//! `listen:true`) get live `notify` pushes, everyone else polls `peek`.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::proto;
use crate::store::Store;

/// The queue file next to the config.
pub fn open_store(config_path: &Path) -> Result<Store, String> {
    Store::new(&Store::default_path(config_path))
}

/// Daemon-side state shared by every connection task.
pub struct Daemon {
    store: Store,
    /// Configured shared secret; `None` = localhost-trust mode.
    token: Option<String>,
    history_limit: u64,
    /// agent name → push channel, only while a `listen` connection lives.
    live: Mutex<HashMap<String, mpsc::Sender<String>>>,
    /// "Who answers" routing (disabled = every room behaves as today).
    routing: crate::routing::Routing,
    /// routing-log directory under the comms data dir (`None` for tests
    /// on the in-memory store).
    log_dir: Option<std::path::PathBuf>,
}

type Reply = Result<Value, (String, String)>;

fn cfg_u64(config: &Value, key: &str, default: u64) -> u64 {
    config.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn cfg_str(config: &Value, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Serve forever on `bind:port`. Returns only on a fatal bind/store error.
pub async fn serve(config: &Value, store: Store) -> Result<(), String> {
    let bind = cfg_str(config, "bind");
    let bind = if bind.is_empty() { "127.0.0.1" } else { &bind };
    let port = cfg_u64(config, "port", 19876);
    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("cannot bind {addr}: {e}"))?;
    serve_on(listener, config, store).await
}

/// Serve on an already-bound listener (tests bind :0 for a free port).
pub async fn serve_on(listener: TcpListener, config: &Value, store: Store) -> Result<(), String> {
    let token = config
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let log_dir = store.data_dir().map(|d| d.join("routing-log"));
    let daemon = Arc::new(Daemon {
        store,
        token,
        routing: crate::routing::Routing::from_config(config),
        log_dir,
        history_limit: cfg_u64(config, "history_limit", 500),
        live: Mutex::new(HashMap::new()),
    });
    if let Ok(local) = listener.local_addr() {
        eprintln!("gray-comms listening on {local}");
    }
    // Heartbeat: an idle listener otherwise hits the client's IO timeout
    // and drops, missing later pushes. Beat well under that timeout.
    {
        let d = daemon.clone();
        let every =
            std::time::Duration::from_millis(cfg_u64(config, "keepalive_ms", 5_000).max(50));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await; // interval fires immediately; wait one out
            loop {
                tick.tick().await;
                let line = proto::encode(&proto::heartbeat());
                if let Ok(live) = d.live.lock() {
                    for tx in live.values() {
                        let _ = tx.try_send(line.clone());
                    }
                }
            }
        });
    }
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let d = daemon.clone();
        tokio::spawn(async move {
            let _ = handle_conn(d, sock, peer).await;
        });
    }
}

/// One client connection: the read loop dispatches requests, a writer task
/// owns the socket so responses and live pushes never interleave mid-line.
async fn handle_conn(daemon: Arc<Daemon>, sock: TcpStream, _peer: SocketAddr) -> Result<(), String> {
    let (read, mut write) = sock.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut me: Option<String> = None;
    let mut reader = BufReader::new(read);
    loop {
        let mut buf = Vec::new();
        let n = match reader.read_until(b'\n', &mut buf).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 || buf.len() > proto::MAX_LINE {
            break;
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => continue,
        };
        let Some(id) = req.get("id").cloned().filter(|v| v.is_number()) else {
            continue; // client→daemon traffic is always request/response
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = proto::params_of(&req);
        let reply = match dispatch(&daemon, method, &params, &mut me, &tx) {
            Ok(v) => proto::ok(&id, v),
            Err((code, msg)) => proto::err(&id, &code, msg),
        };
        if tx.send(proto::encode(&reply)).await.is_err() {
            break;
        }
    }
    // Connection closed: drop presence; memberships and history persist.
    if let Some(name) = me {
        if let Ok(mut live) = daemon.live.lock() {
            if live.get(&name).is_some_and(|s| s.same_channel(&tx)) {
                live.remove(&name);
            }
        }
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn dispatch(
    d: &Daemon,
    method: &str,
    params: &Value,
    me: &mut Option<String>,
    tx: &mpsc::Sender<String>,
) -> Reply {
    match method {
        "join" => return join(d, params, me, tx),
        "ping" => return Ok(json!({"pong": true})),
        _ => {}
    }
    let Some(name) = me.clone() else {
        return Err(("unauthorized".to_string(), "join first".to_string()));
    };
    match method {
        "leave" => leave(d, params, &name),
        "join_room" => join_room(d, params, &name),
        "say" => say(d, params, &name),
        "dm" => dm(d, params, &name),
        "who" => who(d, params),
        "rooms" => rooms(d),
        "peek" => peek(d, params, &name),
        "status" => set_status(d, params, &name),
        _ => Err(("unknown_method".to_string(), format!("no method {method}"))),
    }
}

/// join: register/refresh presence. `listen:true` attaches the push channel
/// and drains queued DMs as dm events. `rooms:[..]` subscribes on the spot.
fn join(
    d: &Daemon,
    params: &Value,
    me: &mut Option<String>,
    tx: &mpsc::Sender<String>,
) -> Reply {
    // Auth: when the daemon is configured with a token, every join presents it.
    if let Some(want) = &d.token {
        let got = proto::str_param(params, "auth").unwrap_or("");
        if got != want {
            return Err((
                "unauthorized".to_string(),
                "bad or missing auth token".to_string(),
            ));
        }
    }
    let name = proto::str_param(params, "name").unwrap_or("").trim();
    if !proto::agent_name_valid(name) {
        return Err((
            "bad_name".to_string(),
            "name must be 1-64 chars of [A-Za-z0-9_.-]".to_string(),
        ));
    }
    let status = proto::str_param(params, "status")
        .filter(|s| proto::status_valid(s))
        .map(str::to_string);
    let listen = params
        .get("listen")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    d.store
        .upsert_agent(name, status.as_deref())
        .map_err(internal)?;
    let missed = d.store.undelivered_dms(name).map_err(internal)?;
    if listen {
        if let Ok(mut live) = d.live.lock() {
            live.insert(name.to_string(), tx.clone());
        }
        // Drain the offline queue as live dm events.
        let ids: Vec<String> = missed
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        for m in &missed {
            let _ = tx.try_send(proto::encode(&proto::notify(json!({
                "type": "dm",
                "message": m,
            }))));
        }
        let _ = d.store.mark_delivered(&ids);
    }
    *me = Some(name.to_string());
    let rooms = d.store.agent_rooms(name).map_err(internal)?;
    let mut joined = Vec::new();
    if let Some(list) = params.get("rooms").and_then(Value::as_array) {
        for r in list.iter().filter_map(Value::as_str) {
            if let Some(room) = proto::room_normalize(r) {
                if d.store.join_room(&room, name).map_err(internal)? {
                    broadcast(
                        d,
                        &room,
                        name,
                        json!({
                            "type": "member_joined",
                            "room": room,
                            "agent": name,
                        }),
                        crate::routing::Targets::All,
                    );
                }
                joined.push(room);
            }
        }
    }
    Ok(json!({
        "agent": {
            "id": name,
            "name": name,
            "status": status.unwrap_or_else(|| "active".to_string()),
        },
        "rooms": rooms,
        "joined": joined,
        "missed": missed.len(),
    }))
}

/// leave `{room?}`: one room, or every room when `room` is absent.
fn leave(d: &Daemon, params: &Value, me: &str) -> Reply {
    if let Some(raw) = proto::str_param(params, "room") {
        let room = proto::room_normalize(raw).ok_or_else(bad_room)?;
        if !d.store.leave_room(&room, me).map_err(internal)? {
            return Err(("not_member".to_string(), format!("not a member of {room}")));
        }
        broadcast(
            d,
            &room,
            me,
            json!({
                "type": "member_left",
                "room": room,
                "agent": me,
            }),
            crate::routing::Targets::All,
        );
        return Ok(json!({"left": [room]}));
    }
    let rooms = d.store.leave_all(me).map_err(internal)?;
    for room in &rooms {
        broadcast(
            d,
            room,
            me,
            json!({
                "type": "member_left",
                "room": room,
                "agent": me,
            }),
            crate::routing::Targets::All,
        );
    }
    Ok(json!({"left": rooms}))
}

/// join_room `{room}`: subscribe to (or create) a room.
fn join_room(d: &Daemon, params: &Value, me: &str) -> Reply {
    let raw = proto::str_param(params, "room").unwrap_or("");
    let room = proto::room_normalize(raw).ok_or_else(bad_room)?;
    let fresh = d.store.join_room(&room, me).map_err(internal)?;
    if fresh {
        broadcast(
            d,
            &room,
            me,
            json!({
                "type": "member_joined",
                "room": room,
                "agent": me,
            }),
            crate::routing::Targets::All,
        );
    }
    Ok(json!({
        "room": room,
        "members": d.store.room_members(&room).map_err(internal)?,
    }))
}

/// say `{room,text}` to a room the sender belongs to.
fn say(d: &Daemon, params: &Value, me: &str) -> Reply {
    let raw = proto::str_param(params, "room").unwrap_or("");
    let room = proto::room_normalize(raw).ok_or_else(bad_room)?;
    let text = checked_text(params)?;
    if !d.store.is_member(&room, me).map_err(internal)? {
        return Err((
            "not_member".to_string(),
            format!("not a member of {room}; join it first"),
        ));
    }
    let msg = d
        .store
        .add_room_message(&room, me, text, d.history_limit)
        .map_err(internal)?;
    let members = d.store.room_members(&room).map_err(internal)?;
    let targets = d
        .routing
        .route(&room, me, text, &members, d.log_dir.as_deref());
    broadcast(
        d,
        &room,
        me,
        json!({
            "type": "room_message",
            "message": msg,
        }),
        targets,
    );
    Ok(json!({"message": msg}))
}

/// dm `{to,text}`; queued in the store when the recipient is not listening.
fn dm(d: &Daemon, params: &Value, me: &str) -> Reply {
    let to = proto::str_param(params, "to").unwrap_or("").trim();
    if !proto::agent_name_valid(to) {
        return Err(("bad_name".to_string(), "to must be an agent name".to_string()));
    }
    let text = checked_text(params)?;
    if !d.store.agent_exists(to).map_err(internal)? {
        return Err((
            "unknown_agent".to_string(),
            format!("no agent named {to} has ever joined"),
        ));
    }
    let msg = d.store.add_dm(me, to, text, false).map_err(internal)?;
    // Push to a live listener; a failed push leaves the row queued.
    let delivered = d
        .live
        .lock()
        .ok()
        .and_then(|l| l.get(to).cloned())
        .is_some_and(|tx| {
            tx.try_send(proto::encode(&proto::notify(json!({
                "type": "dm",
                "message": msg,
            }))))
            .is_ok()
        });
    if delivered {
        if let Some(id) = msg.get("id").and_then(Value::as_str) {
            let _ = d.store.mark_delivered(&[id.to_string()]);
        }
    }
    Ok(json!({
        "message": msg,
        "delivery": if delivered { "delivered" } else { "queued" },
    }))
}

/// who `{room?}` → presence list (room members or every known agent).
fn who(d: &Daemon, params: &Value) -> Reply {
    let live: Vec<String> = d
        .live
        .lock()
        .map(|l| l.keys().cloned().collect())
        .unwrap_or_default();
    let mut agents = if let Some(raw) = proto::str_param(params, "room") {
        let room = proto::room_normalize(raw).ok_or_else(bad_room)?;
        if !d.store.room_exists(&room).map_err(internal)? {
            return Err(("unknown_room".to_string(), format!("no room {room}")));
        }
        let members = d.store.room_members(&room).map_err(internal)?;
        let all = d.store.agents().map_err(internal)?;
        all.into_iter()
            .filter(|a| {
                a.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| members.iter().any(|m| m == n))
            })
            .collect()
    } else {
        d.store.agents().map_err(internal)?
    };
    for a in &mut agents {
        let name = a
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !live.iter().any(|n| n == &name) {
            a["status"] = Value::String("offline".to_string());
        }
        a["rooms"] = Value::Array(
            d.store
                .agent_rooms(&name)
                .unwrap_or_default()
                .into_iter()
                .map(Value::String)
                .collect(),
        );
    }
    Ok(json!({"agents": agents}))
}

fn rooms(d: &Daemon) -> Reply {
    Ok(json!({"rooms": d.store.rooms().map_err(internal)?}))
}

/// peek: `{room}` room history · `{with}` DM thread · `{unread:true}` my
/// undelivered DMs (then marked delivered) · bare = my DM inbox.
fn peek(d: &Daemon, params: &Value, me: &str) -> Reply {
    let limit = proto::limit_param(params);
    if params.get("unread").and_then(Value::as_bool) == Some(true) {
        let msgs = d.store.undelivered_dms(me).map_err(internal)?;
        let ids: Vec<String> = msgs
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let capped: Vec<Value> = msgs.into_iter().take(limit.max(1)).collect();
        let _ = d.store.mark_delivered(&ids);
        return Ok(json!({"messages": capped}));
    }
    if let Some(raw) = proto::str_param(params, "room") {
        let room = proto::room_normalize(raw).ok_or_else(bad_room)?;
        if !d.store.is_member(&room, me).map_err(internal)? {
            return Err(("not_member".to_string(), format!("not a member of {room}")));
        }
        let msgs = d.store.room_history(&room, limit).map_err(internal)?;
        return Ok(json!({"room": room, "messages": msgs}));
    }
    if let Some(with) = proto::str_param(params, "with") {
        if !proto::agent_name_valid(with) {
            return Err(("bad_name".to_string(), "with must be an agent name".to_string()));
        }
        let msgs = d.store.dm_thread(me, with, limit).map_err(internal)?;
        return Ok(json!({"with": with, "messages": msgs}));
    }
    let msgs = d.store.dm_inbox(me, limit).map_err(internal)?;
    Ok(json!({"messages": msgs}))
}

fn set_status(d: &Daemon, params: &Value, me: &str) -> Reply {
    let status = proto::str_param(params, "status").unwrap_or("");
    if !proto::status_valid(status) {
        return Err((
            "bad_status".to_string(),
            "status must be active, idle or busy".to_string(),
        ));
    }
    d.store.set_status(me, status).map_err(internal)?;
    Ok(json!({"status": status}))
}

/// Push an event to live members except the sender. `targets` narrows the
/// recipient set for routed room messages; `All` is today's fan-out.
fn broadcast(d: &Daemon, room: &str, from: &str, event: Value, targets: crate::routing::Targets) {
    let Ok(members) = d.store.room_members(room) else {
        return;
    };
    let line = proto::encode(&proto::notify(event));
    if let Ok(live) = d.live.lock() {
        for m in members {
            if m == from {
                continue;
            }
            if let crate::routing::Targets::Only(ref pick) = targets {
                if !pick.iter().any(|p| p == &m) {
                    continue;
                }
            }
            if let Some(tx) = live.get(&m) {
                let _ = tx.try_send(line.clone());
            }
        }
    }
}

fn checked_text(params: &Value) -> Result<&str, (String, String)> {
    let text = proto::str_param(params, "text").unwrap_or("").trim();
    if text.is_empty() {
        return Err(("empty".to_string(), "text is empty".to_string()));
    }
    if text.len() > proto::MAX_TEXT {
        return Err((
            "too_large".to_string(),
            "text exceeds the 16 KiB message limit".to_string(),
        ));
    }
    Ok(text)
}

fn bad_room() -> (String, String) {
    (
        "bad_room".to_string(),
        "room must be a slug like #dev or dev".to_string(),
    )
}

fn internal(e: String) -> (String, String) {
    ("internal".to_string(), e)
}
