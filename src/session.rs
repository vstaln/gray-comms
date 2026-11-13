use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const TAIL_MIN: u64 = 128 * 1024;
const TAIL_MAX: u64 = 8 * 1024 * 1024;
const RECAP_CHARS: usize = 90;
const LINE_CHARS: usize = 88;
const MIN_SHORT: usize = 5;
pub const STALE_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    Stale,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Busy => "busy",
            Status::Idle | Status::Stale => "idle",
        }
    }

    pub fn fresh(self) -> bool {
        match self {
            Status::Busy | Status::Idle => true,
            Status::Stale => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Tool,
    Result,
    Said,
    Asked,
}

impl Glyph {
    pub fn ch(self) -> char {
        match self {
            Glyph::Tool => '⚙',
            Glyph::Result => '⟲',
            Glyph::Said => '‹',
            Glyph::Asked => '›',
        }
    }
}

#[derive(Debug, Clone)]
pub struct Line {
    pub clock: String,
    pub glyph: Glyph,
    pub text: String,
    pub tool: String,
}

#[derive(Debug, Clone)]
pub struct View {
    pub id: String,
    pub short: String,
    pub pid: Option<u32>,
    pub cwd: String,
    pub model: String,
    pub origin: String,
    pub status: Status,
    pub idle_secs: u64,
    pub want: String,
    pub did: String,
    pub tail: Vec<Line>,
    pub me: bool,
    pub last_ms: i64,
}

impl View {
    pub fn short_model(&self) -> String {
        let m = self
            .model
            .rsplit('/')
            .next()
            .unwrap_or(&self.model)
            .to_string();
        if m.is_empty() {
            "?".to_string()
        } else {
            m
        }
    }

    pub fn recap(&self) -> String {
        match (self.want.is_empty(), self.did.is_empty()) {
            (false, false) => format!("{} ⇒ {}", self.want, self.did),
            (false, true) => self.want.clone(),
            (true, false) => self.did.clone(),
            (true, true) => String::new(),
        }
    }

    pub fn cwd_short(&self) -> String {
        tilde(&self.cwd)
    }
}

enum Block {
    Text(String),
    Tool { name: String, input: String },
    Result(String),
}

struct Entry {
    at: i64,
    role: String,
    injected: bool,
    blocks: Vec<Block>,
}

pub fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn home(config: &Value) -> PathBuf {
    if let Some(p) = config
        .get("gray_home")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("GRAY_USER_HOME").or_else(|| std::env::var_os("GRAY_HOME")) {
        return PathBuf::from(p);
    }
    user_home().join(".gray")
}

pub fn root(config: &Value) -> PathBuf {
    home(config).join("sessions")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub fn inbox_dir(config: &Value, id: &str) -> PathBuf {
    home(config).join("cron").join("inbox").join(id)
}

pub fn pending_mail(config: &Value, id: &str) -> usize {
    let Ok(rd) = std::fs::read_dir(inbox_dir(config, id)) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".json") && !name.starts_with('.')
        })
        .count()
}

pub fn pending_inboxes(config: &Value) -> Vec<(String, usize)> {
    let root = home(config).join("cron").join("inbox");
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, usize)> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            if !e.path().is_dir() {
                return None;
            }
            let n = pending_mail(config, &id);
            (n > 0).then_some((id, n))
        })
        .collect();
    out.sort();
    out
}

pub fn tilde(path: &str) -> String {
    let home = user_home();
    let home = home.to_string_lossy();
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(&format!("{home}/")) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

fn clip(s: &str, n: usize) -> String {
    let flat: String = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', '-', '*', '>', ' '])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= n {
        return flat;
    }
    let mut out: String = flat.chars().take(n).collect();
    out.push('…');
    out
}

fn clock(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at).single() {
        Some(t) => t.format("%H:%M").to_string(),
        None => "--:--".to_string(),
    }
}

fn lock_held(path: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

fn holder_pid(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    v.get("pid").and_then(Value::as_u64).map(|p| p as u32)
}

fn header(path: &Path) -> Option<(String, String, String)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let line = text.lines().next()?;
    let v: Value = serde_json::from_str(line).ok()?;
    Some((
        v.get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        v.get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        v.get("origin")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    ))
}

fn tool_input(name: &str, v: &Value) -> String {
    if v.is_null() || v.as_object().is_some_and(|o| o.is_empty()) {
        return String::new();
    }
    for key in [
        "command",
        "cmd",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
        "text",
    ] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return clip(s, LINE_CHARS);
            }
        }
    }
    clip(&v.to_string(), LINE_CHARS)
        .trim_start_matches(name)
        .to_string()
}

