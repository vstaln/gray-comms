use serde_json::{json, Value};
use std::path::PathBuf;

pub const TTL_SECS: u64 = 1800;
const MAX_CHARS: usize = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Set,
    Clear,
}

impl Action {
    pub fn parse(raw: Option<&str>) -> Result<Action, String> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("set") => Ok(Action::Set),
            Some("clear") => Ok(Action::Clear),
            Some(other) => Err(format!(
                "\"action\" must be \"set\" or \"clear\", not \"{other}\""
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Claim {
    pub session: String,
    pub text: String,
    pub set_ms: u64,
}

impl Claim {
    pub fn age_secs(&self) -> u64 {
        let now = now_ms();
        now.saturating_sub(self.set_ms) / 1000
    }

    pub fn expired(&self) -> bool {
        self.age_secs() > TTL_SECS
    }

    pub fn render(&self) -> String {
        let text = clip(&self.text, MAX_CHARS);
        let mins = self.age_secs() / 60;
        if mins == 0 {
            text
        } else {
            format!("{text} ({mins}m)")
        }
    }

    pub fn remaining_secs(&self) -> u64 {
        TTL_SECS.saturating_sub(self.age_secs())
    }
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<&str>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn dir(config: &Value) -> PathBuf {
    crate::session::home(config).join("comms/claims")
}

fn path(config: &Value, session: &str) -> PathBuf {
    dir(config).join(format!("{session}.json"))
}

pub fn set(config: &Value, session: &str, text: &str) -> Result<Claim, String> {
    let flat = text.split_whitespace().collect::<Vec<&str>>().join(" ");
    if flat.is_empty() {
        return Err("text is empty: say what you are working on".to_string());
    }
    if session.trim().is_empty() {
        return Err("no session id to attach the claim to".to_string());
    }
    let claim = Claim {
        session: session.to_string(),
        text: flat,
        set_ms: now_ms(),
    };
    let d = dir(config);
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    crate::config::atomic_json(
        &path(config, session),
        &json!({
            "session": claim.session,
            "text": claim.text,
            "set_ms": claim.set_ms,
            "ttl_secs": TTL_SECS,
        }),
    )
    .map_err(|e| format!("cannot write the claim: {e}"))?;
    Ok(claim)
}

pub fn clear(config: &Value, session: &str) -> bool {
    std::fs::remove_file(path(config, session)).is_ok()
}

pub fn get(config: &Value, session: &str) -> Option<Claim> {
    let raw = std::fs::read(path(config, session)).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let claim = Claim {
        session: v
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or(session)
            .to_string(),
        text: v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        set_ms: v.get("set_ms").and_then(Value::as_u64).unwrap_or(0),
    };
    if claim.text.is_empty() || claim.expired() {
        return None;
    }
    Some(claim)
}

pub fn label(config: &Value, session: &str) -> String {
    get(config, session).map(|c| c.render()).unwrap_or_default()
}
