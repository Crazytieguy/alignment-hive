//! The server under test as a separate process, spoken to as an MCP client
//! over stdio. Sync on purpose: tests read top to bottom, and "close the
//! laptop" is a signal to a process group.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::providers::{runpod_key, scrub, vast_key};

/// The binary under test: the crate's own build unless `RK_LIVE_BINARY` names
/// another (so the same scenarios run against an older build).
pub fn binary_under_test() -> PathBuf {
    std::env::var_os("RK_LIVE_BINARY").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_remote-kernels")),
        PathBuf::from,
    )
}

/// A finished tool call, as the agent would see it.
#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub is_error: bool,
    pub secs: f64,
}

impl Reply {
    pub fn ok(&self) -> bool {
        !self.is_error
    }

    pub fn mentions(&self, needle: &str) -> bool {
        self.text.to_lowercase().contains(&needle.to_lowercase())
    }
}

type Waiters = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;

/// A tool call in flight.
pub struct Pending {
    rx: mpsc::Receiver<Value>,
    sent: Instant,
    done: Option<Reply>,
}

impl Pending {
    /// `None` on timeout; call again to keep waiting.
    pub fn wait(&mut self, timeout: Duration) -> Option<Reply> {
        if let Some(reply) = &self.done {
            return Some(reply.clone());
        }
        match self.rx.recv_timeout(timeout) {
            Ok(message) => {
                let reply = render(&message, self.sent.elapsed().as_secs_f64());
                self.done = Some(reply.clone());
                Some(reply)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let reply = Reply {
                    text: "the server exited before replying".to_string(),
                    is_error: true,
                    secs: self.sent.elapsed().as_secs_f64(),
                };
                self.done = Some(reply.clone());
                Some(reply)
            }
        }
    }

    pub fn replied(&mut self) -> bool {
        self.wait(Duration::ZERO).is_some()
    }
}

fn render(message: &Value, secs: f64) -> Reply {
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| error.to_string(), String::from);
        return Reply {
            text: scrub(&text),
            is_error: true,
            secs,
        };
    }
    let result = message.get("result").cloned().unwrap_or(Value::Null);
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Reply {
        text: scrub(&text),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        secs,
    }
}

/// How to launch one server process.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub binary: Option<PathBuf>,
    /// Prepended to PATH: L7's failing `ssh` shim lives here.
    pub path_prefix: Option<PathBuf>,
    /// Extra environment, e.g. `HTTPS_PROXY` or `REMOTE_KERNELS_BUDGET`.
    pub env: Vec<(String, String)>,
}

/// One running server.
pub struct Server {
    tag: String,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    waiters: Waiters,
    next_id: AtomicU64,
    log: Arc<Mutex<std::fs::File>>,
    t0: Instant,
    /// Seconds from spawn to the `initialize` reply.
    pub handshake_secs: f64,
}

