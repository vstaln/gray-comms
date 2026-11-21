//! NDJSON protocol 1.1 sidecar.
//! Manifest answers without config; a down daemon fails fast and safe.
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;

/// Static manifest (`comms` 0.1.x, protocol 1.1, no commands).
pub static MANIFEST: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "name": "comms",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "commands": ["/comms"],
        "command_descriptions": {"/comms": "message other agents on the comms mesh"},
        "hooks": ["prompt/context"],
        "tools": [
            {
                "name": "comms_join",
                "label": "Comms Join",
                "description": "Subscribe this session to a mesh room. Presence is automatic (each Gray session joins as its own name, busy/idle from activity), so this only matters for rooms; it returns the room membership.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room to join (e.g. dev or #dev); omitted = presence only"},
                        "status": {"type": "string", "enum": ["active", "idle", "busy"], "description": "Presence to advertise"}
                    }
                }
            },
            {
                "name": "comms_say",
                "label": "Comms Say",
                "description": "Post a message to a comms room this agent belongs to. Room defaults to the configured home room.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room name; default: the configured home room"},
                        "text": {"type": "string", "description": "Message text (plain, up to 16 KiB)"}
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "comms_dm",
                "label": "Comms DM",
                "description": "Send a direct message to another agent by name. Online agents get it pushed; offline agents get it on their next check-in. Reply reports delivery.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {"type": "string", "description": "Recipient agent name"},
                        "text": {"type": "string", "description": "Message text"}
                    },
                    "required": ["to", "text"]
                }
            },
            {
                "name": "comms_send",
                "label": "Comms Send",
                "description": "Message another live Gray session by short id, id prefix, pid, cwd substring, or all:true. Lands in that session's context mid-turn through gray send (exactly-once, state is Gray's own receipt); a target that is not a live session falls back to the mesh DM queue, which it picks up at its next check-in. Sender label is this session's mesh name, added automatically.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {"type": "string", "description": "Target: live session short id or prefix, pid, cwd substring, or mesh agent name"},
                        "text": {"type": "string", "description": "Message text (plain, one screen at most)"},
                        "all": {"type": "boolean", "description": "Send to every other live session"},
                        "mode": {"type": "string", "enum": ["message", "kickstart"], "description": "message (default) lands as context the target may act on; kickstart frames it as a directive the target acts on now"},
                        "ttl": {"type": "string", "description": "How long queued mail waits for a target that is not running (default 24h; e.g. 90m, 7d)"}
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "comms_status",
                "label": "Comms Status",
                "description": "Claim what this session is working on, in free text (e.g. \"editing src/session.rs\"). The claim shows up in every other live Gray session's awareness block until it expires after 30 minutes; action:\"clear\" removes it early. Use it before starting a change another session could collide with.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "What you are working on, one line"},
                        "action": {"type": "string", "enum": ["set", "clear"], "description": "set (default) claims; clear drops the claim early"}
                    }
                }
            },
            {
                "name": "comms_who",
                "label": "Comms Who",
                "description": "List agents on the mesh (or the members of one room) with live presence: active/idle/busy while connected, offline otherwise.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Restrict to members of this room"}
                    }
                }
            },
            {
                "name": "comms_read",
                "label": "Comms Read",
                "description": "Read comms history: a room's recent messages (room), a DM thread with one agent (with), this agent's DM inbox (no args), or only newly arrived DMs (unread:true — also what the prompt hook previews).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room to read (must be a member)"},
                        "with": {"type": "string", "description": "Agent name: the DM thread with them"},
                        "unread": {"type": "boolean", "description": "Only newly arrived DMs"},
                        "limit": {"type": "integer", "description": "Max messages (default 20, max 100)"}
                    }
                }
            }
        ]
    })
});

const PROMPT_CONTEXT: &str = "Comms: comms_join/comms_say for rooms, comms_dm for mesh DMs (queued while offline), comms_read for history, comms_who for mesh agents; new DMs are flagged below. Plain text only; never send secrets.";

fn is_error() -> Value {
    json!({
        "content": "Comms call failed. Run `gray-comms doctor`; do not blindly retry.",
        "is_error": true
    })
}

