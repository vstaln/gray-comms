use crate::session::View;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::theme::{Palette, Role};

const TAIL: usize = 400;
const STATUS_SECS: u64 = 3;
const QUIET_SECS: u64 = 10 * 60;
const DEFAULT_WINDOW: u64 = 200_000;
const SPAWN_REFRESH: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    Compose,
    Mail,
    Rename,
    Filter,
}

pub enum Row {
    Folder(String),
    Session { view: usize, depth: u8 },
}

pub struct Status {
    pub text: String,
    pub ok: bool,
    pub at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Warn,
    Calm,
}

pub struct Badge {
    pub glyph: &'static str,
    pub text: String,
    pub tone: Tone,
}

pub struct App {
    pub config_path: PathBuf,
    pub config: Value,
    pub me: String,
    pub mesh: String,
    pub views: Vec<View>,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub mode: Mode,
    pub input: String,
    pub filter: String,
    pub status: Option<Status>,
    pub scroll_back: usize,
    pub mail: Vec<crate::mailog::Entry>,
    pub mail_scroll: usize,
    pub nicks: HashMap<String, String>,
    pub spawns: HashMap<String, crate::spawns::Spawn>,
    pub window: u64,
    windows: HashMap<String, u64>,
    pub palette: Palette,
    pub quit: bool,
    pub pending: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    sent_to: String,
    last_spawns: Instant,
}

impl App {
    pub fn new(config_path: &Path, config: Value) -> App {
        let me = crate::identity::session_id(&Value::Null).unwrap_or_default();
        let mesh = crate::identity::mesh_name_now(&config, Some(&me)).unwrap_or_default();
        let window = gray_window(&config);
        let windows = load_windows(&config);
        let mut app = App {
            config_path: config_path.to_path_buf(),
            config,
            me,
            mesh,
            views: Vec::new(),
            rows: Vec::new(),
            sel: 0,
            mode: Mode::List,
            input: String::new(),
            filter: String::new(),
            status: None,
            scroll_back: 0,
            mail: Vec::new(),
            mail_scroll: 0,
            nicks: HashMap::new(),
            spawns: HashMap::new(),
            window,
            windows,
            palette: Palette::load(&Value::Null),
            quit: false,
            pending: None,
            sent_to: String::new(),
            last_spawns: Instant::now() - Duration::from_secs(SPAWN_REFRESH),
        };
        app.palette = Palette::load(&app.config);
        app.spawns = crate::spawns::load(&app.config);
        app.refresh();
        app
    }

