use serde_json::{Map, Value};
use std::path::PathBuf;

pub fn path(config: &Value) -> PathBuf {
    crate::session::home(config)
        .join("comms")
        .join("nicknames.json")
}

fn read(config: &Value) -> Map<String, Value> {
    std::fs::read_to_string(path(config))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn all(config: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = read(config)
        .into_iter()
        .filter_map(|(k, v)| {
            let nick = v.as_str()?.trim().to_string();
            (!nick.is_empty()).then_some((k, nick))
        })
        .collect();
    out.sort();
    out
}

pub fn get(config: &Value, id: &str) -> Option<String> {
    let m = read(config);
    m.get(id)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn set(config: &Value, id: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return clear(config, id);
    }
    let mut m = read(config);
    m.insert(id.to_string(), Value::String(name.to_string()));
    crate::config::atomic_json(&path(config), &Value::Object(m))
}

pub fn clear(config: &Value, id: &str) -> Result<(), String> {
    let mut m = read(config);
    if m.remove(id).is_none() {
        return Ok(());
    }
    crate::config::atomic_json(&path(config), &Value::Object(m))
}

pub fn resolve(config: &Value, raw: &str) -> Option<String> {
    let want = raw.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let all = all(config);
    if let Some((id, _)) = all.iter().find(|(_, n)| n.to_lowercase() == want) {
        return Some(id.clone());
    }
    let mut hits = all
        .iter()
        .filter(|(_, n)| n.to_lowercase().starts_with(&want));
    let first = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(first.0.clone())
}

pub fn label(config: &Value, id: &str) -> String {
    get(config, id).unwrap_or_default()
}
