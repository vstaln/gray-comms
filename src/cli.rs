//! CLI tree: `gray-comms --config <path> <subcommand>`.
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Cross-agent message mesh: daemon, sidecar tools, and a small client CLI.
#[derive(Debug, Parser)]
#[command(name = "gray-comms", bin_name = "gray comms")]
pub struct Cli {
    /// Config file path.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn config_path(&self) -> PathBuf {
        match &self.config {
            Some(p) => p.clone(),
            None => crate::config::default_path(),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Interactive setup: agent name, bind policy, port, token.
    Setup,
    /// Run the comms daemon in the foreground.
    Run,
    Sidecar,
    /// Print the plugin manifest (what `gray plugin install comms` reads).
    #[command(hide = true)]
    Manifest,
    Register,
    Install,
    Status,
    Stop,
    Restart,
    Doctor,
    Uninstall,
    /// Join a room as the configured agent.
    Join { room: String },
    /// Post a message to a room.
    Say { room: String, text: String },
    /// Direct-message an agent.
    Dm { to: String, text: String },
    /// List agents (or one room's members) with presence.
    Who { room: Option<String> },
    /// List rooms and their members.
    Rooms,
    /// Read history: `--room`, `--with`, `--unread` or the DM inbox.
    Read {
        #[arg(long)]
        room: Option<String>,
        #[arg(long)]
        with: Option<String>,
        #[arg(long)]
        unread: bool,
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Stream live events (room messages, DMs, joins/leaves) until Ctrl-C.
    Listen {
        /// Rooms to join on connect, comma-separated.
        #[arg(long)]
        rooms: Option<String>,
    },
}

/// Register the outgoing `comms_*` tools in gray's plugin lock.
/// Matches `gray_plugin::lock::LockEntry` (explicit argv, no fake install).
pub fn register(config: &Value, config_path: &Path) -> Result<(), String> {
    let gray_home = config
        .get("gray_home")
        .and_then(Value::as_str)
        .unwrap_or("");
    let lock = Path::new(gray_home).join("plugins/lock.json");
    let mut data: Value = std::fs::read(&lock)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({"schema": 1, "plugins": {}}));
    if data.get("schema").and_then(Value::as_u64) != Some(1)
        || !data.get("plugins").is_some_and(Value::is_object)
    {
        return Err("Unsupported gray plugin lock; not changing it".to_string());
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| "cannot locate plugin binary".to_string())?;
    let resolved = config_path
        .to_str()
        .map(str::to_string)
        .unwrap_or_else(|| config_path.to_string_lossy().into_owned());
    let argv = vec![
        Value::String(exe.clone()),
        Value::String("sidecar".to_string()),
        Value::String("--config".to_string()),
        Value::String(resolved),
    ];
    let prev_exe = data.pointer("/plugins/comms/argv/0").and_then(Value::as_str);
    if prev_exe.is_some_and(|p| p != exe && Path::new(p).is_file()) {
        return Err("Another comms plugin is registered; refusing to overwrite it".to_string());
    }
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    // Merge into any existing row: gray keeps cli_argv and the consent
    // record in the same lock entry, so replacing the object would drop them.
    let entry = &mut data["plugins"]["comms"];
    if !entry.is_object() {
        *entry = json!({});
    }
    entry["ecosystem"] = json!("gray-native");
    entry["version"] = json!(env!("CARGO_PKG_VERSION"));
    entry["source"] = json!("https://github.com/vstaln/gray-comms");
    entry["argv"] = Value::Array(argv);
    entry["adapter_version"] = json!("1.1");
    entry["installed_at"] = json!(installed_at);
    if entry.get("hash").is_none() {
        entry["hash"] = json!("");
    }
    if entry.get("scope").is_none() {
        entry["scope"] = json!("user");
    }
    if entry.get("enabled").is_none() {
        entry["enabled"] = json!(true);
    }
    // Re-register heals a row whose CLI binary moved (reinstall, cargo
    // install to a new prefix); a working custom cli_argv is kept.
    let cli_ok = entry
        .get("cli_argv")
        .and_then(|a| a.get(0))
        .and_then(Value::as_str)
        .is_some_and(|p| Path::new(p).is_file());
    if !cli_ok {
        entry["cli_argv"] = json!([exe]);
    }
    crate::config::atomic_json(&lock, &data)
}

/// Parse the service-install answer: default yes.
pub fn install_confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

fn connect(config_path: &Path) -> Result<(crate::client::Client, Value), String> {
    let config = crate::config::load_config(config_path)?;
    let client = crate::client::Client::connect(&config)?;
    Ok((client, config))
}

fn print_messages(res: &Value) {
    let msgs = res
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if msgs.is_empty() {
        println!("(no messages)");
        return;
    }
    for m in &msgs {
        let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
        let content = m.get("content").and_then(Value::as_str).unwrap_or("");
        match m.get("to").and_then(Value::as_str) {
            Some(t) => println!("{from} -> {t}: {content}"),
            None => println!("{from}: {content}"),
        }
    }
}

fn print_event(v: &Value) -> bool {
    let Some(ev) = v.get("event") else {
        return false;
    };
    let ty = ev.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "room_message" | "dm" => {
            let m = ev.get("message").cloned().unwrap_or(Value::Null);
            let from = m.get("from").and_then(Value::as_str).unwrap_or("?");
            let content = m.get("content").and_then(Value::as_str).unwrap_or("");
            if ty == "dm" {
                let to = m.get("to").and_then(Value::as_str).unwrap_or("?");
                println!("[dm] {from} -> {to}: {content}");
            } else {
                let room = m.get("room").and_then(Value::as_str).unwrap_or("?");
                println!("[#{room}] {from}: {content}");
            }
        }
        "member_joined" | "member_left" => {
            let room = ev.get("room").and_then(Value::as_str).unwrap_or("?");
            let agent = ev.get("agent").and_then(Value::as_str).unwrap_or("?");
            let verb = if ty == "member_joined" { "joined" } else { "left" };
            println!("[#{room}] {agent} {verb}");
        }
        _ => println!("{v}"),
    }
    true
}

