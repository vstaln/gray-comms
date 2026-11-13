use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::foreign::{Harness, Presence};

const MARK: &str = "hook --harness";

pub fn exe() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "gray-comms".to_string())
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

fn ppid_of(pid: u32) -> u32 {
    let Ok(raw) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    let Some((_, tail)) = raw.rsplit_once(") ") else {
        return 0;
    };
    let mut fields = tail.split_whitespace();
    fields
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0)
}

fn harness_pid(h: Harness) -> u32 {
    let mut pid = ppid();
    let mut found = 0;
    for _ in 0..8 {
        if pid <= 1 {
            break;
        }
        let cmd = cmdline(pid);
        if cmd.contains(h.label()) && !cmd.contains(MARK) {
            found = pid;
        }
        pid = ppid_of(pid);
    }
    if found == 0 {
        ppid()
    } else {
        found
    }
}

fn ppid() -> u32 {
    let Ok(raw) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    let Some(rest) = raw.rsplit_once(concatenated_close()) else {
        return 0;
    };
    let tail = rest.1;
    let mut fields = tail.split_whitespace();
    fields.next();
    fields
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0)
}

fn concatenated_close() -> &'static str {
    ") "
}

fn claude_slug(cwd: &str) -> String {
    cwd.replace('/', "-")
}

fn transcript_for(h: Harness, cwd: &str, sid: &str) -> String {
    match h {
        Harness::Claude => {
            let dir = user_home().join(".claude/projects").join(claude_slug(cwd));
            let exact = dir.join(format!("{sid}.jsonl"));
            if exact.is_file() {
                return exact.display().to_string();
            }
            newest_in(&dir)
        }
        Harness::Codex => newest_in(&user_home().join(".codex/sessions")),
        Harness::Opencode => String::new(),
    }
}

fn newest_in(dir: &Path) -> String {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().map(|x| x != "jsonl").unwrap_or(true) {
                continue;
            }
            let Ok(meta) = e.metadata() else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, p));
            }
        }
    }
    best.map(|(_, p)| p.display().to_string())
        .unwrap_or_default()
}

fn sid_from(cwd: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in cwd.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

pub fn hook(config: &Value, harness: &str) -> Result<String, String> {
    let h = Harness::parse(harness)
        .ok_or_else(|| format!("unknown harness \"{harness}\" (claude, codex or opencode)"))?;
    let mut raw = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut raw);
    let input: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_default();
    let sid = input
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| input.get("sessionId").and_then(Value::as_str))
        .or_else(|| input.get("thread_id").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| sid_from(&cwd));
    let transcript = input
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| transcript_for(h, &cwd, &sid));
    let presence = Presence {
        harness: h,
        sid: sid.clone(),
        pid: harness_pid(h),
        cwd: cwd.clone(),
        transcript,
        last_seen_ms: now_ms(),
    };
    crate::foreign::save(config, &presence)?;
    let key = presence.key();
    let mut out = String::new();
    if let Some(block) = crate::aware::block(config, &key, 3, None) {
        out.push_str(&block);
    }
    let mail = crate::foreign::drain(config, &key);
    if !mail.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&mail.join("\n"));
    }
    Ok(out)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Err(format!("cannot create {}", parent.display()));
        }
    }
    let body = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{body}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn hook_command(h: Harness) -> String {
    format!("{} {MARK} {}", exe(), h.label())
}

fn is_ours(cmd: &str, h: Harness) -> bool {
    cmd.contains(MARK) && cmd.contains(h.label())
}

fn merge_settings(path: &Path, h: Harness, on: bool) -> Result<String, String> {
    let mut doc: Value = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let events = ["SessionStart", "UserPromptSubmit"];
    let hooks = doc
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| format!("{} has a hooks key that is not an object", path.display()))?;
    for event in events {
        let list = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("{} hooks.{event} is not an array", path.display()))?;
        list.retain(|entry| {
            let cmds: Vec<String> = entry
                .get("hooks")
                .and_then(Value::as_array)
                .map(|hs| {
                    hs.iter()
                        .filter_map(|x| x.get("command").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            !cmds.iter().any(|c| is_ours(c, h))
        });
        if on {
            list.push(json!({
                "hooks": [{"type": "command", "command": hook_command(h), "timeout": 10}]
            }));
        }
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        doc.as_object_mut().map(|o| o.remove("hooks"));
    }
    atomic_json(path, &doc)?;
    Ok(path.display().to_string())
}

fn opencode_plugin(on: bool) -> Result<String, String> {
    let path = user_home().join(".config/opencode/plugins/gray-comms.js");
    if !on {
        let _ = std::fs::remove_file(&path);
        return Ok(path.display().to_string());
    }
    let cmd = hook_command(Harness::Opencode);
    let body = format!(
        "export const GrayComms = async ({{ $ }}) => ({{\n  event: async ({{ event }}) => {{\n    try {{\n      await $`{cmd}`.quiet();\n    }} catch (e) {{}}\n  }}\n}});\n"
    );
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Err(format!("cannot create {}", parent.display()));
        }
    }
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

pub fn alias_path() -> PathBuf {
    user_home().join(".local/bin/crew")
}

pub fn installed() -> Vec<String> {
    let mut out = Vec::new();
    let claude = user_home().join(".claude/settings.json");
    if let Ok(raw) = std::fs::read(&claude) {
        if String::from_utf8_lossy(&raw).contains(MARK) {
            out.push(Harness::Claude.label().to_string());
        }
    }
    let codex = user_home().join(".codex/hooks.json");
    if let Ok(raw) = std::fs::read(&codex) {
        if String::from_utf8_lossy(&raw).contains(MARK) {
            out.push(Harness::Codex.label().to_string());
        }
    }
    if user_home()
        .join(".config/opencode/plugins/gray-comms.js")
        .is_file()
    {
        out.push(Harness::Opencode.label().to_string());
    }
    out
}

fn alias(on: bool) -> Result<String, String> {
    let path = alias_path();
    if !on {
        if std::fs::read_link(&path).is_ok() {
            let _ = std::fs::remove_file(&path);
        }
        return Ok(path.display().to_string());
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Err(format!("cannot create {}", parent.display()));
        }
    }
    if path.is_file() && std::fs::read_link(&path).is_err() {
        return Err(format!(
            "{} exists and is not a symlink; leaving it alone",
            path.display()
        ));
    }
    let _ = std::fs::remove_file(&path);
    std::os::unix::fs::symlink(exe(), &path)
        .map_err(|e| format!("cannot link {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

pub fn install(config: &Value, only: Option<&str>, on: bool) -> Result<(), String> {
    let _ = config;
    let wanted: Vec<Harness> = match only.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("all") => Harness::all().to_vec(),
        Some(other) => vec![Harness::parse(other).ok_or_else(|| {
            format!("unknown harness \"{other}\" (claude, codex, opencode or all)")
        })?],
    };
    for h in wanted {
        let where_done = match h {
            Harness::Claude => merge_settings(
                &user_home().join(".claude/settings.json"),
                Harness::Claude,
                on,
            )?,
            Harness::Codex => {
                merge_settings(&user_home().join(".codex/hooks.json"), Harness::Codex, on)?
            }
            Harness::Opencode => opencode_plugin(on)?,
        };
        let verb = if on { "hooked" } else { "unhooked" };
        println!("{verb} {} — {where_done}", h.label());
    }
    let link = alias(on)?;
    println!(
        "{} — {link}",
        if on { "crew alias" } else { "alias removed" }
    );
    Ok(())
}
