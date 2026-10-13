//! Wire-level test: a real daemon + two clients exchanging DMs and room
//! traffic, matching the task's "two local clients exchange DMs" gate.
use gray_comms::client::Client;
use gray_comms::daemon;
use gray_comms::store::Store;
use serde_json::{json, Value};
use std::net::SocketAddr;

/// Boot a daemon on an ephemeral port in its own runtime thread.
fn spawn_daemon() -> SocketAddr {
    spawn_daemon_with(json!({"history_limit": 50}))
}

/// Same, with an explicit daemon config (routing, limits).
fn spawn_daemon_with(config: Value) -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
            let _ = daemon::serve_on(listener, &config, Store::memory().expect("store")).await;
        });
    });
    // Wait until the listener task is accepting.
    for _ in 0..100 {
        if std::net::TcpStream::connect(addr).is_ok() {
            return addr;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("daemon never came up");
}

fn config(name: &str) -> Value {
    json!({"name": name})
}

#[test]
fn two_clients_exchange_dms() {
    let addr = spawn_daemon().to_string();

    let mut alice = Client::connect_addr(&addr, &config("alice")).expect("alice joins");
    // Bob has never joined: the DM is refused, not silently dropped.
    assert!(alice.call("dm", json!({"to": "bob", "text": "early"})).is_err());

    let mut bob = Client::connect_addr(&addr, &config("bob")).expect("bob joins");
    // Neither side listens: the DM queues durably.
    let res = alice
        .call("dm", json!({"to": "bob", "text": "hello bob"}))
        .expect("dm ok");
    assert_eq!(res.get("delivery").and_then(Value::as_str), Some("queued"));

    // Bob's unread peek drains the queue and marks it delivered.
    let res = bob
        .call("peek", json!({"unread": true}))
        .expect("peek ok");
    let msgs = res.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].get("content").and_then(Value::as_str),
        Some("hello bob")
    );
    assert_eq!(msgs[0].get("from").and_then(Value::as_str), Some("alice"));
    // Drained: a second unread peek is empty.
    let res = bob.call("peek", json!({"unread": true})).expect("peek2");
    assert_eq!(
        res.get("messages").and_then(Value::as_array).unwrap().len(),
        0
    );

    // Live push: bob listens; alice's next DM arrives as a notify event.
    bob.listen().expect("bob listens");
    let res = alice
        .call("dm", json!({"to": "bob", "text": "live ping"}))
        .expect("dm2 ok");
    assert_eq!(
        res.get("delivery").and_then(Value::as_str),
        Some("delivered")
    );
    let ev = bob.next_event().expect("push event");
    assert_eq!(
        ev.pointer("/event/type").and_then(Value::as_str),
        Some("dm")
    );
    assert_eq!(
        ev.pointer("/event/message/content").and_then(Value::as_str),
        Some("live ping")
    );
}

#[test]
fn rooms_and_history() {
    let addr = spawn_daemon().to_string();
    let mut a = Client::connect_addr(&addr, &config("ann")).expect("ann");
    let mut b = Client::connect_addr(&addr, &config("bea")).expect("bea");

    a.call("join_room", json!({"room": "#dev"})).expect("a joins dev");
    // Saying before joining is refused.
    assert!(b.call("say", json!({"room": "dev", "text": "nope"})).is_err());
    b.call("join_room", json!({"room": "dev"})).expect("b joins dev");
    b.listen().expect("b listens");

    a.call("say", json!({"room": "dev", "text": "room hello"}))
        .expect("say");
    let ev = b.next_event().expect("room push");
    assert_eq!(
        ev.pointer("/event/type").and_then(Value::as_str),
        Some("room_message")
    );
    assert_eq!(
        ev.pointer("/event/message/content").and_then(Value::as_str),
        Some("room hello")
    );

    // History pull is bounded and ordered oldest-first.
    a.call("say", json!({"room": "dev", "text": "second"})).expect("say2");
    let _ = b.next_event();
    let res = b
        .call("peek", json!({"room": "dev", "limit": 10}))
        .expect("peek room");
    let msgs = res.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        msgs[0].get("content").and_then(Value::as_str),
        Some("room hello")
    );
    assert_eq!(
        msgs[1].get("content").and_then(Value::as_str),
        Some("second")
    );

    // Presence: bea listens (active), ann connected-but-not-listening (offline).
    let res = a.call("who", json!({})).expect("who");
    let agents = res.get("agents").and_then(Value::as_array).unwrap();
    let status_of = |n: &str| {
        agents
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(n))
            .and_then(|a| a.get("status").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string()
    };
    assert_eq!(status_of("bea"), "active");
    assert_eq!(status_of("ann"), "offline");
}

#[test]
fn name_and_room_validation() {
    use gray_comms::proto::*;
    assert!(agent_name_valid("gray-main_2.x"));
    assert!(!agent_name_valid(""));
    assert!(!agent_name_valid("has space"));
    assert!(!agent_name_valid(&"x".repeat(65)));
    assert_eq!(room_normalize("#Dev"), None); // uppercase rejected
    assert_eq!(room_normalize("#dev").as_deref(), Some("dev"));
    assert_eq!(room_normalize("dev").as_deref(), Some("dev"));
    assert!(room_normalize("bad room").is_none());
    assert!(status_valid("busy"));
    assert!(!status_valid("offline"));
}