/// Dispatch a parsed subcommand.
pub fn run(cmd: &Command, config_path: &Path) -> Result<(), String> {
    match cmd {
        Command::Sidecar => crate::sidecar::serve(config_path),
        Command::Manifest => {
            use clap::CommandFactory;
            let mut m = crate::sidecar::MANIFEST.clone();
            m["completion"] = Cli::command()
                .get_subcommands()
                .filter(|c| {
                    !c.is_hide_set() && !["run", "sidecar", "register"].contains(&c.get_name())
                })
                .map(|c| Value::String(c.get_name().to_string()))
                .collect();
            m["sidecar_args"] = serde_json::json!(["sidecar"]);
            println!("{m}");
            Ok(())
        }
        Command::Register => {
            let config: Value = crate::config::load_config(config_path)?;
            register(&config, config_path)
        }
        Command::Setup => {
            use crate::setup::Prompter;
            let mut io = crate::setup::Tty;
            let done = crate::setup::run(config_path, &mut io)?;
            if done {
                let config: Value = crate::config::load_config(config_path)?;
                register(&config, config_path)?;
                println!("Comms tools registered with gray.");
                if crate::service::installed(config_path) {
                    // A reconfigured daemon takes effect now.
                    println!("{}", crate::service::restart(config_path)?);
                    return Ok(());
                }
                let answer = io.prompt("Enable and start the daemon now? [Y/n] ")?;
                if install_confirmed(&answer) {
                    let line = crate::service::install(config_path)?;
                    println!("{line}");
                    println!("Run gray comms status to check it.");
                    if matches!(
                        crate::service::detect(),
                        crate::service::Supervisor::SystemdUser { .. }
                    ) {
                        println!("For operation after logout: loginctl enable-linger \"$USER\"");
                    }
                } else {
                    println!(
                        "Run gray comms install when ready, or gray comms run in the foreground."
                    );
                }
            }
            Ok(())
        }
        Command::Run => {
            let config = crate::config::load_config(config_path)?;
            let store = crate::daemon::open_store(config_path)?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| "cannot start daemon runtime".to_string())?;
            rt.block_on(crate::daemon::serve(&config, store))
        }
        Command::Status => {
            println!("{}", crate::service::status(config_path)?);
            Ok(())
        }
        Command::Stop => {
            println!("{}", crate::service::stop(config_path)?);
            Ok(())
        }
        Command::Restart => {
            println!("{}", crate::service::restart(config_path)?);
            Ok(())
        }
        Command::Uninstall => {
            let line = crate::service::uninstall(config_path)?;
            println!("{line}");
            println!("Daemon removed. Configuration, history and registered tools retained.");
            Ok(())
        }
        Command::Install => {
            let line = crate::service::install(config_path)?;
            println!("{line}");
            if matches!(
                crate::service::detect(),
                crate::service::Supervisor::SystemdUser { .. }
            ) {
                println!("To survive logout, enable user linger: loginctl enable-linger \"$USER\"");
            }
            Ok(())
        }
        Command::Doctor => {
            let config = crate::config::load_config(config_path)?;
            crate::doctor::doctor(&config)
        }
        Command::Join { room } => {
            let (mut c, _) = connect(config_path)?;
            let res = c.call("join_room", json!({"room": room}))?;
            let members = res
                .get("members")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            println!("joined #{room} ({members} members)");
            Ok(())
        }
        Command::Say { room, text } => {
            let (mut c, _) = connect(config_path)?;
            c.call("say", json!({"room": room, "text": text}))?;
            println!("posted to #{room}");
            Ok(())
        }
        Command::Dm { to, text } => {
            let (mut c, _) = connect(config_path)?;
            let res = c.call("dm", json!({"to": to, "text": text}))?;
            let delivery = res
                .get("delivery")
                .and_then(Value::as_str)
                .unwrap_or("queued");
            println!("dm to {to}: {delivery}");
            Ok(())
        }
        Command::Who { room } => {
            let (mut c, _) = connect(config_path)?;
            let mut params = json!({});
            if let Some(r) = room {
                params["room"] = json!(r);
            }
            let res = c.call("who", params)?;
            let agents = res
                .get("agents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if agents.is_empty() {
                println!("(no agents)");
            }
            for a in &agents {
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
                println!("{name}\t{status}\t{rooms}");
            }
            Ok(())
        }
        Command::Rooms => {
            let (mut c, _) = connect(config_path)?;
            let res = c.call("rooms", json!({}))?;
            let rooms = res
                .get("rooms")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if rooms.is_empty() {
                println!("(no rooms)");
            }
            for r in &rooms {
                let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
                let members = r
                    .get("members")
                    .and_then(Value::as_array)
                    .map(|m| {
                        m.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                println!("#{name}\t{members}");
            }
            Ok(())
        }
        Command::Read {
            room,
            with,
            unread,
            limit,
        } => {
            let (mut c, _) = connect(config_path)?;
            let mut params = json!({});
            if let Some(r) = room {
                params["room"] = json!(r);
            }
            if let Some(w) = with {
                params["with"] = json!(w);
            }
            if *unread {
                params["unread"] = json!(true);
            }
            if let Some(n) = limit {
                params["limit"] = json!(n);
            }
            let res = c.call("peek", params)?;
            print_messages(&res);
            Ok(())
        }
        Command::Listen { rooms } => {
            let (mut c, _) = connect(config_path)?;
            c.listen()?;
            if let Some(list) = rooms {
                for r in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    c.call("join_room", json!({"room": r}))?;
                }
            }
            eprintln!("listening as {} — ctrl-c to stop", c.me);
            for ev in std::mem::take(&mut c.events) {
                print_event(&ev);
            }
            loop {
                match c.next_event() {
                    Ok(v) => {
                        print_event(&v);
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        break;
                    }
                }
            }
            Ok(())
        }
    }
}
