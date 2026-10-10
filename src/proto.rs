//! Wire protocol: newline-delimited JSON between clients and the daemon.
//!
//! Requests carry a numeric `id` and a `method`; replies echo the id with
//! `result` or `error`. Live listeners also receive `{"method":"notify",
//! "event":{...}}` pushes. Event shapes mirror agent-comms' DeliveryEvent
//! (`room_message`, `dm`, `member_joined`, `member_left`) so a bridge to the
//! real mesh stays a thin rename.
use serde_json::{json, Value};

/// Largest single line a peer may send (256 KiB, same as the sidecar wire).
pub const MAX_LINE: usize = 256 * 1024;
/// Largest message body accepted by `say`/`dm`.
pub const MAX_TEXT: usize = 16 * 1024;
/// Default and ceiling for history pulls.
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;

pub fn encode(msg: &Value) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

pub fn ok(id: &Value, result: Value) -> Value {
    json!({"id": id, "result": result})
}

pub fn err(id: &Value, code: &str, message: impl std::fmt::Display) -> Value {
    json!({"id": id, "error": {"code": code, "message": message.to_string()}})
}

pub fn notify(event: Value) -> Value {
    json!({"method": "notify", "event": event})
}

/// Agent names: 1-64 chars of `[A-Za-z0-9_.-]` — they are also DM targets
/// and room-member ids, so nothing whitespace-bearing gets through.
pub fn agent_name_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Room names are lowercase slugs; a leading `#` is accepted and stripped.
pub fn room_normalize(raw: &str) -> Option<String> {
    let s = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if s.is_empty()
        || s.len() > 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    Some(s.to_string())
}

/// Presence states, mirroring agent-comms' AgentStatus minus `offline`
/// (offline is derived: no live connection).
pub fn status_valid(s: &str) -> bool {
    matches!(s, "active" | "idle" | "busy")
}

/// The `params` object of a decoded request (`{}` when absent).
pub fn params_of(req: &Value) -> Value {
    match req.get("params") {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    }
}

pub fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

pub fn limit_param(params: &Value) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT)
}
