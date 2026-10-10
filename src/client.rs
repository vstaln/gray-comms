//! Blocking NDJSON client for the daemon. One TCP connection per call site
//! (sidecar tools, CLI verbs): join, run the ops, drop. Live event streams
//! (`listen`) share the same reader path.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::proto;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// How hard a client pushes before giving up. The daemon is local, so this
/// is short — a down daemon must fail fast, not hang a tool call.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    /// Lines pushed by the daemon that were not a reply (`notify` events).
    pub events: Vec<Value>,
    /// The name this connection joined as (empty until joined).
    pub me: String,
    /// Rooms this agent belongs to, per the last join reply.
    pub rooms: Vec<String>,
}

fn stream_clone(s: &TcpStream) -> Result<TcpStream, String> {
    s.try_clone().map_err(|_| "connection unusable".to_string())
}

impl Client {
    /// Connect to the address the config points at.
    pub fn connect(config: &Value) -> Result<Self, String> {
        let addr = crate::config::connect_addr(config);
        Self::connect_addr(&addr, config)
    }

    /// Connect to an explicit `host:port` (tests, ad-hoc clients).
    pub fn connect_addr(addr: &str, config: &Value) -> Result<Self, String> {
        let sock_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|_| format!("bad daemon address {addr}"))?;
        let stream = TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT)
            .map_err(|_| "daemon not reachable (is `gray-comms run` up?)".to_string())?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_nodelay(true);
        let mut c = Client {
            reader: BufReader::new(stream_clone(&stream)?),
            writer: stream,
            events: Vec::new(),
            me: String::new(),
            rooms: Vec::new(),
        };
        // Identity: configured agent name + optional LAN token.
        let name = config
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return Err("no agent name configured; run setup".to_string());
        }
        let mut params = json!({"name": name});
        if let Some(t) = config.get("token").and_then(Value::as_str) {
            if !t.is_empty() {
                params["auth"] = Value::String(t.to_string());
            }
        }
        let res = c.call("join", params)?;
        c.me = name;
        c.rooms = res
            .get("rooms")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        Ok(c)
    }

    /// Re-join with `listen:true` so pushes stream to `events`/the socket.
    pub fn listen(&mut self) -> Result<Value, String> {
        let res = self.call("join", json!({"name": self.me, "listen": true}))?;
        Ok(res)
    }

    /// One request/response round-trip. Interleaved `notify` lines are
    /// collected into `self.events` rather than confused for the reply.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let req = json!({"id": id, "method": method, "params": params});
        self.writer
            .write_all(proto::encode(&req).as_bytes())
            .map_err(|_| "daemon connection lost".to_string())?;
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|_| "daemon connection lost or timed out".to_string())?;
            if n == 0 {
                return Err("daemon closed the connection".to_string());
            }
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("failed");
                    return Err(format!("{code}: {msg}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // Not our reply: a live push event.
            self.events.push(v);
        }
    }

    /// Read one pushed event (blocking until IO_TIMEOUT). `Err` on timeout
    /// or close; callers that stream just loop until Err.
    pub fn next_event(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|_| "connection closed or timed out".to_string())?;
        if n == 0 {
            return Err("daemon closed the connection".to_string());
        }
        serde_json::from_str(line.trim()).map_err(|_| "bad frame".to_string())
    }
}
