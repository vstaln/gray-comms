//! Wire-level test: a real daemon + two clients exchanging DMs and room
//! traffic, matching the task's "two local clients exchange DMs" gate.
use gray_comms::client::Client;
use gray_comms::daemon;
use gray_comms::store::Store;
use serde_json::{json, Value};
use std::net::SocketAddr;

/// Boot a daemon on an ephemeral port in its own runtime thread.
fn spawn_daemon() -> SocketAddr {
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
            let config = json!({"history_limit": 50});
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
