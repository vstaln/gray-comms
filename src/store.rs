//! Durable SQLite state: agents, rooms, memberships, and a bounded message
//! log. DM rows carry `delivered`; undelivered rows are the offline queue a
//! joining agent drains (or a `peek unread` read settles).
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agents (
    name TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'active',
    joined REAL NOT NULL, seen REAL NOT NULL);
CREATE TABLE IF NOT EXISTS rooms (
    name TEXT PRIMARY KEY, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS members (
    room TEXT NOT NULL, agent TEXT NOT NULL,
    PRIMARY KEY (room, agent));
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, room TEXT,
    from_agent TEXT NOT NULL, to_agent TEXT,
    content TEXT NOT NULL, ts REAL NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS messages_room ON messages(room, ts);
CREATE INDEX IF NOT EXISTS messages_dm ON messages(to_agent, delivered);
";

pub struct Store {
    conn: Mutex<Connection>,
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

pub fn uuid_hex() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    } else {
        // Fallback: time + pid entropy — ids only need uniqueness.
        let nanos = now_secs().to_bits();
        bytes[..8].copy_from_slice(&nanos.to_le_bytes());
        bytes[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 3339 timestamp for the wire (agent-comms' `timestamp` shape).
pub fn iso(ts: f64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, ((ts.fract()) * 1e9) as u32)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn map_err(e: rusqlite::Error) -> String {
    format!("store error: {e}")
}

fn msg_json(kind: &str, id: String, room: Option<String>, from: String, to: Option<String>, content: String, ts: f64) -> Value {
    let mut m = json!({
        "id": id,
        "from": from,
        "content": content,
        "timestamp": iso(ts),
        "readBy": [],
    });
    if kind == "dm" {
        m["to"] = Value::String(to.unwrap_or_default());
    } else {
        m["room"] = Value::String(room.unwrap_or_default());
    }
    m
}

impl Store {
    pub fn new(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "cannot create state dir".to_string())?;
        }
        let conn = Connection::open(path).map_err(map_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(map_err)?;
        conn.execute_batch(SCHEMA).map_err(map_err)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store for tests.
    pub fn memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(map_err)?;
        conn.execute_batch(SCHEMA).map_err(map_err)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// Default location beside the config file.
    pub fn default_path(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("comms.sqlite")
    }

    /// Register or refresh an agent; returns its stored status.
    pub fn upsert_agent(&self, name: &str, status: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let now = now_secs();
        conn.execute(
            "INSERT INTO agents (name, status, joined, seen) VALUES (?1, 'active', ?2, ?2)
             ON CONFLICT(name) DO UPDATE SET seen=?2",
            rusqlite::params![name, now],
        )
        .map_err(map_err)?;
        if let Some(s) = status {
            conn.execute(
                "UPDATE agents SET status=?2 WHERE name=?1",
                rusqlite::params![name, s],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    pub fn set_status(&self, name: &str, status: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.execute(
            "UPDATE agents SET status=?2, seen=?3 WHERE name=?1",
            rusqlite::params![name, status, now_secs()],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn agent_exists(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.query_row(
            "SELECT COUNT(*) FROM agents WHERE name=?1",
            rusqlite::params![name],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(map_err)
    }

    /// Every known agent: `{name, status, rooms}` (presence overlay added by
    /// the daemon, which owns the live-connection map).
    pub fn agents(&self) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare("SELECT name, status FROM agents ORDER BY name")
            .map_err(map_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (name, status) = row;
            out.push(json!({"id": name, "name": name, "status": status}));
        }
        Ok(out)
    }

    pub fn agent_rooms(&self, name: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare("SELECT room FROM members WHERE agent=?1 ORDER BY room")
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![name], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        Ok(rows.flatten().collect())
    }

    /// Join (creating the room on first use); returns false if already in.
    pub fn join_room(&self, room: &str, agent: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO rooms (name, created) VALUES (?1, ?2)",
            rusqlite::params![room, now_secs()],
        )
        .map_err(map_err)?;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO members (room, agent) VALUES (?1, ?2)",
                rusqlite::params![room, agent],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    pub fn leave_room(&self, room: &str, agent: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.execute(
            "DELETE FROM members WHERE room=?1 AND agent=?2",
            rusqlite::params![room, agent],
        )
        .map(|n| n > 0)
        .map_err(map_err)
    }

    pub fn leave_all(&self, agent: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare("SELECT room FROM members WHERE agent=?1")
            .map_err(map_err)?;
        let rooms: Vec<String> = st
            .query_map(rusqlite::params![agent], |r| r.get(0))
            .map_err(map_err)?
            .flatten()
            .collect();
        drop(st);
        conn.execute("DELETE FROM members WHERE agent=?1", rusqlite::params![agent])
            .map_err(map_err)?;
        Ok(rooms)
    }

    pub fn room_members(&self, room: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare("SELECT agent FROM members WHERE room=?1 ORDER BY agent")
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![room], |r| r.get(0))
            .map_err(map_err)?;
        Ok(rows.flatten().collect())
    }

    pub fn is_member(&self, room: &str, agent: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.query_row(
            "SELECT COUNT(*) FROM members WHERE room=?1 AND agent=?2",
            rusqlite::params![room, agent],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(map_err)
    }

    pub fn room_exists(&self, room: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.query_row(
            "SELECT COUNT(*) FROM rooms WHERE name=?1",
            rusqlite::params![room],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(map_err)
    }

    /// Every room with its member list.
    pub fn rooms(&self) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare("SELECT name FROM rooms ORDER BY name")
            .map_err(map_err)?;
        let names: Vec<String> = st
            .query_map([], |r| r.get(0))
            .map_err(map_err)?
            .flatten()
            .collect();
        drop(st);
        let mut out = Vec::new();
        for name in names {
            let mut st = conn
                .prepare("SELECT agent FROM members WHERE room=?1 ORDER BY agent")
                .map_err(map_err)?;
            let members: Vec<String> = st
                .query_map(rusqlite::params![&name], |r| r.get(0))
                .map_err(map_err)?
                .flatten()
                .collect();
            out.push(json!({"name": name, "members": members}));
        }
        Ok(out)
    }

    /// Append a room message; returns the wire-shaped message object.
    pub fn add_room_message(
        &self,
        room: &str,
        from: &str,
        content: &str,
        history_limit: u64,
    ) -> Result<Value, String> {
        let id = self.insert("room", Some(room), from, None, content, true)?;
        self.prune_room(room, history_limit)?;
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let (ts,): (f64,) = conn
            .query_row(
                "SELECT ts FROM messages WHERE id=?1",
                rusqlite::params![&id],
                |r| Ok((r.get(0)?,)),
            )
            .map_err(map_err)?;
        Ok(msg_json("room", id, Some(room.to_string()), from.to_string(), None, content.to_string(), ts))
    }

    /// Append a DM; `delivered=false` keeps it in the recipient's queue.
    pub fn add_dm(&self, from: &str, to: &str, content: &str, delivered: bool) -> Result<Value, String> {
        let id = self.insert("dm", None, from, Some(to), content, delivered)?;
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let (ts,): (f64,) = conn
            .query_row(
                "SELECT ts FROM messages WHERE id=?1",
                rusqlite::params![&id],
                |r| Ok((r.get(0)?,)),
            )
            .map_err(map_err)?;
        Ok(msg_json("dm", id, None, from.to_string(), Some(to.to_string()), content.to_string(), ts))
    }

    fn insert(
        &self,
        kind: &str,
        room: Option<&str>,
        from: &str,
        to: Option<&str>,
        content: &str,
        delivered: bool,
    ) -> Result<String, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let id = uuid_hex();
        conn.execute(
            "INSERT INTO messages (id,kind,room,from_agent,to_agent,content,ts,delivered)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![id, kind, room, from, to, content, now_secs(), delivered as i64],
        )
        .map_err(map_err)?;
        Ok(id)
    }

    /// Undelivered DMs for `agent`, oldest first.
    pub fn undelivered_dms(&self, agent: &str) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare(
                "SELECT id, from_agent, content, ts FROM messages
                 WHERE kind='dm' AND to_agent=?1 AND delivered=0 ORDER BY ts",
            )
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![agent], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .map_err(map_err)?;
        Ok(rows
            .flatten()
            .map(|(id, from, content, ts)| {
                msg_json("dm", id, None, from, Some(agent.to_string()), content, ts)
            })
            .collect())
    }

    /// Mark a set of message ids delivered (queue drain).
    pub fn mark_delivered(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        for id in ids {
            conn.execute(
                "UPDATE messages SET delivered=1 WHERE id=?1",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Last `limit` messages of a room, oldest first.
    pub fn room_history(&self, room: &str, limit: usize) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare(
                "SELECT id, from_agent, content, ts FROM messages
                 WHERE kind='room' AND room=?1 ORDER BY ts DESC, id DESC LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![room, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .map_err(map_err)?;
        let mut out: Vec<Value> = rows
            .flatten()
            .map(|(id, from, content, ts)| {
                msg_json("room", id, Some(room.to_string()), from, None, content, ts)
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    /// Last `limit` DMs between two agents (either direction), oldest first.
    pub fn dm_thread(&self, a: &str, b: &str, limit: usize) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare(
                "SELECT id, from_agent, to_agent, content, ts FROM messages
                 WHERE kind='dm' AND ((from_agent=?1 AND to_agent=?2) OR (from_agent=?2 AND to_agent=?1))
                 ORDER BY ts DESC, id DESC LIMIT ?3",
            )
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![a, b, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })
            .map_err(map_err)?;
        let mut out: Vec<Value> = rows
            .flatten()
            .map(|(id, from, to, content, ts)| msg_json("dm", id, None, from, Some(to), content, ts))
            .collect();
        out.reverse();
        Ok(out)
    }

    /// DMs sent to `agent` (any state), oldest first.
    pub fn dm_inbox(&self, agent: &str, limit: usize) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        let mut st = conn
            .prepare(
                "SELECT id, from_agent, content, ts FROM messages
                 WHERE kind='dm' AND to_agent=?1 ORDER BY ts DESC, id DESC LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = st
            .query_map(rusqlite::params![agent, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .map_err(map_err)?;
        let mut out: Vec<Value> = rows
            .flatten()
            .map(|(id, from, content, ts)| {
                msg_json("dm", id, None, from, Some(agent.to_string()), content, ts)
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    /// Keep only the newest `keep` room messages.
    fn prune_room(&self, room: &str, keep: u64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "store locked".to_string())?;
        conn.execute(
            "DELETE FROM messages WHERE kind='room' AND room=?1 AND id NOT IN (
                SELECT id FROM messages WHERE kind='room' AND room=?1
                ORDER BY ts DESC, id DESC LIMIT ?2)",
            rusqlite::params![room, keep.max(1) as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }
}
