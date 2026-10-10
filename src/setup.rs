//! Interactive setup wizard.
//! The LAN token stays in the wizard; the model never receives it.
use serde_json::{json, Value};
use std::path::Path;

/// Terminal IO. The real implementation talks to stdin/stderr; tests inject
/// a scripted fake. `print_line` goes to user-visible output (never logs).
pub trait Prompter {
    fn is_terminal(&self) -> bool;
    fn prompt(&mut self, text: &str) -> Result<String, String>;
    fn prompt_hidden(&mut self, text: &str) -> Result<String, String>;
    fn confirm(&mut self, text: &str) -> Result<bool, String>;
    fn print_line(&mut self, text: &str);
}

/// Real terminal prompter (hidden input via `stty -echo`, no new deps).
pub struct Tty;

impl Prompter for Tty {
    fn is_terminal(&self) -> bool {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
    fn prompt(&mut self, text: &str) -> Result<String, String> {
        use std::io::Write;
        let _ = std::io::stderr().write_all(text.as_bytes());
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|_| "cannot read terminal input".to_string())?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }
    fn prompt_hidden(&mut self, text: &str) -> Result<String, String> {
        use std::io::Write;
        let _ = std::io::stderr().write_all(text.as_bytes());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let res = without_echo(|| std::io::stdin().read_line(&mut line));
        let _ = std::io::stderr().write_all(b"\n");
        res.map_err(|_| "cannot read terminal input".to_string())?;
        Ok(line.trim().to_string())
    }
    fn confirm(&mut self, text: &str) -> Result<bool, String> {
        let answer = self.prompt(text)?.trim().to_ascii_lowercase();
        Ok(answer == "y" || answer == "yes")
    }
    fn print_line(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

/// Echo off for one read. Ctrl-C would otherwise kill setup with the
/// terminal still silent, so SIGINT restores it first (tcsetattr is
/// async-signal-safe).
fn without_echo<T>(read: impl FnOnce() -> T) -> T {
    static SAVED: std::sync::OnceLock<libc::termios> = std::sync::OnceLock::new();
    extern "C" fn restore_and_exit(_: libc::c_int) {
        if let Some(t) = SAVED.get() {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
        }
        unsafe { libc::_exit(130) };
    }
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(0, &mut term) } != 0 {
        return read();
    }
    let saved = *SAVED.get_or_init(|| term);
    let mut quiet = saved;
    quiet.c_lflag &= !libc::ECHO;
    let handler = restore_and_exit as extern "C" fn(libc::c_int) as libc::sighandler_t;
    let old = unsafe {
        libc::tcsetattr(0, libc::TCSANOW, &quiet);
        libc::signal(libc::SIGINT, handler)
    };
    let out = read();
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, &saved);
        libc::signal(libc::SIGINT, old);
    }
    out
}

