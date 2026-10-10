//! NDJSON protocol 1.1 sidecar.
//! Manifest answers without config; a down daemon fails fast and safe.
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;

/// Static manifest (`comms` 0.1.x, protocol 1.1, no commands).
pub static MANIFEST: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "name": "comms",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "commands": [],
        "hooks": ["prompt/context"],
        "tools": [
            {
                "name": "comms_join",
                "label": "Comms Join",
                "description": "Join the agent-comms mesh, optionally subscribing to a room. This agent's name comes from the config; join announces presence and returns how many DMs were waiting.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room to join (e.g. dev or #dev); omitted = presence only"},
                        "status": {"type": "string", "enum": ["active", "idle", "busy"], "description": "Presence to advertise"}
                    }
                }
            },
            {
                "name": "comms_say",
                "label": "Comms Say",
                "description": "Post a message to a comms room this agent belongs to. Room defaults to the configured home room.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room name; default: the configured home room"},
                        "text": {"type": "string", "description": "Message text (plain, up to 16 KiB)"}
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "comms_dm",
                "label": "Comms DM",
                "description": "Send a direct message to another agent by name. Online agents get it pushed; offline agents get it on their next check-in. Reply reports delivery.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": {"type": "string", "description": "Recipient agent name"},
                        "text": {"type": "string", "description": "Message text"}
                    },
                    "required": ["to", "text"]
                }
            },
            {
                "name": "comms_who",
                "label": "Comms Who",
                "description": "List agents on the mesh (or the members of one room) with live presence: active/idle/busy while connected, offline otherwise.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Restrict to members of this room"}
                    }
                }
            },
            {
                "name": "comms_read",
                "label": "Comms Read",
                "description": "Read comms history: a room's recent messages (room), a DM thread with one agent (with), this agent's DM inbox (no args), or only newly arrived DMs (unread:true — also what the prompt hook previews).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "room": {"type": "string", "description": "Room to read (must be a member)"},
                        "with": {"type": "string", "description": "Agent name: the DM thread with them"},
                        "unread": {"type": "boolean", "description": "Only newly arrived DMs"},
                        "limit": {"type": "integer", "description": "Max messages (default 20, max 100)"}
                    }
                }
            }
        ]
    })
});

const PROMPT_CONTEXT: &str = "Comms mesh tools: comms_join announces presence (optionally joining a room), comms_say posts to a room, comms_dm messages one agent by name (queued while they're offline), comms_who lists agents/presence, comms_read pulls room history or DM threads. DMs arrive between turns — new ones are flagged below. Keep messages plain text; never send secrets.";

fn is_error() -> Value {
    json!({
        "content": "Comms call failed. Run `gray-comms doctor`; do not blindly retry.",
        "is_error": true
    })
}

/// A specific, fixable failure. `detail` must never contain config values.
fn not_done(detail: impl std::fmt::Display) -> Value {
    json!({
        "content": format!("Not done: {detail}. Fix it and call again."),
        "is_error": true
    })
}

/// Dispatch one protocol method.
pub fn dispatch(method: &str, params: &Value, config_path: &Path) -> Value {
    if method == "plugin/manifest" {
        return MANIFEST.clone();
    }
    if method == "prompt/context" {
        return prompt_context(config_path);
    }
    if method == "tool/call" {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        return match name {
            "comms_join" | "comms_say" | "comms_dm" | "comms_who" | "comms_read" => {
                tool_call(name, params, config_path)
            }
            _ => is_error(),
        };
    }
    json!({"error": "Unsupported method"})
}

fn tool_call(name: &str, params: &Value, config_path: &Path) -> Value {
    let args = params.get("args").cloned().unwrap_or(Value::Null);
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => return not_done(e),
    };
    let mut client = match crate::client::Client::connect(&config) {
        Ok(c) => c,
        Err(e) => return not_done(e),
    };
    match run_tool(&mut client, name, &args, &config) {
        Ok(text) => json!({"content": text}),
        Err(e) => not_done(e),
    }
}

