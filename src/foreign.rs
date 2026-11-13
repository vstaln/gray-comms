use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::session::{Glyph, Line, Status, View};

pub const MAIL_TTL_SECS: u64 = 24 * 60 * 60;
const STALE_SECS: u64 = 30 * 60;
const TAIL_BYTES: u64 = 256 * 1024;
const LINE_CHARS: usize = 88;
const RECAP_CHARS: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Opencode,
}

impl Harness {
    pub fn parse(raw: &str) -> Option<Harness> {
        match raw.trim().to_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::Opencode),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
        }
    }

    pub fn all() -> [Harness; 3] {
        [Harness::Claude, Harness::Codex, Harness::Opencode]
    }
}

#[derive(Debug, Clone)]
pub struct Presence {
    pub harness: Harness,
    pub sid: String,
    pub pid: u32,
    pub cwd: String,
    pub transcript: String,
    pub last_seen_ms: u64,
}

impl Presence {
    pub fn key(&self) -> String {
        key_for(self.harness, &self.sid)
    }
}

pub fn key_for(h: Harness, sid: &str) -> String {
    let short: String = sid.chars().take(8).collect();
    format!("{}-{short}", h.label())
}

pub fn dir(config: &Value) -> PathBuf {
    crate::session::home(config).join("comms/foreign")
}

fn presence_path(config: &Value, key: &str) -> PathBuf {
    dir(config).join(format!("{key}.json"))
}