/// A specific, fixable failure. `detail` must never contain config values.
fn not_done(detail: impl std::fmt::Display) -> Value {
    json!({
        "content": format!("Not done: {detail}. Fix it and call again."),
        "is_error": true
    })
}

/// Dispatch one protocol method.
pub fn dispatch(method: &str, params: &Value, config_path: &Path) -> Value {
    if method == "plugin/manifest" {
        return MANIFEST.clone();
    }
    if method == "prompt/context" {
        return prompt_context(config_path, params);
    }
    if method == "command/run" {
        return run_command(params, config_path);
    }
    if method == "tool/call" {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        return match name {
            "comms_join" | "comms_say" | "comms_dm" | "comms_who" | "comms_read" | "comms_send"
            | "comms_status" => tool_call(name, params, config_path),
            _ => is_error(),
        };
    }
    json!({"error": "Unsupported method"})
}

fn tool_call(name: &str, params: &Value, config_path: &Path) -> Value {
    let args = params.get("args").cloned().unwrap_or(Value::Null);
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => return not_done(e),
    };
    let session = crate::identity::session_id(params);
    let mesh = match crate::identity::mesh_name_now(&config, session.as_deref()) {
        Ok(n) => n,
        Err(e) => return not_done(e),
    };
    if let Some(sid) = &session {
        crate::presence::start(config.clone(), sid.clone(), mesh.clone());
    }
    if name == "comms_send" {
        return match send_tool(&config, &session, &mesh, &args) {
            Ok(text) => json!({"content": text}),
            Err(e) => not_done(e),
        };
    }
    if name == "comms_status" {
        return match status_tool(&config, &session, &args) {
            Ok(text) => json!({"content": text}),
            Err(e) => not_done(e),
        };
    }
    let mut client = match crate::client::Client::connect_as(&config, &mesh) {
        Ok(c) => c,
        Err(e) => return not_done(e),
    };
    match run_tool(&mut client, name, &args, &config) {
        Ok(text) => json!({"content": text}),
        Err(e) => not_done(e),
    }
}

pub(crate) fn ttl_secs(raw: Option<&str>) -> Result<u64, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(crate::foreign::MAIL_TTL_SECS);
    };
    let (num, mult) = match raw.chars().last() {
        Some('s') => (&raw[..raw.len() - 1], 1),
        Some('m') => (&raw[..raw.len() - 1], 60),
        Some('h') => (&raw[..raw.len() - 1], 3600),
        Some('d') => (&raw[..raw.len() - 1], 86_400),
        _ => (raw, 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("\"ttl\" must look like 90s, 30m, 24h or 7d, not \"{raw}\""))?;
    if n == 0 {
        return Err("\"ttl\" must be at least 1 second".to_string());
    }
    Ok(n.saturating_mul(mult))
}

pub(crate) fn status_tool(
    config: &Value,
    session: &Option<String>,
    args: &Value,
) -> Result<String, String> {
    use crate::claim::Action;
    let action = Action::parse(args.get("action").and_then(Value::as_str))?;
    let sid = session.clone().unwrap_or_default();
    if sid.trim().is_empty() {
        return Err("claims need a Gray session: this call carried no session id".to_string());
    }
    match action {
        Action::Clear => {
            let had = crate::claim::clear(config, &sid);
            Ok(if had {
                format!("claim cleared for {sid}")
            } else {
                format!("{sid} had no claim")
            })
        }
        Action::Set => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            let claim = crate::claim::set(config, &sid, text)?;
            Ok(format!(
                "claim set for {sid}: {} — other live sessions see it in their next context block; it expires in {}m",
                claim.render(),
                claim.remaining_secs() / 60
            ))
        }
    }
}