fn run_tool(
    c: &mut crate::client::Client,
    name: &str,
    args: &Value,
    config: &Value,
) -> Result<String, String> {
    match name {
        "comms_join" => {
            let mut out = Vec::new();
            if let Some(status) = args.get("status").and_then(Value::as_str) {
                c.call("status", json!({"status": status}))?;
            }
            if let Some(room) = args.get("room").and_then(Value::as_str) {
                let res = c.call("join_room", json!({"room": room}))?;
                let room = res
                    .get("room")
                    .and_then(Value::as_str)
                    .unwrap_or(room)
                    .to_string();
                let members = res
                    .get("members")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                if !c.rooms.iter().any(|r| r == &room) {
                    c.rooms.push(room.clone());
                }
                out.push(format!("joined #{room} ({members} members)"));
            }
            out.push(format!(
                "on the mesh as {}; rooms: {}",
                c.me,
                if c.rooms.is_empty() {
                    "none".to_string()
                } else {
                    c.rooms.join(", ")
                }
            ));
            Ok(out.join("; "))
        }
        "comms_say" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.trim().is_empty() {
                return Err("text is empty".to_string());
            }
            let room = args
                .get("room")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    config
                        .get("home_room")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .ok_or_else(|| "no room given and no home_room configured".to_string())?;
            let res = c.call("say", json!({"room": room, "text": text}))?;
            let room = res
                .get("message")
                .and_then(|m| m.get("room"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            Ok(format!("posted to #{room}"))
        }
        "comms_dm" => {
            let to = args.get("to").and_then(Value::as_str).unwrap_or("");
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.trim().is_empty() {
                return Err("text is empty".to_string());
            }
            let res = c.call("dm", json!({"to": to, "text": text}))?;
            let delivery = res
                .get("delivery")
                .and_then(Value::as_str)
                .unwrap_or("queued");
            Ok(format!("dm to {to}: {delivery}"))
        }
        "comms_who" => {
            let mut params = json!({});
            if let Some(room) = args.get("room").and_then(Value::as_str) {
                params["room"] = Value::String(room.to_string());
            }
            let res = c.call("who", params)?;
            let agents = res
                .get("agents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if agents.is_empty() {
                return Ok("no agents".to_string());
            }
            let lines: Vec<String> = agents
                .iter()
                .map(|a| {
                    let name = a.get("name").and_then(Value::as_str).unwrap_or("?");
                    let status = a.get("status").and_then(Value::as_str).unwrap_or("?");
                    let rooms = a
                        .get("rooms")
                        .and_then(Value::as_array)
                        .map(|r| {
                            r.iter()
                                .filter_map(Value::as_str)
                                .map(|s| format!("#{s}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    format!("{name} ({status}) {rooms}")
                })
                .collect();
            Ok(lines.join("\n"))
        }
        "comms_read" => {
            let mut params = json!({});
            for (k, v) in [("room", "room"), ("with", "with")] {
                if let Some(s) = args.get(k).and_then(Value::as_str) {
                    params[v] = Value::String(s.to_string());
                }
            }
            if args.get("unread").and_then(Value::as_bool) == Some(true) {
                params["unread"] = Value::Bool(true);
            }
            if let Some(n) = args.get("limit").and_then(Value::as_u64) {
                params["limit"] = json!(n);
            }
            let res = c.call("peek", params)?;
            Ok(format_messages(&res))
        }
        _ => Err("unknown tool".to_string()),
    }
}

/// Render a peek result as transcript lines: `from: text`.
fn format_messages(res: &Value) -> String {
    let msgs = res
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if msgs.is_empty() {
        return "no messages".to_string();
    }
    msgs.iter()
        .map(|m| {
            let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
            let content = m.get("content").and_then(Value::as_str).unwrap_or("");
            let to = m.get("to").and_then(Value::as_str);
            match to {
                Some(t) if t != "?" => format!("{from} -> {t}: {content}"),
                _ => format!("{from}: {content}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// prompt/context: static tool guidance plus a one-line note when DMs
/// arrived since the last look. Fails soft — a down daemon never blocks a
/// prompt.
fn prompt_context(config_path: &Path) -> Value {
    let mut text = PROMPT_CONTEXT.to_string();
    let arrived = (|| -> Option<Vec<Value>> {
        let config = crate::config::load_config(config_path).ok()?;
        let mut client = crate::client::Client::connect(&config).ok()?;
        let res = client.call("peek", json!({"unread": true, "limit": 20})).ok()?;
        res.get("messages").and_then(Value::as_array).cloned().map(|m| m.to_vec())
    })();
    if let Some(msgs) = arrived {
        if !msgs.is_empty() {
            let mut parts = Vec::new();
            for m in msgs.iter().take(5) {
                let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
                let content: String = m
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                parts.push(format!("{from}: {content}"));
            }
            let more = msgs.len().saturating_sub(5);
            let mut note = format!(
                "\n{} new comms DM{} arrived: {}",
                msgs.len(),
                if msgs.len() == 1 { "" } else { "s" },
                parts.join(" | ")
            );
            if more > 0 {
                note.push_str(&format!(" (+{more} more)"));
            }
            note.push_str(" — read them with comms_read unread:true or comms_read with:<name>.");
            text.push_str(&note);
        }
    }
    json!({"text": text})
}

/// The sidecar loop: one JSON request per stdin line, one `{"id", "result"}`
/// reply per line on stdout. `plugin/shutdown` and EOF exit.
pub fn serve(config_path: &Path) -> ! {
    use std::io::{BufRead, Write};
    const LIMIT: usize = 256 * 1024;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        let mut buf: Vec<u8> = Vec::new();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => std::process::exit(1),
        };
        if n == 0 {
            std::process::exit(0);
        }
        if buf.len() > LIMIT {
            std::process::exit(1);
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !req.is_object() {
            continue;
        }
        if req.get("method").and_then(Value::as_str) == Some("plugin/shutdown") {
            std::process::exit(0);
        }
        let id = match req.get("id") {
            Some(Value::Number(_)) => req.get("id").cloned().unwrap(),
            _ => continue,
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = match req.get("params") {
            Some(Value::Object(_)) => req.get("params").cloned().unwrap(),
            _ => json!({}),
        };
        let result = dispatch(method, &params, config_path);
        let row = json!({"id": id, "result": result});
        if writeln!(out, "{row}").is_err() {
            std::process::exit(1);
        }
        if out.flush().is_err() {
            std::process::exit(1);
        }
    }
}