    pub fn poll_pending(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(out)) => {
                let state = if out.contains("delivered") {
                    "delivered"
                } else if out.contains("queued") {
                    "queued"
                } else {
                    "sent"
                };
                let to = self.sent_to.clone();
                self.set_status(format!("{state} · {to}"), true);
                self.pending = None;
                false
            }
            Ok(Err(e)) => {
                self.set_status(format!("failed · {e}"), false);
                self.pending = None;
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.set_status("failed · send worker died".to_string(), false);
                self.pending = None;
                false
            }
        }
    }

    pub fn refresh(&mut self) {
        self.poll_pending();
        self.nicks = crate::nick::all(&self.config).into_iter().collect();
        if self.last_spawns.elapsed() >= Duration::from_secs(SPAWN_REFRESH) {
            crate::spawns::refresh(&self.config, &mut self.spawns, 64);
            self.last_spawns = Instant::now();
        }
        let keep = self.selected().map(|v| v.id.clone());
        self.views = crate::session::collect(&self.config, &self.me, TAIL, None);
        self.build_rows();
        if let Some(id) = keep {
            if let Some(i) = self.selectable_ids().iter().position(|v| *v == id) {
                self.sel = i;
            }
        }
        if self.sel >= self.selectable_count() {
            self.sel = self.selectable_count().saturating_sub(1);
        }
        if self.mode == Mode::Mail {
            self.load_mail();
        }
        let expired = self
            .status
            .as_ref()
            .is_some_and(|s| s.at.elapsed() >= Duration::from_secs(STATUS_SECS));
        if expired {
            self.status = None;
        }
    }

    pub fn load_mail_now(&mut self) {
        self.load_mail();
    }

    pub fn apply_filter(&mut self) {
        self.filter = self.input.clone();
        self.build_rows();
        self.sel = 0;
        self.scroll_back = 0;
    }

    fn load_mail(&mut self) {
        self.mail = crate::mailog::recent(&self.config, 200);
    }

    fn build_rows(&mut self) {
        let filter = self.filter.to_lowercase();
        let mut keep: Vec<usize> = Vec::new();
        for (i, v) in self.views.iter().enumerate() {
            if filter.is_empty() {
                keep.push(i);
                continue;
            }
            let hay = format!(
                "{} {} {} {} {} {}",
                v.id,
                v.short,
                self.label(v),
                v.cwd_short(),
                v.short_model(),
                v.origin
            )
            .to_lowercase();
            if hay.contains(&filter) {
                keep.push(i);
            }
        }
        let index_of: HashMap<&str, usize> = keep
            .iter()
            .map(|i| (self.views[*i].id.as_str(), *i))
            .collect();
        let mut parent: HashMap<usize, usize> = HashMap::new();
        for i in &keep {
            let Some(spawn) = self.spawns.get(&self.views[*i].id) else {
                continue;
            };
            if let Some(p) = index_of.get(spawn.parent.as_str()) {
                if p != i {
                    parent.insert(*i, *p);
                }
            }
        }
        let mut folder_of: HashMap<usize, String> = HashMap::new();
        for i in &keep {
            let v = &self.views[*i];
            let folder = if v.harness().is_some() {
                "other apps".to_string()
            } else {
                v.cwd_short()
            };
            folder_of.insert(*i, folder);
        }
        for i in &keep {
            if let Some(p) = parent.get(i) {
                if let Some(up) = parent.get(p) {
                    parent.insert(*i, *up);
                }
            }
        }
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut slot: HashMap<String, usize> = HashMap::new();
        for i in &keep {
            let anchor = parent.get(i).copied().unwrap_or(*i);
            let folder = folder_of.get(&anchor).cloned().unwrap_or_default();
            let at = match slot.get(&folder) {
                Some(at) => *at,
                None => {
                    groups.push((folder.clone(), Vec::new()));
                    let at = groups.len() - 1;
                    slot.insert(folder, at);
                    at
                }
            };
            groups[at].1.push(*i);
        }
        let me = self.me.clone();
        let views = &self.views;
        let holds_me = |g: &(String, Vec<usize>)| g.1.iter().any(|i| views[*i].id == me);
        let fresh = |g: &(String, Vec<usize>)| {
            g.1.iter()
                .map(|i| views[*i].last_ms)
                .max()
                .unwrap_or_default()
        };
        let mut order: Vec<usize> = (0..groups.len()).collect();
        let foreign = |g: &(String, Vec<usize>)| g.1.iter().any(|i| views[*i].harness().is_some());
        order.sort_by(|a, b| {
            foreign(&groups[*a])
                .cmp(&foreign(&groups[*b]))
                .then(holds_me(&groups[*b]).cmp(&holds_me(&groups[*a])))
                .then(fresh(&groups[*b]).cmp(&fresh(&groups[*a])))
        });
        let mut rows: Vec<Row> = Vec::new();
        for g in order {
            let (folder, members) = &groups[g];
            rows.push(Row::Folder(folder.clone()));
            let mut roots: Vec<usize> = members
                .iter()
                .copied()
                .filter(|i| !parent.contains_key(i))
                .collect();
            roots.sort_by_key(|i| std::cmp::Reverse(views[*i].last_ms));
            for i in roots {
                rows.push(Row::Session { view: i, depth: 0 });
                let mut kids: Vec<usize> = members
                    .iter()
                    .copied()
                    .filter(|c| parent.get(c) == Some(&i))
                    .collect();
                kids.sort_by_key(|c| std::cmp::Reverse(views[*c].last_ms));
                for c in kids {
                    rows.push(Row::Session { view: c, depth: 1 });
                }
            }
        }
        self.rows = rows;
    }

    pub fn selectable_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, Row::Session { .. }))
            .count()
    }

    pub fn selectable_ids(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Session { view, .. } => Some(self.views[*view].id.clone()),
                Row::Folder(_) => None,
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&View> {
        let mut n = 0;
        for r in &self.rows {
            if let Row::Session { view, .. } = r {
                if n == self.sel {
                    return Some(&self.views[*view]);
                }
                n += 1;
            }
        }
        None
    }

    pub fn move_sel(&mut self, down: bool) {
        let n = self.selectable_count();
        if n == 0 {
            return;
        }
        self.sel = if down {
            (self.sel + 1).min(n - 1)
        } else {
            self.sel.saturating_sub(1)
        };
        self.scroll_back = 0;
    }

    pub fn label(&self, v: &View) -> String {
        if let Some(nick) = self.nicks.get(&v.id).filter(|n| !n.trim().is_empty()) {
            return nick.clone();
        }
        if let Some(title) = crate::session::title(&self.config, &v.id) {
            return summary(&title);
        }
        if let Some(spawn) = self.spawns.get(&v.id) {
            if !spawn.name.is_empty() {
                return spawn.name.clone();
            }
        }
        if !v.run.is_empty() {
            return v.run.clone();
        }
        if v.harness().is_some() {
            return match v.origin.as_str() {
                "claude" => "Claude Code".to_string(),
                "codex" => "Codex".to_string(),
                "opencode" => "OpenCode".to_string(),
                other => other.to_string(),
            };
        }
        summary(&v.want)
    }

    pub fn badge(&self, v: &View) -> Badge {
        if let Some(reason) = retry_reason(v) {
            return Badge {
                glyph: "⚠",
                text: reason,
                tone: Tone::Warn,
            };
        }
        if matches!(v.status, crate::session::Status::Busy) && v.idle_secs > QUIET_SECS {
            return Badge {
                glyph: "⚠",
                text: format!("quiet {}", short_dur(v.idle_secs)),
                tone: Tone::Warn,
            };
        }
        let pct = v.ctx_used * 100 / self.window_for(v).max(1);
        if pct >= 90 && pct <= 200 {
            return Badge {
                glyph: "⚠",
                text: format!("ctx {pct}%"),
                tone: Tone::Warn,
            };
        }
        match v.status {
            crate::session::Status::Busy => Badge {
                glyph: "●",
                text: "busy".to_string(),
                tone: Tone::Good,
            },
            crate::session::Status::Idle | crate::session::Status::Stale => Badge {
                glyph: "○",
                text: "idle".to_string(),
                tone: Tone::Calm,
            },
        }
    }

    pub fn window_for(&self, v: &View) -> u64 {
        if let Some(n) = self.windows.get(&v.model) {
            return *n;
        }
        if let Some(n) = self.windows.get(v.short_model().as_str()) {
            return *n;
        }
        self.window
    }

    pub fn tone_color(&self, tone: Tone) -> ratatui::style::Color {
        self.palette.get(match tone {
            Tone::Good => Role::Success,
            Tone::Bad => Role::Error,
            Tone::Warn => Role::AccentSoft,
            Tone::Calm => Role::Dim,
        })
    }

    pub fn set_status(&mut self, text: String, ok: bool) {
        self.status = Some(Status {
            text,
            ok,
            at: Instant::now(),
        });
    }

    pub fn send(&mut self, kickstart: bool) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.pending.is_some() {
            self.set_status("still sending the previous message".to_string(), false);
            return;
        }
        let Some(v) = self.selected() else {
            self.set_status("no session selected".to_string(), false);
            return;
        };
        let id = v.id.clone();
        let short = v.short.clone();
        let args = serde_json::json!({
            "to": id,
            "text": text,
            "mode": if kickstart { "kickstart" } else { "message" },
        });
        let session = Some(self.me.clone());
        let mesh = self.mesh.clone();
        let config = self.config.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = crate::sidecar::send_tool(&config, &session, &mesh, &args);
            let _ = tx.send(out);
        });
        self.pending = Some(rx);
        self.sent_to = short.clone();
        self.input.clear();
        self.mode = Mode::List;
        self.set_status(format!("sending to {short}…"), true);
    }

    pub fn save_nick(&mut self, name: &str) {
        let Some(v) = self.selected() else {
            self.set_status("no session selected".to_string(), false);
            return;
        };
        let id = v.id.clone();
        let label = self.label(v);
        match crate::nick::set(&self.config, &id, name) {
            Ok(()) => {
                let name = name.trim();
                self.nicks = crate::nick::all(&self.config).into_iter().collect();
                self.set_status(
                    if name.is_empty() {
                        format!("cleared nickname for {label}")
                    } else {
                        format!("named {id} {name}")
                    },
                    true,
                );
            }
            Err(e) => self.set_status(format!("rename failed · {e}"), false),
        }
    }

    pub fn open_selected(&mut self) {
        let Some(v) = self.selected() else {
            return;
        };
        let id = v.id.clone();
        let cwd = v.cwd.clone();
        let label = self.label(v);
        let bin = self
            .config
            .get("gray_bin")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("gray")
            .to_string();
        let short = v.short.clone();
        let _ = label;
        match spawn_terminal(&bin, &id, &cwd) {
            Ok(emu) => self.set_status(format!("opened {short} in {emu}"), true),
            Err(e) => self.set_status(format!("open failed · {e}"), false),
        }
    }

    pub fn rename_start(&mut self) {
        let Some(v) = self.selected() else {
            return;
        };
        let id = v.id.clone();
        self.input = self.nicks.get(&id).cloned().unwrap_or_default();
        self.mode = Mode::Rename;
    }

    pub fn cursor_hint(&self) -> String {
        self.selected()
            .map(|v| v.short.clone())
            .unwrap_or_else(|| "nobody".to_string())
    }
}

