use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Spawn {
    pub parent: String,
    pub name: String,
}

pub fn dir(config: &Value) -> PathBuf {
    crate::session::home(config).join("subagents").join("runs")
}

fn parse(path: &Path) -> Option<(String, Spawn)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let child = v.get("child_session").and_then(Value::as_str)?.trim();
    let parent = v.get("session_id").and_then(Value::as_str)?.trim();
    if child.is_empty() || parent.is_empty() || child == parent {
        return None;
    }
    Some((
        child.to_string(),
        Spawn {
            parent: parent.to_string(),
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
        },
    ))
}

fn run_files(dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(i64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "json" {
                return None;
            }
            let ms = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default();
            Some((ms, path))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

pub fn load(config: &Value) -> HashMap<String, Spawn> {
    let mut map = HashMap::new();
    for (_, path) in run_files(&dir(config)) {
        if let Some((child, spawn)) = parse(&path) {
            map.insert(child, spawn);
        }
    }
    map
}

pub fn refresh(config: &Value, map: &mut HashMap<String, Spawn>, newest: usize) {
    for (_, path) in run_files(&dir(config)).into_iter().take(newest) {
        if let Some((child, spawn)) = parse(&path) {
            map.insert(child, spawn);
        }
    }
}
