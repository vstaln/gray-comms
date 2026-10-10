//! Private config load/save/validate.
//! Errors never contain config values.
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Default config path: ~/.config/gray-comms/config.json
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/gray-comms/config.json")
}

/// Write JSON (pretty + trailing newline) atomically: parents 0700,
/// temp file + rename, replacement mode 0600.
pub fn atomic_json(path: &Path, data: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|_| "cannot create config directory".to_string())?;
        }
    }
    let mut text =
        serde_json::to_string_pretty(data).map_err(|_| "cannot encode config".to_string())?;
    text.push('\n');
    let tmp_path = path.with_extension("tmp");
    // Born 0600: a LAN token is never readable by others, not even briefly.
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|_| "cannot write config".to_string())?;
    }
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "cannot protect config".to_string())?;
    fs::rename(&tmp_path, path).map_err(|_| "cannot replace config".to_string())?;
    Ok(())
}

fn str_nonempty(data: &Value, key: &str) -> Result<(), String> {
    match data.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(()),
        _ => Err(format!("{key} is missing; run setup")),
    }
}

/// Where the client side connects: `host` when set, else `bind`.
pub fn connect_addr(config: &Value) -> String {
    let host = config
        .get("host")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            config
                .get("bind")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "127.0.0.1".to_string());
    // Binding wildcard still connects on loopback.
    let host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1".to_string()
    } else {
        host
    };
    let port = config.get("port").and_then(Value::as_u64).unwrap_or(19876);
    format!("{host}:{port}")
}

/// Validate a config object. Error texts never echo config values.
pub fn validate_config(data: &Value) -> Result<(), String> {
    if !data.is_object() {
        return Err("Configuration must be an object".to_string());
    }
    str_nonempty(data, "name")?;
    match data.get("name").and_then(Value::as_str) {
        Some(n) if crate::proto::agent_name_valid(n) => {}
        _ => return Err("name must be 1-64 chars of [A-Za-z0-9_.-]".to_string()),
    }
    for key in ["bind", "host"] {
        if let Some(v) = data.get(key).and_then(Value::as_str) {
            if v.is_empty()
                || v.len() > 255
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_'))
            {
                return Err(format!("{key} must be a hostname or IP literal"));
            }
        }
    }
    if let Some(v) = data.get("port") {
        if !v.as_u64().is_some_and(|n| (1..=65535).contains(&n)) {
            return Err("port must be an integer between 1 and 65535".to_string());
        }
    }
    if let Some(v) = data.get("history_limit") {
        if !v.as_u64().is_some_and(|n| (1..=100_000).contains(&n)) {
            return Err("history_limit must be an integer between 1 and 100000".to_string());
        }
    }
    if let Some(v) = data.get("token") {
        if !v.is_string() {
            return Err("token must be a string".to_string());
        }
    }
    if let Some(v) = data.get("home_room") {
        let ok = v
            .as_str()
            .is_some_and(|r| crate::proto::room_normalize(r).is_some());
        if !ok {
            return Err("home_room must be a room slug like #dev or dev".to_string());
        }
    }
    for key in ["gray_bin", "gray_home", "workdir"] {
        let ok = data
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| Path::new(s).is_absolute());
        if !ok {
            return Err(format!("{key} must be an absolute path"));
        }
    }
    Ok(())
}

/// Load + validate. Any IO/parse failure maps to the setup hint.
pub fn load_config(path: &Path) -> Result<Value, String> {
    let data: Value = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| "Configuration missing or invalid; run setup".to_string())?;
    validate_config(&data)?;
    Ok(data)
}

/// Validate + atomic save.
pub fn save_config(path: &Path, data: &Value) -> Result<(), String> {
    validate_config(data)?;
    atomic_json(path, data)
}