pub(crate) fn send_tool(
    config: &Value,
    session: &Option<String>,
    mesh: &str,
    args: &Value,
) -> Result<String, String> {
    use crate::deliver::{Mode, Target};
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("text is empty".to_string());
    }
    let mode = Mode::parse(args.get("mode").and_then(Value::as_str))?;
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let target = match Target::parse(args.get("to").and_then(Value::as_str), all)? {
        Target::One(raw) => Target::One(crate::nick::resolve(config, &raw).unwrap_or(raw)),
        other => other,
    };
    let me = session.clone().unwrap_or_default();
    let views = crate::session::collect(config, &me, 0, None);
    let gray_bin = config.get("gray_bin").and_then(Value::as_str).unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    match target {
        Target::All => {
            let others: Vec<_> = views.iter().filter(|v| !v.me).collect();
            if others.is_empty() {
                return Err("no other live session to broadcast to".to_string());
            }
            for v in others {
                match crate::deliver::send(config, gray_bin, &v.id, mesh, mode, &text) {
                    Ok(sent) => out.push(format!("{} ({}) {}", v.short, v.id, sent.state)),
                    Err(e) => out.push(format!("{} failed: {e}", v.short)),
                }
            }
            Ok(format!(
                "sent as {} from {mesh}: {}",
                mode.label(),
                out.join(", ")
            ))
        }
        Target::One(raw) => match crate::session::find(config, &views, &raw) {
            Ok(v) if v.harness().is_some() => {
                let ttl = ttl_secs(args.get("ttl").and_then(Value::as_str))?;
                let queued = crate::foreign::queue(
                    config,
                    &v.id,
                    &format!("↳ from {mesh}\n{}", mode.payload(&text)),
                    ttl,
                )?;
                crate::mailog::append(config, mesh, &v.id, mode.label(), "queued", &text);
                Ok(format!(
                    "sent as {} from {mesh}: {} ({}) queued for its next hook call ({} message(s) waiting, ttl {}m)",
                    mode.label(),
                    v.short,
                    v.id,
                    queued,
                    ttl / 60
                ))
            }
            Ok(v) => {
                let sent = crate::deliver::send(config, gray_bin, &v.id, mesh, mode, &text)?;
                Ok(format!(
                    "sent as {} from {mesh}: {} ({}) {} id {}",
                    mode.label(),
                    v.short,
                    v.id,
                    sent.state,
                    sent.id.chars().take(8).collect::<String>()
                ))
            }
            Err(why) => {
                if !why.starts_with("no live session matches") {
                    return Err(why);
                }
                if let Ok(id) = crate::session::find_known(config, &raw) {
                    let sent = crate::deliver::send(config, gray_bin, &id, mesh, mode, &text)?;
                    return Ok(format!(
                        "sent as {} from {mesh}: {id} (not live) {} id {}",
                        mode.label(),
                        sent.state,
                        sent.id.chars().take(8).collect::<String>()
                    ));
                }
                let mut notes: Vec<String> = Vec::new();
                if let Some(run) = crate::spawns::find_run(config, raw.trim()) {
                    match crate::deliver::steer(config, gray_bin, &run.name, mesh, mode, &text) {
                        Ok(_) => {
                            let child = if run.child.is_empty() {
                                "its session".to_string()
                            } else {
                                format!("session {}", run.child)
                            };
                            return Ok(format!(
                                "sent as {} from {mesh}: steered subagent run {} — {} is not live in this mesh, the run picks the message up on its next turn",
                                mode.label(),
                                run.name,
                                child
                            ));
                        }
                        Err(e) => notes.push(e),
                    }
                }
                if crate::proto::agent_name_valid(raw.trim()) {
                    if let Ok(mut client) = crate::client::Client::connect_as(config, mesh) {
                        match client.call("dm", json!({"to": raw.trim(), "text": text})) {
                            Ok(res) => {
                                let delivery = res
                                    .get("delivery")
                                    .and_then(Value::as_str)
                                    .unwrap_or("queued");
                                crate::mailog::append(
                                    config,
                                    mesh,
                                    raw.trim(),
                                    mode.label(),
                                    delivery,
                                    &text,
                                );
                                return Ok(format!(
                                    "no live session matched \"{raw}\"; queued a mesh dm from {mesh} to {}: {delivery}",
                                    raw.trim()
                                ));
                            }
                            Err(e) => notes.push(e),
                        }
                    }
                }
                if notes.is_empty() {
                    Err(why)
                } else {
                    Err(format!(
                        "{why}; also tried a mesh dm — {}",
                        notes.join("; ")
                    ))
                }
            }
        },
    }
}