/// Routing fail-open at the wire: a room in `auto` mode whose model call
/// cannot succeed (dead endpoint) still pushes to every live member —
/// exactly what the room did before routing existed.
#[test]
fn auto_room_fails_open() {
    let addr = spawn_daemon_with(json!({
        "history_limit": 50,
        "routing": {
            "enabled": true,
            "timeout_ms": 500,
            "rooms": {"dev": "auto"},
            // Refused instantly — curl exits before the budget.
            "model": {"base_url": "http://127.0.0.1:1", "model": "fake"}
        }
    }))
    .to_string();

    let mut ann = Client::connect_addr(&addr, &config("ann")).expect("ann");
    let mut bea = Client::connect_addr(&addr, &config("bea")).expect("bea");
    let mut carol = Client::connect_addr(&addr, &config("carol")).expect("carol");
    for c in [&mut ann, &mut bea, &mut carol] {
        c.call("join_room", json!({"room": "dev"})).expect("join dev");
    }
    bea.listen().expect("bea listens");
    carol.listen().expect("carol listens");

    ann.call("say", json!({"room": "dev", "text": "anyone around?"}))
        .expect("say");
    for c in [&mut bea, &mut carol] {
        let ev = c.next_event().expect("push");
        assert_eq!(
            ev.pointer("/event/type").and_then(Value::as_str),
            Some("room_message")
        );
    }
}

/// Routing `lead` mode narrows the live push to the room's lead.
#[test]
fn lead_room_pushes_only_lead() {
    let addr = spawn_daemon_with(json!({
        "history_limit": 50,
        "routing": {
            "enabled": true,
            "rooms": {"dev": "lead"},
            "leads": {"dev": "bea"}
        }
    }))
    .to_string();

    let mut ann = Client::connect_addr(&addr, &config("ann")).expect("ann");
    let mut bea = Client::connect_addr(&addr, &config("bea")).expect("bea");
    let mut carol = Client::connect_addr(&addr, &config("carol")).expect("carol");
    for c in [&mut ann, &mut bea, &mut carol] {
        c.call("join_room", json!({"room": "dev"})).expect("join dev");
    }
    bea.listen().expect("bea listens");
    carol.listen().expect("carol listens");

    ann.call("say", json!({"room": "dev", "text": "ping"})).expect("say");
    let ev = bea.next_event().expect("bea gets the push");
    assert_eq!(
        ev.pointer("/event/message/content").and_then(Value::as_str),
        Some("ping")
    );
    // Carol was not picked: no room_message push for her. A `who` call
    // drains any interleaved notify into `events`; give the push a moment
    // to have arrived (or not).
    std::thread::sleep(std::time::Duration::from_millis(300));
    carol.call("who", json!({})).expect("who");
    assert!(carol.events.is_empty(), "carol got pushed: {:?}", carol.events);
}

/// The daemon pushes heartbeat frames to live listeners; on the wire they
/// are `notify` events of type `ping` and they keep coming.
#[test]
fn heartbeat_frames_on_the_wire() {
    use gray_comms::proto;
    use std::io::{BufRead, BufReader, Write};
    use std::time::Duration;

    let addr = spawn_daemon_with(json!({"keepalive_ms": 150})).to_string();
    let sock = std::net::TcpStream::connect(addr).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut w = sock.try_clone().unwrap();
    w.write_all(
        proto::encode(&json!({
            "id": 1, "method": "join",
            "params": {"name": "hb", "listen": true}
        }))
        .as_bytes(),
    )
    .unwrap();
    let mut r = BufReader::new(sock);
    let mut pings = 0;
    for _ in 0..5 {
        let mut line = String::new();
        r.read_line(&mut line).expect("read");
        let v: Value = serde_json::from_str(line.trim()).expect("json");
        if v.get("id").is_some() {
            continue; // join reply
        }
        assert_eq!(v.get("method").and_then(Value::as_str), Some("notify"));
        assert_eq!(
            v.pointer("/event/type").and_then(Value::as_str),
            Some("ping")
        );
        pings += 1;
        if pings == 2 {
            return;
        }
    }
    panic!("expected two heartbeat frames, got {pings}");
}

/// An idle listener used to die at the client's 10s IO timeout. With
/// heartbeats, `next_event` stays blocked and a later push still lands.
#[test]
fn listen_survives_idle() {
    use std::sync::mpsc;
    use std::time::Duration;

    let addr = spawn_daemon_with(json!({"keepalive_ms": 200})).to_string();
    let mut alice = Client::connect_addr(&addr, &config("alice")).expect("alice");
    let mut bob = Client::connect_addr(&addr, &config("bob")).expect("bob");
    bob.listen().expect("bob listens");

    // Heartbeats are wire activity, not events: they must not collect in
    // `events` while a request/response call reads through them.
    std::thread::sleep(Duration::from_millis(500));
    bob.call("ping", json!({})).expect("ping");
    assert!(
        !bob.events.iter().any(gray_comms::proto::is_heartbeat),
        "heartbeats leaked into events: {:?}",
        bob.events
    );

    // Block in next_event across the full client IO timeout.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        let r = bob.next_event();
        let dead = r.is_err();
        if tx.send(r).is_err() || dead {
            return;
        }
    });
    std::thread::sleep(Duration::from_secs(11));
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "listener errored during idle"
    );

    alice
        .call("dm", json!({"to": "bob", "text": "still there?"}))
        .expect("dm");
    let ev = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("push after idle")
        .expect("push ok");
    assert_eq!(
        ev.pointer("/event/type").and_then(Value::as_str),
        Some("dm")
    );
    // Dropping rx lets the reader thread exit on its next send.
}