fn result_text(v: &Value) -> String {
    if let Some(s) = v.get("text").and_then(Value::as_str) {
        return clip(s, LINE_CHARS);
    }
    if let Some(a) = v.get("content").and_then(Value::as_array) {
        for b in a {
            if let Some(s) = b.get("text").and_then(Value::as_str) {
                if !s.trim().is_empty() {
                    return clip(s, LINE_CHARS);
                }
            }
        }
    }
    if let Some(s) = v.as_str() {
        return clip(s, LINE_CHARS);
    }
    clip(&v.to_string(), LINE_CHARS)
}

fn parse_entry(v: &Value) -> Option<Entry> {
    let m = v.get("message")?;
    let role = m.get("role").and_then(Value::as_str)?.to_string();
    let injected = m.get("injected").and_then(Value::as_bool).unwrap_or(false);
    let at = v
        .get("timestamp")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let mut blocks = Vec::new();
    for b in m
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !t.trim().is_empty() {
                        blocks.push(Block::Text(t.to_string()));
                    }
                }
            }
            "tool_use" => blocks.push(Block::Tool {
                name: b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
                input: tool_input(
                    b.get("name").and_then(Value::as_str).unwrap_or("?"),
                    b.get("input").unwrap_or(&Value::Null),
                ),
            }),
            "tool_result" => blocks.push(Block::Result(result_text(
                b.get("content").unwrap_or(&Value::Null),
            ))),
            _ => {}
        }
    }
    Some(Entry {
        at,
        role,
        injected,
        blocks,
    })
}

fn read_window(path: &Path, len: u64, window: u64) -> Vec<Entry> {
    let start = len.saturating_sub(window);
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = String::new();
    if f.take(len - start).read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = buf.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| parse_entry(&v))
        .collect()
}

fn entries(path: &Path) -> Vec<Entry> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let len = meta.len();
    let mut window = TAIL_MIN;
    loop {
        let got = read_window(path, len, window);
        if got.len() >= 4 || window >= len || window >= TAIL_MAX {
            return got;
        }
        window *= 2;
    }
}

fn shorts(ids: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for id in ids {
        let chars: Vec<char> = id.chars().collect();
        let mut chosen = id.clone();
        for n in MIN_SHORT..=chars.len() {
            let prefix: String = chars[..n].iter().collect();
            let hits = ids.iter().filter(|o| o.starts_with(&prefix)).count();
            if hits == 1 {
                chosen = prefix;
                break;
            }
        }
        out.insert(id.clone(), chosen);
    }
    out
}

fn build(id: String, path: &Path, pid: Option<u32>) -> Option<View> {
    let (cwd, model, origin) = header(path)?;
    let list = entries(path);
    let last_ms = list.last().map(|e| e.at).unwrap_or_default();
    let mut want = String::new();
    let mut did = String::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut busy = false;
    for e in list.iter().filter(|e| !e.injected) {
        let mut has_tool = false;
        for b in &e.blocks {
            match b {
                Block::Tool { name, input } => {
                    has_tool = true;
                    lines.push(Line {
                        clock: clock(e.at),
                        glyph: Glyph::Tool,
                        text: if input.is_empty() {
                            name.clone()
                        } else {
                            format!("{name}: {input}")
                        },
                        tool: name.clone(),
                    });
                }
                Block::Result(t) => lines.push(Line {
                    clock: clock(e.at),
                    glyph: Glyph::Result,
                    text: t.clone(),
                    tool: String::new(),
                }),
                Block::Text(t) => {
                    if e.role == "user" {
                        let skip = t.starts_with("You are a subagent")
                            || t.starts_with("Subagent finished")
                            || t.starts_with("Directive —");
                        if !skip && !t.trim().is_empty() {
                            want = clip(t, RECAP_CHARS);
                            lines.push(Line {
                                clock: clock(e.at),
                                glyph: Glyph::Asked,
                                text: clip(t, LINE_CHARS),
                                tool: String::new(),
                            });
                        }
                    } else if e.role == "assistant" {
                        did = clip(t, RECAP_CHARS);
                        lines.push(Line {
                            clock: clock(e.at),
                            glyph: Glyph::Said,
                            text: clip(t, LINE_CHARS),
                            tool: String::new(),
                        });
                    }
                }
            }
        }
        busy = match e.role.as_str() {
            "user" => true,
            "assistant" => has_tool,
            _ => busy,
        };
    }
    let idle_secs = if last_ms > 0 {
        ((now_ms() - last_ms).max(0) / 1000) as u64
    } else {
        0
    };
    let status = match (busy, idle_secs > STALE_SECS) {
        (_, true) => Status::Stale,
        (true, false) => Status::Busy,
        (false, false) => Status::Idle,
    };
    Some(View {
        short: id.clone(),
        id,
        pid,
        cwd,
        model,
        origin,
        status,
        idle_secs,
        want,
        did,
        tail: lines,
        me: false,
        last_ms,
    })
}