/// Resolve the gray binary (`GRAY_BIN` env or `PATH` lookup).
fn resolve_gray(explicit: &str) -> Option<String> {
    let candidate = explicit.trim();
    if candidate.is_empty() {
        return None;
    }
    if candidate.contains('/') {
        let path = Path::new(candidate);
        if path.is_file() && is_executable(path) {
            return Some(candidate.to_string());
        }
        return None;
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(candidate);
        if full.is_file() && is_executable(&full) {
            return Some(full.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn absolutize(path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn shellexpand_home(raw: &str) -> String {
    if raw == "~" || raw.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let home = Path::new(&home).to_string_lossy().into_owned();
            return if raw == "~" {
                home
            } else {
                format!("{home}/{}", &raw[2..])
            };
        }
    }
    raw.to_string()
}

fn default_name() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    let host: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let host = host.trim_matches('-');
    if host.is_empty() {
        "gray".to_string()
    } else {
        format!("gray-{}", host.chars().take(32).collect::<String>())
    }
}

/// A random token for LAN mode (not secret-grade; a shared push-key).
fn gen_token() -> String {
    crate::store::uuid_hex()
}

/// The wizard: agent name, bind policy, port, home room, LAN token.
pub fn run(path: &Path, io: &mut dyn Prompter) -> Result<bool, String> {
    if !io.is_terminal() {
        return Err("Setup needs a terminal".to_string());
    }
    if path.exists() && !io.confirm("Replace existing configuration? [y/N] ")? {
        return Ok(false);
    }
    // Gray's binary and home are resolved silently (env, then the
    // conventional defaults) — derived facts, not questions.
    let gray = resolve_gray(&std::env::var("GRAY_BIN").unwrap_or_else(|_| "gray".to_string()))
        .ok_or_else(|| "Install gray first; executable not found".to_string())?;
    let gray_home = absolutize(Path::new(&shellexpand_home(
        &std::env::var("GRAY_HOME").unwrap_or_else(|_| "~/.gray".to_string()),
    )));

    // Prior config: the wizard replaces only what it asks about.
    let prior = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object);

    for line in [
        "Set up the comms mesh: a tiny TCP daemon holding rooms, presence and",
        "durable history; the comms_* tools talk to it. Two grays on one box,",
        "or across a LAN, message each other through it.",
    ] {
        io.print_line(line);
    }

    let def_name = prior
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(default_name);
    let answer = io.prompt(&format!("This agent's mesh name [{def_name}]: "))?;
    let name = if answer.trim().is_empty() {
        def_name
    } else {
        answer.trim().to_string()
    };
    if !crate::proto::agent_name_valid(&name) {
        return Err("Mesh name must be 1-64 chars of [A-Za-z0-9_.-]".to_string());
    }

    let lan = io.confirm(
        "Listen on the LAN (other machines reach this daemon)? [y/N] ",
    )?;
    let bind = if lan { "0.0.0.0" } else { "127.0.0.1" };

    let prior_port = prior
        .as_ref()
        .and_then(|p| p.get("port"))
        .and_then(Value::as_u64)
        .unwrap_or(19876);
    let port_answer = io.prompt(&format!("Daemon port [{prior_port}]: "))?;
    let port: u64 = if port_answer.trim().is_empty() {
        prior_port
    } else {
        port_answer
            .trim()
            .parse()
            .map_err(|_| "Port must be a number between 1 and 65535".to_string())?
    };
    if !(1..=65535).contains(&port) {
        return Err("Port must be a number between 1 and 65535".to_string());
    }

    let home_answer = io.prompt("Home room for comms_say [#lobby → lobby]: ")?;
    let home_room = if home_answer.trim().is_empty() {
        "lobby".to_string()
    } else {
        crate::proto::room_normalize(home_answer.trim())
            .ok_or_else(|| "Room must be a slug like #dev or dev".to_string())?
    };

    // LAN without a token lets anyone who can reach the port read the mesh.
    let mut token = String::new();
    if lan {
        let answer = io.prompt(
            "Shared token for LAN joins (blank generates one; it goes in the config): ",
        )?;
        token = if answer.trim().is_empty() {
            gen_token()
        } else {
            answer.trim().to_string()
        };
        if !token.is_empty() {
            io.print_line("Peers need the same token in their config to join.");
        }
    }

    let workdir = path
        .parent()
        .map(|p| absolutize(p).to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let mut saved = json!({
        "name": name,
        "bind": bind,
        "host": "127.0.0.1",
        "port": port,
        "home_room": home_room,
        "history_limit": 500,
        "gray_bin": absolutize(Path::new(&gray)).to_string_lossy(),
        "gray_home": gray_home.to_string_lossy(),
        "workdir": workdir,
    });
    if !token.is_empty() {
        saved["token"] = Value::String(token);
    }
    // A re-run replaces only what the wizard asked about: other keys survive.
    if let Some(mut merged) = prior {
        for (k, v) in saved.as_object().into_iter().flatten() {
            merged[k] = v.clone();
        }
        if crate::config::validate_config(&merged).is_ok() {
            saved = merged;
        }
    }
    crate::config::save_config(path, &saved)?;
    io.print_line("Configuration saved privately.");

    // If a daemon is already answering, prove the config against it.
    if crate::doctor::doctor(&saved).is_ok() {
        io.print_line("Doctor verified the running daemon.");
    } else {
        io.print_line("No daemon is answering yet; install starts one.");
    }
    Ok(true)
}