fn summary(ask: &str) -> String {
    let stop = [
        "the", "a", "an", "is", "it", "to", "and", "of", "in", "on", "for", "can", "you", "please",
        "i", "we", "my", "me", "do", "does", "how", "what", "why", "there", "that", "this", "but",
        "so", "if", "with", "just", "now", "let", "lets", "let's", "ok", "okay", "hey", "go",
    ];
    let mut words: Vec<String> = Vec::new();
    for raw in ask.split_whitespace() {
        let w: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '/')
            .collect();
        if w.is_empty() {
            continue;
        }
        if words.is_empty() && stop.contains(&w.to_lowercase().as_str()) {
            continue;
        }
        words.push(w);
        if words.len() >= 4 {
            break;
        }
    }
    if words.is_empty() {
        return "no ask yet".to_string();
    }
    let mut out = words.join(" ");
    if out.chars().count() > 28 {
        out = out.chars().take(27).collect::<String>() + "…";
    }
    out
}

fn retry_reason(v: &View) -> Option<String> {
    let hay = v
        .tail
        .iter()
        .rev()
        .take(6)
        .map(|l| l.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "retrying",
        "request failed",
        "rate limit",
        "rate-limited",
        "overloaded",
        "provider retry",
    ] {
        if hay.contains(needle) {
            return Some("retrying".to_string());
        }
    }
    None
}

