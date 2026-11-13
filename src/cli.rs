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
    Sidecar {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        args: Vec<String>,
    },
    /// Print the plugin manifest (what `gray plugin install comms` reads).
    #[command(hide = true)]
    Manifest,
    Register,
    Install,
    Status,
    Stop,
    Restart,
    Doctor {
        /// Repair what can be repaired: config keys, state dirs, plugin
        /// registration, the crew alias, and a stopped service.
        #[arg(long)]
        fix: bool,
    },
    Uninstall,
    /// Join a room as the configured agent.
    Join {
        room: String,
    },
    /// Post a message to a room.
    Say {
        room: String,
        text: String,
    },
    /// Direct-message an agent.
    Dm {
        to: String,
        text: String,
    },
    /// List agents (or one room's members) with presence.
    Who {
        room: Option<String>,
    },
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
    /// Every live Gray session on this machine (what the prompt block samples).
    Crew {
        /// Machine-readable rows.
        #[arg(long)]
        json: bool,
        /// Transcript lines per session.
        #[arg(long)]
        tail: Option<u64>,
        /// Only sessions whose cwd contains this path fragment.
        #[arg(long)]
        dir: Option<String>,
        /// Include sessions idle longer than 30 minutes.
        #[arg(long)]
        all: bool,
    },
    /// Message another live session by short id, pid, cwd substring, or --all.
    Send {
        text: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        all: bool,
        /// Deliver as a directive the target acts on now.
        #[arg(short = 'k', long)]
        kickstart: bool,
    },
    /// Claim what you are working on; shown in every other live session's block for 30 minutes.
    Claim {
        /// Free text, e.g. "editing src/session.rs".
        text: Option<String>,
        /// Drop the claim early.
        #[arg(long)]
        clear: bool,
        /// Session to attach the claim to (default: this session).
        #[arg(long)]
        session: Option<String>,
    },
    /// Fed by a harness hook (Claude Code, Codex, OpenCode): records presence
    /// and prints the awareness block plus any mail waiting for this session.
    #[command(hide = true)]
    Hook {
        #[arg(long, default_value = "claude")]
        harness: String,
    },
    /// Merge the comms hook into Claude Code, Codex and OpenCode configs and
    /// install the `crew` alias.
    InstallHook {
        /// claude, codex, opencode, or all (default).
        #[arg(long)]
        harness: Option<String>,
        /// Remove the hooks and the alias instead.
        #[arg(long)]
        uninstall: bool,
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
    let prev_exe = data
        .pointer("/plugins/comms/argv/0")
        .and_then(Value::as_str);
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
    // The awareness block has to run in lean sessions too, so the hook is
    // global: gray only consults global plugins for prompt/context there.
    entry["global"] = json!(true);
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
    let plugins = Path::new(gray_home).join("plugins");
    let mut manifest = crate::sidecar::MANIFEST.clone();
    manifest["sidecar_args"] = json!(["sidecar"]);
    crate::config::atomic_json(&plugins.join("comms-manifest.json"), &manifest)?;
    crate::config::atomic_json(&lock, &data)
}

/// Parse the service-install answer: default yes.
pub fn install_confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

fn ago(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

fn pending_of(config: &Value, v: &crate::session::View) -> usize {
    match v.harness() {
        Some(_) => crate::foreign::mail_count(config, &v.id),
        None => crate::session::pending_mail(config, &v.id),
    }
}

fn view_json(v: &crate::session::View) -> Value {
    json!({
        "id": v.id,
        "short": v.short,
        "pid": v.pid,
        "cwd": v.cwd,
        "cwd_tilde": v.cwd_short(),
        "model": v.model,
        "origin": v.origin,
        "status": v.status.label(),
        "claim": v.claim,
        "harness": v.harness(),
        "fresh": v.status.fresh(),
        "idle_secs": v.idle_secs,
        "recap": v.recap(),
        "self": v.me,
        "tail": v.tail.iter().map(|l| json!({
            "clock": l.clock,
            "glyph": l.glyph.ch().to_string(),
            "text": l.text,
            "tool": l.tool,
        })).collect::<Vec<Value>>(),
    })
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
            let verb = if ty == "member_joined" {
                "joined"
            } else {
                "left"
            };
            println!("[#{room}] {agent} {verb}");
        }
        _ => println!("{v}"),
    }
    true
}