impl Server {
    /// Spawn and complete the MCP handshake.
    pub fn spawn(
        project: &Path,
        log_dir: &Path,
        keys_root: &Path,
        tag: &str,
        session: &str,
        launch: &Launch,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(log_dir)?;
        let log = Arc::new(Mutex::new(std::fs::File::create(
            log_dir.join(format!("{tag}.mcp.log")),
        )?));
        let stderr_log = std::fs::File::create(log_dir.join(format!("{tag}.stderr.log")))?;
        let binary = launch.binary.clone().unwrap_or_else(binary_under_test);

        let mut path = std::env::var("PATH").unwrap_or_default();
        if let Some(prefix) = &launch.path_prefix {
            path = format!("{}:{path}", prefix.display());
        }
        let mut command = Command::new(&binary);
        command
            .arg("--project-dir")
            .arg(project)
            .current_dir(project)
            .env_clear()
            .env("PATH", path)
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("RUST_LOG", "remote_kernels=info,rmcp=warn")
            .env("REMOTE_KERNELS_KEYS_ROOT", keys_root)
            .env("CLAUDE_CODE_SESSION_ID", session)
            .env("RUNPOD_API_KEY", runpod_key())
            .env("VAST_API_KEY", vast_key())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for (key, value) in &launch.env {
            command.env(key, value);
        }

        let t0 = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning {}: {e}", binary.display()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let waiters: Waiters = Arc::default();
        {
            let waiters = Arc::clone(&waiters);
            let log = Arc::clone(&log);
            std::thread::spawn(move || read_stdout(stdout, &waiters, &log, t0));
        }
        std::thread::spawn(move || copy_stderr(stderr, stderr_log, t0));

        let mut server = Self {
            tag: tag.to_string(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            waiters,
            next_id: AtomicU64::new(1),
            log,
            t0,
            handshake_secs: 0.0,
        };
        server.handshake()?;
        Ok(server)
    }

    fn handshake(&mut self) -> anyhow::Result<()> {
        let (id, rx) = self.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "rk-live", "version": "1"},
            }),
        )?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|e| anyhow::anyhow!("no initialize reply (request {id}): {e}"))?;
        self.handshake_secs = self.t0.elapsed().as_secs_f64();
        self.notify("notifications/initialized", &json!({}))?;
        Ok(())
    }

    fn write_line(&self, line: &str) -> anyhow::Result<()> {
        self.log_line(">>", line);
        let mut guard = self.stdin.lock().expect("stdin lock");
        let stdin = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("stdin is closed"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn log_line(&self, direction: &str, line: &str) {
        let at = self.t0.elapsed().as_secs_f64();
        let _ = writeln!(
            self.log.lock().expect("log lock"),
            "[{at:8.1}s] {direction} {}",
            scrub(line)
        );
    }

    fn request(
        &self,
        method: &str,
        params: &Value,
    ) -> anyhow::Result<(u64, mpsc::Receiver<Value>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.waiters.lock().expect("waiters lock").insert(id, tx);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&message.to_string())?;
        Ok((id, rx))
    }

    fn notify(&self, method: &str, params: &Value) -> anyhow::Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_line(&message.to_string())
    }

    /// Start a tool call without waiting for it.
    pub fn call_async(&self, tool: &str, args: &Value) -> Pending {
        let sent = Instant::now();
        let (_, rx) = self
            .request("tools/call", &json!({"name": tool, "arguments": args}))
            .unwrap_or_else(|e| {
                let (tx, rx) = mpsc::channel();
                drop(tx);
                eprintln!("[{}] could not send {tool}: {e}", self.tag);
                (0, rx)
            });
        Pending {
            rx,
            sent,
            done: None,
        }
    }

    /// A tool call that must answer within `timeout`; a silent server comes
    /// back as an error reply naming the timeout, so a hang is a finding and
    /// not a stuck test.
    pub fn call(&self, tool: &str, args: &Value, timeout: Duration) -> Reply {
        let mut pending = self.call_async(tool, args);
        pending.wait(timeout).unwrap_or_else(|| Reply {
            text: format!("no reply from {tool} within {}s", timeout.as_secs()),
            is_error: true,
            secs: timeout.as_secs_f64(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.lock().expect("child lock").id()
    }

    /// Close stdin: the disconnect a client's exit produces.
    pub fn disconnect(&self) {
        *self.stdin.lock().expect("stdin lock") = None;
    }

    /// Whether the process has exited.
    pub fn exited(&self) -> bool {
        matches!(
            self.child.lock().expect("child lock").try_wait(),
            Ok(Some(_))
        )
    }

    fn signal_group(&self, signal: &str) {
        let target = format!("-{}", self.pid());
        let _ = Command::new("kill")
            .args([format!("-{signal}"), target])
            .stderr(Stdio::null())
            .status();
    }

    /// Close the laptop: freeze the server and its ssh children together.
    pub fn sigstop(&self) {
        self.signal_group("STOP");
    }

    /// Open the laptop.
    pub fn sigcont(&self) {
        self.signal_group("CONT");
    }

    /// `kill -9` the whole process group, tunnels included. Safe to repeat.
    pub fn kill9(&self) {
        self.signal_group("KILL");
        let mut child = self.child.lock().expect("child lock");
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        *self.stdin.lock().expect("stdin lock") = None;
    }

    /// Kill the server's established ssh connections, leaving the server up.
    pub fn kill_ssh_children(&self) {
        let _ = Command::new("pkill")
            .args(["-9", "-g", &self.pid().to_string(), "^ssh$"])
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill9();
    }
}

fn read_stdout(
    stdout: std::process::ChildStdout,
    waiters: &Waiters,
    log: &Mutex<std::fs::File>,
    t0: Instant,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let at = t0.elapsed().as_secs_f64();
        let _ = writeln!(
            log.lock().expect("log lock"),
            "[{at:8.1}s] << {}",
            scrub(&line)
        );
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64)
            && message.get("method").is_none()
            && let Some(tx) = waiters.lock().expect("waiters lock").remove(&id)
        {
            let _ = tx.send(message);
        }
    }
    // Dropping every sender tells callers still waiting that the server is gone.
    waiters.lock().expect("waiters lock").clear();
}

fn copy_stderr(stderr: std::process::ChildStderr, mut log: std::fs::File, t0: Instant) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        let at = t0.elapsed().as_secs_f64();
        let _ = writeln!(log, "[{at:8.1}s] {}", scrub(&line));
    }
}