fn run_tool(
    c: &mut crate::client::Client,
    name: &str,
    args: &Value,
    config: &Value,
) -> Result<String, String> {
    match name {
        "comms_join" => {
            let mut out = Vec::new();
            if let Some(status) = args.get("status").and_then(Value::as_str) {
                c.call("status", json!({"status": status}))?;
            }
            if let Some(room) = args.get("room").and_then(Value::as_str) {
                let res = c.call("join_room", json!({"room": room}))?;
                let room = res
                    .get("room")
                    .and_then(Value::as_str)
                    .unwrap_or(room)
                    .to_string();
                let members = res
                    .get("members")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                if !c.rooms.iter().any(|r| r == &room) {
                    c.rooms.push(room.clone());
                }
                out.push(format!("joined #{room} ({members} members)"));
            }
            out.push(format!(
                "on the mesh as {}; rooms: {}",
                c.me,
                if c.rooms.is_empty() {
                    "none".to_string()
                } else {
                    c.rooms.join(", ")
                }
            ));
            Ok(out.join("; "))
        }
        "comms_say" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.trim().is_empty() {
                return Err("text is empty".to_string());
            }
            let room = args
                .get("room")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    config
                        .get("home_room")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .ok_or_else(|| "no room given and no home_room configured".to_string())?;
            let res = c.call("say", json!({"room": room, "text": text}))?;
            let room = res
                .get("message")
                .and_then(|m| m.get("room"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            Ok(format!("posted to #{room}"))
        }
        "comms_dm" => {
            let to = args.get("to").and_then(Value::as_str).unwrap_or("").trim();
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.trim().is_empty() {
                return Err("text is empty".to_string());
            }
            if to.is_empty() {
                return Err(format!(
                    "give `to` — a mesh agent, or any live session handle ({})",
                    crew_handles(config)
                ));
            }
            match c.call("dm", json!({"to": to, "text": text})) {
                Ok(res) => {
                    let delivery = res
                        .get("delivery")
                        .and_then(Value::as_str)
                        .unwrap_or("queued");
                    Ok(format!("dm to {to}: {delivery}"))
                }
                Err(e) => {
                    if let Some(name) = mesh_agent_named(c, to) {
                        let res = c.call("dm", json!({"to": name, "text": text}))?;
                        let delivery = res
                            .get("delivery")
                            .and_then(Value::as_str)
                            .unwrap_or("queued");
                        return Ok(format!("dm to {name} (matched \"{to}\"): {delivery}"));
                    }
                    let gray_bin = config.get("gray_bin").and_then(Value::as_str).unwrap_or("");
                    let mesh = crate::identity::mesh_name_now(config, None)?;
                    let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
                    let views = crate::session::collect(config, &me, 0, None);
                    if let Ok(v) = crate::session::find(config, &views, to) {
                        let sent = crate::deliver::send(
                            config,
                            gray_bin,
                            &v.id,
                            &mesh,
                            crate::deliver::Mode::Message,
                            text,
                        )?;
                        return Ok(format!(
                            "no mesh agent \"{to}\"; sent to live session {} ({}) {} id {}",
                            v.handle(),
                            v.id,
                            sent.state,
                            sent.id.chars().take(8).collect::<String>()
                        ));
                    }
                    if let Some(run) = crate::spawns::find_run(config, to) {
                        crate::deliver::steer(
                            config,
                            gray_bin,
                            &run.name,
                            &mesh,
                            crate::deliver::Mode::Message,
                            text,
                        )?;
                        return Ok(format!(
                            "no mesh agent \"{to}\"; steered subagent run {}",
                            run.name
                        ));
                    }
                    Err(format!("{e}; {}", crew_handles(config)))
                }
            }
        }
        "comms_who" => {
            let mut params = json!({});
            if let Some(room) = args.get("room").and_then(Value::as_str) {
                params["room"] = Value::String(room.to_string());
            }
            let res = c.call("who", params)?;
            let agents = res
                .get("agents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if agents.is_empty() {
                return Ok("no agents".to_string());
            }
            let lines: Vec<String> = agents
                .iter()
                .map(|a| {
                    let name = a.get("name").and_then(Value::as_str).unwrap_or("?");
                    let status = a.get("status").and_then(Value::as_str).unwrap_or("?");
                    let rooms = a
                        .get("rooms")
                        .and_then(Value::as_array)
                        .map(|r| {
                            r.iter()
                                .filter_map(Value::as_str)
                                .map(|s| format!("#{s}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    format!("{name} ({status}) {rooms}")
                })
                .collect();
            Ok(lines.join("\n"))
        }
        "comms_read" => {
            let mut params = json!({});
            for (k, v) in [("room", "room"), ("with", "with")] {
                if let Some(s) = args.get(k).and_then(Value::as_str) {
                    params[v] = Value::String(s.to_string());
                }
            }
            if args.get("unread").and_then(Value::as_bool) == Some(true) {
                params["unread"] = Value::Bool(true);
            }
            if let Some(n) = args.get("limit").and_then(Value::as_u64) {
                params["limit"] = json!(n);
            }
            let res = match c.call("peek", params.clone()) {
                Ok(r) => r,
                Err(e) if e.contains("not_member") => {
                    if let Some(room) = args.get("room").and_then(Value::as_str) {
                        c.call("join_room", json!({"room": room}))?;
                        c.call("peek", params)?
                    } else {
                        return Err(format!("{e}; pass room to join it first"));
                    }
                }
                Err(e) => return Err(e),
            };
            Ok(format_messages(&res))
        }
        _ => Err("unknown tool".to_string()),
    }
}

fn crew_handles(config: &Value) -> String {
    let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
    let views = crate::session::collect(config, &me, 0, None);
    let live: Vec<String> = views
        .iter()
        .filter(|v| v.status.fresh())
        .take(8)
        .map(|v| v.handle())
        .collect();
    if live.is_empty() {
        "no live session to message".to_string()
    } else {
        format!("live sessions: {}", live.join(", "))
    }
}

fn mesh_agent_named(c: &mut crate::client::Client, raw: &str) -> Option<String> {
    let res = c.call("who", json!({})).ok()?;
    let want = raw.to_lowercase();
    res.get("agents")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|a| a.get("name").and_then(Value::as_str))
        .find(|n| {
            let low = n.to_lowercase();
            low == want || low.ends_with(&format!(".{want}"))
        })
        .map(str::to_string)
}

/// Render a peek result as transcript lines: `from: text`.
fn format_messages(res: &Value) -> String {
    let msgs = res
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if msgs.is_empty() {
        return "no messages".to_string();
    }
    msgs.iter()
        .map(|m| {
            let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
            let content = m.get("content").and_then(Value::as_str).unwrap_or("");
            let to = m.get("to").and_then(Value::as_str);
            match to {
                Some(t) if t != "?" => format!("{from} -> {t}: {content}"),
                _ => format!("{from}: {content}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// prompt/context: static tool guidance plus a one-line note when DMs
/// arrived since the last look. Fails soft — a down daemon never blocks a
/// prompt.
fn prompt_context(config_path: &Path, params: &Value) -> Value {
    let mut text = PROMPT_CONTEXT.to_string();
    let arrived = (|| -> Option<Vec<Value>> {
        let config = crate::config::load_config(config_path).ok()?;
        let mut client = crate::client::Client::connect(&config).ok()?;
        let res = client
            .call("peek", json!({"unread": true, "limit": 20}))
            .ok()?;
        res.get("messages")
            .and_then(Value::as_array)
            .cloned()
            .map(|m| m.to_vec())
    })();
    if let Some(msgs) = arrived {
        if !msgs.is_empty() {
            let mut parts = Vec::new();
            for m in msgs.iter().take(5) {
                let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
                let content: String = m
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                parts.push(format!("{from}: {content}"));
            }
            let more = msgs.len().saturating_sub(5);
            let mut note = format!(
                "\n{} new comms DM{} arrived: {}",
                msgs.len(),
                if msgs.len() == 1 { "" } else { "s" },
                parts.join(" | ")
            );
            if more > 0 {
                note.push_str(&format!(" (+{more} more)"));
            }
            note.push_str(" — read them with comms_read unread:true or comms_read with:<name>.");
            text.push_str(&note);
        }
    }
    if let Ok(config) = crate::config::load_config(config_path) {
        if let Some(sid) = crate::identity::session_id(params) {
            if let Ok(mesh) = crate::identity::mesh_name_now(&config, Some(&sid)) {
                crate::presence::start(config.clone(), sid.clone(), mesh);
            }
            if let Some(block) = crate::aware::block(&config, &sid, 3, None) {
                if crate::aware::changed(&config, &sid, &block) {
                    text = format!("{block}\n{text}");
                }
            }
        }
    }
    json!({"text": text})
}

fn run_command(params: &Value, config_path: &Path) -> Value {
    let mut args: Vec<String> = params
        .get("argv")
        .or_else(|| params.get("args"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let raw = params
        .get("raw")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let head = args.first().map(String::as_str);
    if matches!(head, Some("setup")) || (args.is_empty() && !configured(config_path)) {
        if args.is_empty() {
            args.push("setup".to_string());
        }
        return json!({
            "terminal": args,
            "text": "/comms setup needs an interactive terminal"
        });
    }
    if matches!(head, Some("run" | "sidecar")) {
        return json!({ "text": "Run `gray comms run` in a terminal." });
    }
    if let Some(first) = args.first() {
        if first.starts_with('@') || first.starts_with('!') {
            return addressed_send(&args, &raw, first.starts_with('!'), config_path);
        }
    }
    if args.is_empty() {
        return json!({ "terminal": ["tui"] });
    }
    let Ok(exe) = std::env::current_exe() else {
        return json!({ "text": "cannot locate gray-comms binary" });
    };
    match std::process::Command::new(exe)
        .arg("--config")
        .arg(config_path)
        .args(&args)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let text = if combined.is_empty() {
                if out.status.success() {
                    "ok".to_string()
                } else {
                    format!("exited with {}", out.status)
                }
            } else {
                combined
            };
            json!({ "text": text })
        }
        Err(e) => json!({ "text": format!("failed to run command: {e}") }),
    }
}

fn addressed_send(args: &[String], raw: &str, kickstart: bool, config_path: &Path) -> Value {
    let first = args.first().map(String::as_str).unwrap_or("");
    let target = first.trim_start_matches(['@', '!']).trim();
    if target.is_empty() {
        return json!({ "text": "usage: /comms @ID text (send) or /comms !ID text (kickstart)" });
    }
    let text = if let Some(rest) = raw.strip_prefix(first) {
        rest.trim().to_string()
    } else {
        args[1..].join(" ")
    };
    if text.is_empty() {
        return json!({ "text": format!("nothing to send to {target}: /comms @{target} text") });
    }
    let Ok(config) = crate::config::load_config(config_path) else {
        return json!({ "text": "comms is not configured — run /comms setup" });
    };
    let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
    let Ok(mesh) = crate::identity::mesh_name_now(&config, Some(&me)) else {
        return json!({ "text": "comms cannot resolve this session's mesh name" });
    };
    let payload = json!({
        "to": target,
        "text": text,
        "mode": if kickstart { "kickstart" } else { "message" },
    });
    match send_tool(&config, &Some(me), &mesh, &payload) {
        Ok(text) => json!({ "text": text }),
        Err(e) => json!({ "text": format!("send failed: {e}") }),
    }
}

fn configured(config_path: &Path) -> bool {
    crate::config::load_config(config_path).is_ok()
}

/// The sidecar loop: one JSON request per stdin line, one `{"id", "result"}`
/// reply per line on stdout. `plugin/shutdown` and EOF exit.
pub fn serve(config_path: &Path) -> ! {
    use std::io::{BufRead, Write};
    const LIMIT: usize = 256 * 1024;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        let mut buf: Vec<u8> = Vec::new();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => std::process::exit(1),
        };
        if n == 0 {
            std::process::exit(0);
        }
        if buf.len() > LIMIT {
            std::process::exit(1);
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !req.is_object() {
            continue;
        }
        if req.get("method").and_then(Value::as_str) == Some("plugin/shutdown") {
            std::process::exit(0);
        }
        let id = match req.get("id") {
            Some(Value::Number(_)) => req.get("id").cloned().unwrap(),
            _ => continue,
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = match req.get("params") {
            Some(Value::Object(_)) => req.get("params").cloned().unwrap(),
            _ => json!({}),
        };
        let result = dispatch(method, &params, config_path);
        let row = json!({"id": id, "result": result});
        if writeln!(out, "{row}").is_err() {
            std::process::exit(1);
        }
        if out.flush().is_err() {
            std::process::exit(1);
        }
    }
}
