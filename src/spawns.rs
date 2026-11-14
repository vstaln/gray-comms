use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
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

#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub name: String,
    pub child: String,
    pub child_pid: Option<u32>,
    pub status: String,
}

fn job_path(config: &Value, id: &str) -> PathBuf {
    dir(config).join(format!("{id}.json"))
}

fn field(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\": \"");
    let start = text.find(&pat)? + pat.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn head_name(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 512];
    let n = f.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    field(&text, "name").filter(|s| !s.is_empty())
}

fn parse_run(path: &Path) -> Option<Run> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Run {
        id: v.get("run_id").and_then(Value::as_str)?.to_string(),
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        child: v
            .get("child_session")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        child_pid: v.get("child_pid").and_then(Value::as_u64).map(|p| p as u32),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn proc_line(pid: u32, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = text.lines().find(|l| l.starts_with(key))?;
    Some(line.split_whitespace().nth(1)?.to_string())
}

fn supervising(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let at = parts.iter().position(|p| p == "--supervise")?;
    parts.get(at + 1).filter(|s| !s.is_empty()).cloned()
}

pub fn name_for_pid(config: &Value, pid: u32) -> Option<String> {
    let own = supervising(pid);
    let rid = own.or_else(|| {
        let parent: u32 = proc_line(pid, "PPid:")?.parse().ok()?;
        supervising(parent)
    })?;
    head_name(&job_path(config, &rid))
}

fn listing(config: &Value) -> Vec<(String, PathBuf)> {
    run_files(&dir(config))
        .into_iter()
        .filter_map(|(_, path)| head_name(&path).map(|n| (n, path)))
        .collect()
}

pub fn candidate_names(config: &Value) -> Vec<String> {
    listing(config).into_iter().map(|(n, _)| n).collect()
}

pub fn find_run(config: &Value, raw: &str) -> Option<Run> {
    let want = raw.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let rows = listing(config);
    let exact: Vec<&PathBuf> = rows
        .iter()
        .filter(|(n, _)| n.to_lowercase() == want)
        .map(|(_, p)| p)
        .collect();
    if exact.len() == 1 {
        return parse_run(exact[0]);
    }
    let near: Vec<&PathBuf> = rows
        .iter()
        .filter(|(n, _)| n.to_lowercase().starts_with(&want))
        .map(|(_, p)| p)
        .collect();
    if near.len() == 1 {
        return parse_run(near[0]);
    }
    None
}
