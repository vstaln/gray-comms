use serde_json::Value;
use std::path::PathBuf;

use crate::session::{short_for, tilde, View};

pub const MAX_CHARS: usize = 1200;
pub const MAX_SESSIONS: usize = 6;

fn entry_head(v: &View) -> String {
    format!(
        "• {} {} · {} · {}",
        v.short,
        v.status.label(),
        v.short_model(),
        tilde(&v.cwd)
    )
}

fn entry_recap(v: &View) -> Option<String> {
    let r = v.recap();
    if r.is_empty() {
        None
    } else {
        Some(format!("  {r}"))
    }
}

fn entry_tail(v: &View) -> Vec<String> {
    v.tail
        .iter()
        .map(|l| format!("  {} {} {}", l.clock, l.glyph.ch(), l.text))
        .collect()
}

fn footer(me: &str, overflow: usize) -> String {
    let mut out = String::new();
    if overflow > 0 {
        out.push_str(&format!(
            "…and {overflow} more live session(s); gray-comms crew lists them all.\n"
        ));
    }
    out.push_str(&format!(
        "Message one: comms_send {{\"to\":\"<short>\",\"text\":\"...\"}} (add \"mode\":\"kickstart\" to direct), or `gray-comms send <short> \"...\"`. Claim what you are editing: comms_status {{\"text\":\"...\"}}. You are {me}."
    ));
    out
}

pub fn block(config: &Value, me_id: &str, tail: usize, dir: Option<&str>) -> Option<String> {
    let views = crate::session::collect(config, me_id, tail, dir);
    let others: Vec<&View> = views.iter().filter(|v| !v.me && v.status.fresh()).collect();
    if others.is_empty() {
        return None;
    }
    let me = views
        .iter()
        .find(|v| v.me)
        .map(|v| v.short.clone())
        .unwrap_or_else(|| short_for(config, me_id));
    let header = format!(
        "comms: {} other Gray session(s) live. Check them before overlapping work; `gray-comms crew` for the full view.",
        others.len()
    );
    let mut lines: Vec<String> = vec![header];
    let mut shown = 0usize;
    for (i, v) in others.iter().take(MAX_SESSIONS).enumerate() {
        let rest = others.len().min(MAX_SESSIONS).saturating_sub(i + 1);
        let reserve: usize = rest * 74;
        let mut candidate: Vec<String> = vec![entry_head(v)];
        if !v.claim.is_empty() {
            candidate.push(format!("  claim: {}", v.claim));
        }
        if let Some(r) = entry_recap(v) {
            candidate.push(r);
        }
        candidate.extend(entry_tail(v));
        let mut accepted = false;
        for line in candidate {
            let used = render(&lines, &me, others.len().saturating_sub(shown)).len();
            if used + line.len() + 1 + reserve > MAX_CHARS {
                break;
            }
            lines.push(line);
            accepted = true;
        }
        if accepted {
            shown += 1;
        } else {
            break;
        }
    }
    let mut text = render(&lines[1..], &me, others.len().saturating_sub(shown));
    loop {
        let total = lines[0].len() + 1 + text.len();
        if total <= MAX_CHARS || lines.len() <= 2 {
            break;
        }
        lines.pop();
        text = render(&lines[1..], &me, others.len().saturating_sub(shown));
    }
    Some(format!("{}\n{text}", lines[0]))
}

fn render(lines: &[String], me: &str, overflow: usize) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&footer(me, overflow));
    out
}

pub fn state_dir(config: &Value) -> PathBuf {
    crate::session::home(config).join("comms/seen")
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn changed(config: &Value, session_id: &str, text: &str) -> bool {
    let dir = state_dir(config);
    let path = dir.join(format!("{session_id}.json"));
    let digest = format!("{:016x}", hash(text));
    let prev = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("hash").and_then(Value::as_str).map(str::to_string));
    if prev.as_deref() == Some(digest.as_str()) {
        return false;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    let record = serde_json::json!({
        "session": session_id,
        "hash": digest,
        "chars": text.chars().count(),
        "at": chrono::Local::now().to_rfc3339(),
    });
    crate::config::atomic_json(&path, &record).is_ok() || true
}
