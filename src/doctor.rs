use serde_json::{json, Value};

fn check(pass: bool, what: &str, detail: &str) -> bool {
    println!("{} {what} — {detail}", if pass { "✓" } else { "✗" });
    pass
}

enum Daemon {
    Up { addr: String },
    Down { why: String },
}

fn daemon_mode(config: &Value) -> Daemon {
    let addr = crate::config::connect_addr(config);
    let mut why = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match crate::client::Client::connect(config) {
            Ok(mut client) => match client.call("ping", json!({})) {
                Ok(_) => return Daemon::Up { addr },
                Err(e) => why = format!("{addr} answered join but not ping: {e}"),
            },
            Err(e) => why = e,
        }
    }
    Daemon::Down { why }
}

fn live_push_ok(config: &Value) -> bool {
    let watcher = format!("comms-doctor-{}", std::process::id());
    let Ok(mut listener) = crate::client::Client::connect_as(config, &watcher) else {
        return false;
    };
    if listener.listen().is_err() {
        return false;
    }
    let Ok(mut other) = crate::client::Client::connect_as(config, &format!("{watcher}-q")) else {
        return false;
    };
    let Ok(who) = other.call("who", json!({})) else {
        return false;
    };
    who.pointer("/agents")
        .and_then(Value::as_array)
        .is_some_and(|agents| {
            agents.iter().any(|a| {
                a.get("name").and_then(Value::as_str) == Some(watcher.as_str())
                    && a.get("status").and_then(Value::as_str) != Some("offline")
            })
        })
}

pub fn doctor(config: &Value) -> Result<(), String> {
    let gray_bin = config.get("gray_bin").and_then(Value::as_str).unwrap_or("");
    let mut ok = check(
        is_executable(gray_bin),
        "gray binary",
        if gray_bin.is_empty() {
            "gray_bin is missing from the config"
        } else {
            gray_bin
        },
    );

    let daemon = daemon_mode(config);
    match &daemon {
        Daemon::Up { addr } => {
            ok &= check(
                true,
                "daemon mode",
                &format!("up at {addr} (rooms, cross-reboot history, LAN)"),
            );
            ok &= check(
                live_push_ok(config),
                "live push",
                "a listener joined with listen and shows online",
            );
        }
        Daemon::Down { why } => {
            println!("• daemon mode — down ({why}); local run: crew, the block and send");
        }
    }

    let root = crate::session::root(config);
    let live = crate::session::live_ids(config);
    ok &= check(
        root.is_dir() && !live.is_empty(),
        "session store",
        &format!("{} · {} live session(s)", root.display(), live.len()),
    );

    if is_executable(gray_bin) {
        if let Some(n) = gray_live_count(gray_bin) {
            ok &= check(
                n == live.len(),
                "matches `gray read --live`",
                &format!("{n} id(s) from gray's own view"),
            );
        }
    }

    let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
    let identity = crate::identity::Identity::from_config(config)?;
    let mesh = crate::identity::mesh_name_now(config, Some(&me)).unwrap_or_default();
    ok &= check(
        !mesh.is_empty(),
        "identity",
        &format!(
            "{} ({}): this session joins as {mesh}",
            identity.label(),
            match &identity {
                crate::identity::Identity::Fixed(_) => "config name".to_string(),
                crate::identity::Identity::Session { prefix } => match prefix {
                    Some(p) => format!("prefix {p}"),
                    None => "session id only".to_string(),
                },
            }
        ),
    );

    if !me.is_empty() {
        let block = crate::aware::block(config, &me, 3, None);
        ok &= check(
            block.is_some(),
            "awareness block",
            &match &block {
                Some(b) => format!(
                    "{} chars now (cap {}), {} other session(s) fresh",
                    b.chars().count(),
                    crate::aware::MAX_CHARS,
                    b.lines().filter(|l| l.starts_with('•')).count()
                ),
                None => "no other fresh session to show".to_string(),
            },
        );
        let dir = crate::aware::state_dir(config);
        let writable = std::fs::create_dir_all(&dir).is_ok();
        ok &= check(
            writable,
            "state dir",
            &format!("{} (seen-hash per session)", dir.display()),
        );
    }

    let lock = crate::session::home(config).join("plugins/lock.json");
    let entry = std::fs::read(&lock)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.pointer("/plugins/comms").cloned());
    let enabled = entry
        .as_ref()
        .and_then(|e| e.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let global = entry
        .as_ref()
        .and_then(|e| e.get("global"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ok &= check(
        enabled && global,
        "registered in gray",
        &format!(
            "{} (enabled: {enabled}, global: {global} — the hook runs in lean sessions too)",
            lock.display()
        ),
    );

    if ok {
        println!();
        println!("comms is wired: sessions join as themselves, the block lands in prompts, comms_send delivers mid-turn.");
        Ok(())
    } else {
        Err("doctor found problems above".to_string())
    }
}

fn gray_live_count(gray_bin: &str) -> Option<usize> {
    let out = std::process::Command::new(gray_bin)
        .args(["read", "--live", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v.as_array().map(|a| a.len())
}

fn is_executable(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