pub fn short_dur(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn load_windows(config: &Value) -> HashMap<String, u64> {
    let home = crate::session::home(config);
    std::fs::read_to_string(home.join("models.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_u64().filter(|n| *n > 0).map(|n| (k, n)))
                .collect()
        })
        .unwrap_or_default()
}

fn gray_window(config: &Value) -> u64 {
    let home = crate::session::home(config);
    std::fs::read_to_string(home.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("context_window").and_then(Value::as_u64))
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_WINDOW)
}

fn spawn_terminal(bin: &str, id: &str, cwd: &str) -> Result<String, String> {
    let emu = std::env::var("TERMINAL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            [
                "x-terminal-emulator",
                "gnome-terminal",
                "konsole",
                "alacritty",
                "kitty",
                "wezterm",
                "foot",
                "xterm",
            ]
            .into_iter()
            .find(|c| on_path(c))
            .map(str::to_string)
        })
        .ok_or_else(|| "no terminal emulator found (set $TERMINAL)".to_string())?;
    let name = std::path::Path::new(&emu)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| emu.clone());
    let mut cmd = std::process::Command::new(&emu);
    match name.as_str() {
        "gnome-terminal" => {
            cmd.arg("--");
        }
        "wezterm" => {
            cmd.arg("start").arg("--");
        }
        "x-terminal-emulator" | "konsole" | "alacritty" | "kitty" | "foot" | "xterm" => {
            cmd.arg("-e");
        }
        _ => {}
    }
    cmd.arg(bin).arg("resume").arg(id);
    if !cwd.is_empty() && std::path::Path::new(cwd).is_dir() {
        cmd.current_dir(cwd);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| format!("{emu}: {e}"))?;
    Ok(name)
}

fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(name);
        p.is_file()
    })
}
