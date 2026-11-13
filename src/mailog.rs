use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

pub struct Entry {
    pub at: i64,
    pub from: String,
    pub to: String,
    pub mode: String,
    pub state: String,
    pub text: String,
}

pub fn path(config: &Value) -> PathBuf {
    crate::session::home(config)
        .join("comms")
        .join("mail.ndjson")
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub fn append(config: &Value, from: &str, to: &str, mode: &str, state: &str, text: &str) {
    let path = path(config);
    if let Some(parent) = path.parent() {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        let _ = b.create(parent);
    }
    let row = json!({
        "at": now_ms(),
        "from": from,
        "to": to,
        "mode": mode,
        "state": state,
        "text": text,
    });
    let mut line = row.to_string();
    line.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn recent(config: &Value, limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path(config)) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| Entry {
            at: v.get("at").and_then(Value::as_i64).unwrap_or_default(),
            from: v
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            to: v
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            mode: v
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            state: v
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .collect();
    out.reverse();
    out.truncate(limit);
    out.reverse();
    out
}
