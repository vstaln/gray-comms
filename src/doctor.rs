//! Doctor preflight checks with safe errors.
//! Every failure is a category string; config values never surface.
use serde_json::{json, Value};

/// Checks `config` locally, then probes the daemon (join + ping + leave).
pub fn doctor(config: &Value) -> Result<(), String> {
    let gray_bin = config.get("gray_bin").and_then(Value::as_str).unwrap_or("");
    if !is_executable(gray_bin) {
        return Err("gray executable is missing or not executable".to_string());
    }
    let mut client = crate::client::Client::connect(config)?;
    client
        .call("ping", json!({}))
        .map_err(|e| format!("daemon answered join but not ping: {e}"))?;
    println!("Daemon connect, join and ping verified; local gray configuration verified.");
    println!("Room history, LAN reachability and live push were not tested.");
    Ok(())
}

/// `os.access(path, X_OK)` equivalent: a real file with any exec bit set.
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