/// Dispatch a parsed subcommand.
pub fn run(cmd: &Command, config_path: &Path) -> Result<(), String> {
    match cmd {
        Command::Sidecar { args } => {
            if args.is_empty() {
                crate::sidecar::serve(config_path)
            } else {
                let argv = std::iter::once("gray-comms".to_string()).chain(args.iter().cloned());
                let cli = Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
                let cfg = cli.config.unwrap_or_else(|| config_path.to_path_buf());
                run(&cli.command, &cfg)
            }
        }
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
        Command::Doctor { fix } => {
            let config = crate::config::load_config(config_path)?;
            if *fix {
                for line in crate::doctor::repairs(&config, config_path)? {
                    println!("fixed: {line}");
                }
                println!();
            }
            crate::doctor::doctor(&config)
        }
        Command::Crew {
            json,
            tail,
            dir,
            all,
        } => {
            let config = crate::config::load_config(config_path)?;
            let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
            let mesh = crate::identity::mesh_name_now(&config, Some(&me)).unwrap_or_default();
            let views =
                crate::session::collect(&config, &me, tail.unwrap_or(3) as usize, dir.as_deref());
            let shown: Vec<&crate::session::View> =
                views.iter().filter(|v| *all || v.status.fresh()).collect();
            let live_ids: Vec<String> = shown.iter().map(|v| v.id.clone()).collect();
            let queued: Vec<(String, usize)> = crate::session::pending_inboxes(&config)
                .into_iter()
                .filter(|(id, _)| !live_ids.iter().any(|l| l == id))
                .collect();
            if *json {
                let rows: Vec<Value> = shown
                    .iter()
                    .map(|v| {
                        let mut row = view_json(v);
                        row["pending_mail"] = json!(pending_of(&config, v));
                        row
                    })
                    .collect();
                let pending: Vec<Value> = queued
                    .iter()
                    .map(|(id, n)| json!({"id": id, "pending_mail": n}))
                    .collect();
                println!(
                    "{}",
                    json!({"self": me, "mesh": mesh, "sessions": rows, "pending": pending})
                );
                return Ok(());
            }
            println!(
                "{} live session(s) · you are {} ({})",
                shown.len(),
                if me.is_empty() { "?" } else { &me },
                if mesh.is_empty() { "unjoined" } else { &mesh }
            );
            for v in &shown {
                let mail = pending_of(&config, v);
                println!(
                    "• {} {} · {}{} · {}{}{} · pid {} · {}",
                    v.short,
                    v.status.label(),
                    match v.harness() {
                        Some(h) => format!("{h} · "),
                        None => String::new(),
                    },
                    v.short_model(),
                    v.cwd_short(),
                    if v.me { " · self" } else { "" },
                    if mail > 0 {
                        format!(" · {mail} mail")
                    } else {
                        String::new()
                    },
                    v.pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    ago(v.idle_secs)
                );
                if !v.claim.is_empty() {
                    println!("  claim: {}", v.claim);
                }
                let recap = v.recap();
                if !recap.is_empty() {
                    println!("  {recap}");
                }
                for l in &v.tail {
                    println!("  {} {} {}", l.clock, l.glyph.ch(), l.text);
                }
            }
            for (id, n) in queued.iter().take(5) {
                println!(
                    "• {id} not live · {n} queued message(s) · delivered when that session resumes"
                );
            }
            if queued.len() > 5 {
                println!(
                    "…and {} more session(s) holding queued mail",
                    queued.len() - 5
                );
            }
            Ok(())
        }
        Command::Send {
            text,
            to,
            all,
            kickstart,
        } => {
            let config = crate::config::load_config(config_path)?;
            let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
            let mesh = crate::identity::mesh_name_now(&config, Some(&me))?;
            let args = json!({
                "to": to,
                "text": text,
                "all": all,
                "mode": if *kickstart { "kickstart" } else { "message" },
            });
            println!(
                "{}",
                crate::sidecar::send_tool(&config, &Some(me), &mesh, &args)?
            );
            Ok(())
        }
        Command::Hook { harness } => {
            let config = crate::config::load_config(config_path)?;
            println!("{}", crate::harness::hook(&config, harness)?);
            Ok(())
        }
        Command::InstallHook { harness, uninstall } => {
            let config = crate::config::load_config(config_path)?;
            crate::harness::install(&config, harness.as_deref(), !*uninstall)
        }
        Command::Claim {
            text,
            clear,
            session,
        } => {
            let config = crate::config::load_config(config_path)?;
            let sid = session
                .clone()
                .or_else(|| crate::identity::session_id(&Value::Null))
                .unwrap_or_default();
            let args = json!({
                "text": text,
                "action": if *clear { "clear" } else { "set" },
            });
            println!(
                "{}",
                crate::sidecar::status_tool(&config, &Some(sid), &args)?
            );
            Ok(())
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
