use serde_json::json;
use std::sync::OnceLock;
use std::time::Duration;

use crate::session::Status;

const TICK_SECS: u64 = 10;
const REJOIN_TICKS: u32 = 6;
const RETRY_SECS: u64 = 15;

static STARTED: OnceLock<()> = OnceLock::new();

fn beat(config: &serde_json::Value, session_id: &str) -> Status {
    crate::session::status_of(config, session_id)
}

pub fn start(config: serde_json::Value, session_id: String, mesh: String) {
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(move || run(config, session_id, mesh));
}

fn run(config: serde_json::Value, session_id: String, mesh: String) {
    let mut last: Option<Status> = None;
    loop {
        if let Ok(mut c) = crate::client::Client::connect_as(&config, &mesh) {
            let mut tick: u32 = 0;
            loop {
                let now = beat(&config, &session_id);
                let ok = if last != Some(now) || tick.is_multiple_of(REJOIN_TICKS) {
                    c.call("status", json!({"status": now.label()})).is_ok()
                } else {
                    c.call("ping", json!({})).is_ok()
                };
                if !ok {
                    break;
                }
                last = Some(now);
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_secs(TICK_SECS));
            }
        }
        last = None;
        std::thread::sleep(Duration::from_secs(RETRY_SECS));
    }
}