pub fn live_ids(config: &Value) -> Vec<(String, Option<u32>)> {
    let dir = root(config);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "open" {
                return None;
            }
            if !lock_held(&path) {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let pid = holder_pid(&path);
            Some((id, pid))
        })
        .collect()
}

pub fn collect(config: &Value, me: &str, tail: usize, dir: Option<&str>) -> Vec<View> {
    let live = live_ids(config);
    let ids: Vec<String> = live.iter().map(|(id, _)| id.clone()).collect();
    let short = shorts(&ids);
    let dir = dir.map(str::to_lowercase);
    let mut views: Vec<View> = Vec::new();
    for (id, pid) in live {
        let path = root(config).join(format!("{id}.jsonl"));
        let Some(mut v) = build(id.clone(), &path, pid) else {
            continue;
        };
        if let Some(short) = short.get(&id) {
            v.short = short.clone();
        }
        v.me = id == me;
        if let Some(d) = &dir {
            let hay = format!("{} {}", v.cwd.to_lowercase(), tilde(&v.cwd).to_lowercase());
            if !hay.contains(d.as_str()) {
                continue;
            }
        }
        v.tail = v.tail.split_off(v.tail.len().saturating_sub(tail));
        views.push(v);
    }
    views.sort_by(|a, b| b.last_ms.cmp(&a.last_ms).then(a.id.cmp(&b.id)));
    views
}

pub fn find<'a>(views: &'a [View], raw: &str) -> Result<&'a View, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty target".to_string());
    }
    if let Ok(pid) = raw.parse::<u32>() {
        if let Some(v) = views.iter().find(|v| v.pid == Some(pid)) {
            return Ok(v);
        }
    }
    if let Some(v) = views.iter().find(|v| v.id == raw) {
        return Ok(v);
    }
    let by_id: Vec<&View> = views.iter().filter(|v| v.id.starts_with(raw)).collect();
    if by_id.len() == 1 {
        return Ok(by_id[0]);
    }
    if by_id.is_empty() {
        let lower = raw.to_lowercase();
        let by_cwd: Vec<&View> = views
            .iter()
            .filter(|v| {
                v.cwd.to_lowercase().contains(&lower)
                    || tilde(&v.cwd).to_lowercase().contains(&lower)
            })
            .collect();
        match by_cwd.len() {
            1 => return Ok(by_cwd[0]),
            0 => {
                return Err(format!(
                    "no live session matches \"{raw}\" (targets: id prefix, pid, cwd substring)"
                ))
            }
            _ => {
                let list: Vec<String> = by_cwd
                    .iter()
                    .map(|v| format!("{} ({})", v.short, tilde(&v.cwd)))
                    .collect();
                return Err(format!("ambiguous target \"{raw}\": {}", list.join(", ")));
            }
        }
    }
    let list: Vec<String> = by_id.iter().map(|v| v.id.clone()).collect();
    Err(format!("ambiguous target \"{raw}\": {}", list.join(", ")))
}

pub fn find_known(config: &Value, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("no session matches an empty target".to_string());
    }
    let Ok(rd) = std::fs::read_dir(root(config)) else {
        return Err(format!("no session matches \"{raw}\""));
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".jsonl").map(str::to_string)
        })
        .filter(|id| id.starts_with(raw))
        .collect();
    ids.sort();
    ids.dedup();
    match ids.len() {
        1 => Ok(ids.remove(0)),
        0 => Err(format!("no session matches \"{raw}\"")),
        _ => Err(format!(
            "\"{raw}\" matches {} sessions: {}",
            ids.len(),
            ids.iter()
                .take(4)
                .cloned()
                .collect::<Vec<String>>()
                .join(", ")
        )),
    }
}

pub fn short_for(config: &Value, id: &str) -> String {
    let mut ids: Vec<String> = live_ids(config).into_iter().map(|(i, _)| i).collect();
    if !ids.iter().any(|i| i == id) {
        ids.push(id.to_string());
    }
    shorts(&ids)
        .remove(id)
        .unwrap_or_else(|| id.chars().take(MIN_SHORT).collect())
}

pub fn status_of(config: &Value, id: &str) -> Status {
    let path = root(config).join(format!("{id}.jsonl"));
    match build(id.to_string(), &path, None) {
        Some(v) => v.status,
        None => Status::Idle,
    }
}