fn mail_path(config: &Value, key: &str) -> PathBuf {
    dir(config).join(format!("{key}.mail.jsonl"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn save(config: &Value, p: &Presence) -> Result<(), String> {
    let d = dir(config);
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    crate::config::atomic_json(
        &presence_path(config, &p.key()),
        &json!({
            "harness": p.harness.label(),
            "session": p.sid,
            "pid": p.pid,
            "cwd": p.cwd,
            "transcript": p.transcript,
            "last_seen_ms": p.last_seen_ms,
        }),
    )
    .map_err(|e| format!("cannot write presence: {e}"))
}

pub fn list(config: &Value) -> Vec<Presence> {
    let d = dir(config);
    let Ok(rd) = std::fs::read_dir(&d) else {
        return Vec::new();
    };
    let mut out: Vec<Presence> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| {
            let raw = std::fs::read(e.path()).ok()?;
            let v: Value = serde_json::from_slice(&raw).ok()?;
            let harness = Harness::parse(v.get("harness")?.as_str()?)?;
            Some(Presence {
                harness,
                sid: v.get("session")?.as_str()?.to_string(),
                pid: v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
                cwd: v
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                transcript: v
                    .get("transcript")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                last_seen_ms: v.get("last_seen_ms").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .filter_map(|mut p| {
            let age = now_ms().saturating_sub(p.last_seen_ms) / 1000;
            if age > STALE_SECS {
                let _ = std::fs::remove_file(presence_path(config, &p.key()));
                return None;
            }
            p.pid = live_pid(p.harness, &p.cwd)?;
            Some(p)
        })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse((transcript_mtime(p), p.last_seen_ms)));
    let mut seen: Vec<(Harness, u32)> = Vec::new();
    out.retain(|p| {
        if seen.contains(&(p.harness, p.pid)) {
            return false;
        }
        seen.push((p.harness, p.pid));
        true
    });
    out
}

fn transcript_mtime(p: &Presence) -> u64 {
    if p.transcript.is_empty() {
        return 0;
    }
    std::fs::metadata(&p.transcript)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn cmdline_of(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

pub fn live_pid(h: Harness, cwd: &str) -> Option<u32> {
    let rd = std::fs::read_dir("/proc").ok()?;
    let mut best: Option<u32> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let cmd = cmdline_of(pid);
        if !cmd.contains(h.label()) || cmd.contains("hook --harness") {
            continue;
        }
        let Ok(here) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        if here.to_string_lossy() == cwd {
            best = match best {
                Some(b) => Some(b.min(pid)),
                None => Some(pid),
            };
        }
    }
    best
}

pub fn get(config: &Value, key: &str) -> Option<Presence> {
    list(config).into_iter().find(|p| p.key() == key)
}

pub fn find(config: &Value, raw: &str) -> Option<Presence> {
    let raw = raw.trim().to_lowercase();
    if raw.is_empty() {
        return None;
    }
    let all = list(config);
    if let Some(p) = all.iter().find(|p| p.key() == raw) {
        return Some(p.clone());
    }
    let hits: Vec<&Presence> = all
        .iter()
        .filter(|p| p.key().starts_with(&raw) || p.sid.to_lowercase().starts_with(&raw))
        .collect();
    if hits.len() == 1 {
        return Some(hits[0].clone());
    }
    if hits.is_empty() {
        let by_cwd: Vec<&Presence> = all
            .iter()
            .filter(|p| p.cwd.to_lowercase().contains(&raw))
            .collect();
        if by_cwd.len() == 1 {
            return Some(by_cwd[0].clone());
        }
    }
    None
}

pub fn queue(config: &Value, key: &str, text: &str, ttl_secs: u64) -> Result<usize, String> {
    let d = dir(config);
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    let row = json!({
        "text": text,
        "created_ms": now_ms(),
        "ttl_secs": ttl_secs,
    });
    let mut body = serde_json::to_string(&row).map_err(|e| e.to_string())?;
    body.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(mail_path(config, key))
        .map_err(|e| format!("cannot open the mailbox: {e}"))?;
    f.write_all(body.as_bytes())
        .map_err(|e| format!("cannot queue the message: {e}"))?;
    Ok(mail_count(config, key))
}

pub fn mail_count(config: &Value, key: &str) -> usize {
    fresh_mail(config, key, true).len()
}

fn fresh_mail(config: &Value, key: &str, count_only: bool) -> Vec<String> {
    let Ok(raw) = std::fs::read(mail_path(config, key)) else {
        return Vec::new();
    };
    let now = now_ms();
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&raw).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let created = v.get("created_ms").and_then(Value::as_u64).unwrap_or(0);
        let ttl = v
            .get("ttl_secs")
            .and_then(Value::as_u64)
            .unwrap_or(MAIL_TTL_SECS);
        if now.saturating_sub(created) / 1000 > ttl {
            continue;
        }
        if count_only {
            out.push(String::new());
        } else if let Some(t) = v.get("text").and_then(Value::as_str) {
            out.push(t.to_string());
        }
    }
    out
}

pub fn drain(config: &Value, key: &str) -> Vec<String> {
    let msgs = fresh_mail(config, key, false);
    if !msgs.is_empty() {
        let _ = std::fs::write(mail_path(config, key), "");
    }
    msgs
}

#[derive(Debug, Clone)]
enum Piece {
    Text(String),
    Tool(String),
    Result(String),
}

struct Rec {
    role: String,
    clock: String,
    model: String,
    pieces: Vec<Piece>,
}

fn read_tail(path: &Path) -> Vec<Rec> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let size = meta.len();
    let start = size.saturating_sub(TAIL_BYTES);
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = String::new();
    let _ = f.take(TAIL_BYTES).read_to_string(&mut buf);
    let mut out: Vec<Rec> = Vec::new();
    for line in buf.lines() {
        if start > 0 && out.is_empty() && !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let msg = v.get("message").cloned().unwrap_or(Value::Null);
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .or_else(|| v.get("type").and_then(Value::as_str))
            .unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let model = msg
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty() && !m.starts_with('<'))
            .unwrap_or("")
            .to_string();
        let clock = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| t.get(11..16))
            .unwrap_or("")
            .to_string();
        let mut pieces = Vec::new();
        let content = msg.get("content").cloned().unwrap_or(Value::Null);
        match content {
            Value::String(s) => {
                if !s.trim().is_empty() {
                    pieces.push(Piece::Text(s));
                }
            }
            Value::Array(items) => {
                for b in items {
                    let ty = b.get("type").and_then(Value::as_str).unwrap_or("");
                    match ty {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                if !t.trim().is_empty() {
                                    pieces.push(Piece::Text(t.to_string()));
                                }
                            }
                        }
                        "tool_use" => {
                            pieces.push(Piece::Tool(
                                b.get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string(),
                            ));
                        }
                        "tool_result" => {
                            let text = match b.get("content") {
                                Some(Value::String(s)) => s.clone(),
                                Some(Value::Array(items)) => items
                                    .iter()
                                    .filter_map(|i| i.get("text").and_then(Value::as_str))
                                    .collect::<Vec<&str>>()
                                    .join(" "),
                                _ => String::new(),
                            };
                            if !text.trim().is_empty() {
                                pieces.push(Piece::Result(text));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if pieces.is_empty() && model.is_empty() {
            continue;
        }
        out.push(Rec {
            role: role.to_string(),
            clock,
            model,
            pieces,
        });
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<&str>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

pub fn view(p: &Presence, tail: usize) -> View {
    let path = PathBuf::from(&p.transcript);
    let recs = if p.transcript.is_empty() {
        Vec::new()
    } else {
        read_tail(&path)
    };
    let mut want = String::new();
    let mut did = String::new();
    let mut model = String::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut last_role = String::new();
    let mut last_ms = p.last_seen_ms;
    if let Ok(meta) = std::fs::metadata(&path) {
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = modified.elapsed() {
                last_ms = now_ms().saturating_sub(age.as_millis() as u64);
            }
        }
    }
    for r in &recs {
        if !r.model.is_empty() {
            model = r.model.clone();
        }
        last_role = r.role.clone();
        for piece in &r.pieces {
            match piece {
                Piece::Tool(name) => lines.push(Line {
                    clock: r.clock.clone(),
                    glyph: Glyph::Tool,
                    text: name.clone(),
                    tool: name.clone(),
                }),
                Piece::Result(text) => lines.push(Line {
                    clock: r.clock.clone(),
                    glyph: Glyph::Result,
                    text: clip(text, LINE_CHARS),
                    tool: String::new(),
                }),
                Piece::Text(text) => {
                    if r.role == "user" {
                        if text.starts_with('↳') || text.starts_with("[Context update]") {
                            continue;
                        }
                        want = clip(text, RECAP_CHARS);
                        lines.push(Line {
                            clock: r.clock.clone(),
                            glyph: Glyph::Asked,
                            text: clip(text, LINE_CHARS),
                            tool: String::new(),
                        });
                    } else {
                        did = clip(text, RECAP_CHARS);
                        lines.push(Line {
                            clock: r.clock.clone(),
                            glyph: Glyph::Said,
                            text: clip(text, LINE_CHARS),
                            tool: String::new(),
                        });
                    }
                }
            }
        }
    }
    let busy = match last_role.as_str() {
        "user" => true,
        "assistant" => recs
            .last()
            .map(|r| r.pieces.iter().any(|p| matches!(p, Piece::Tool(_))))
            .unwrap_or(false),
        _ => false,
    };
    let idle_secs = now_ms().saturating_sub(last_ms) / 1000;
    let status = match (busy, idle_secs > STALE_SECS) {
        (_, true) => Status::Stale,
        (true, false) => Status::Busy,
        (false, false) => Status::Idle,
    };
    let id = p.key();
    let mut v = View {
        short: id.clone(),
        id,
        pid: (p.pid > 0).then_some(p.pid),
        cwd: p.cwd.clone(),
        model: if model.is_empty() {
            p.harness.label().to_string()
        } else {
            model
        },
        origin: p.harness.label().to_string(),
        status,
        idle_secs,
        want,
        did,
        tail: lines,
        claim: String::new(),
        me: false,
        last_ms: last_ms as i64,
        ctx_used: 0,
        started_ms: 0,
    };
    v.tail = v.tail.split_off(v.tail.len().saturating_sub(tail));
    v
}

pub fn views(config: &Value, tail: usize) -> Vec<View> {
    list(config).iter().map(|p| view(p, tail)).collect()
}
