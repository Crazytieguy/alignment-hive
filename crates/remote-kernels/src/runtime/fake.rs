//! Test backend: "machines" are local Jupyter server processes.
//!
//! Enables full-stack testing of the server (provision → kernel → execute →
//! sync → budget enforcement → cleanup) with zero cloud dependencies. The
//! Jupyter layer is exercised for real; only the machine provider is fake.
//!
//! Environment knobs:
//! - `REMOTE_KERNELS_FAKE_JUPYTER`: command to launch Jupyter (default
//!   `jupyter server`; whitespace-split, e.g. `uv run --with jupyter-server jupyter server`)
//! - `REMOTE_KERNELS_FAKE_COST_PER_HR`: pretend hourly cost (default 0.0) so
//!   budget enforcement can be tested against wall-clock accrual

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

struct FakeInstance {
    child: Option<tokio::process::Child>,
    port: u16,
    token: String,
    workdir: tempfile::TempDir,
    /// Last budget deadline (secs-from-now) pushed by the heartbeat, for test
    /// observation of the budget supervisor.
    last_budget_deadline: Arc<AtomicU64>,
}

pub struct FakeRuntime {
    instances: Arc<Mutex<HashMap<String, FakeInstance>>>,
    cost_per_hr: f64,
}

impl FakeRuntime {
    pub fn new() -> Self {
        let cost_per_hr = std::env::var("REMOTE_KERNELS_FAKE_COST_PER_HR")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        Self {
            instances: Arc::new(Mutex::new(HashMap::new())),
            cost_per_hr,
        }
    }

    fn jupyter_command() -> Vec<String> {
        std::env::var("REMOTE_KERNELS_FAKE_JUPYTER")
            .unwrap_or_else(|_| "jupyter server".to_string())
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    fn spawn_jupyter(
        port: u16,
        token: &str,
        workdir: &Path,
    ) -> anyhow::Result<tokio::process::Child> {
        let cmd_parts = Self::jupyter_command();
        let (program, args) = cmd_parts
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("REMOTE_KERNELS_FAKE_JUPYTER is empty"))?;

        let child = tokio::process::Command::new(program)
            .args(args)
            .arg("--no-browser")
            .arg("--ip=127.0.0.1")
            .arg(format!("--port={port}"))
            .arg("--port-retries=0")
            .arg(format!("--ServerApp.token={token}"))
            .arg("--ServerApp.disable_check_xsrf=True")
            .arg(format!("--ServerApp.root_dir={}", workdir.display()))
            // Kernels inherit the server's cwd; it must be the machine's
            // "workdir" so executed code sees synced files (like /workspace
            // on a real machine).
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        Ok(child)
    }

    fn free_port() -> anyhow::Result<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(listener.local_addr()?.port())
    }
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime for FakeRuntime {
    type Conn = FakeConnection;

    fn name(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stop_resume: StopSupport::Full,
            metered: self.cost_per_hr > 0.0,
        }
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        let external_id = format!("fake-{}", uuid::Uuid::new_v4());
        let port = Self::free_port()?;
        let workdir = tempfile::tempdir()?;
        let child = Self::spawn_jupyter(port, &req.jupyter_token, workdir.path())?;

        self.instances.lock().await.insert(
            external_id.clone(),
            FakeInstance {
                child: Some(child),
                port,
                token: req.jupyter_token.clone(),
                workdir,
                last_budget_deadline: Arc::new(AtomicU64::new(u64::MAX)),
            },
        );

        Ok(InstanceHandle {
            external_id,
            gpu_name: "Fake GPU".to_string(),
            cost_per_hr: Some(self.cost_per_hr),
        })
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let instances = self.instances.lock().await;
        anyhow::ensure!(instances.contains_key(external_id), "unknown fake instance");
        Ok(InstanceHandle {
            external_id: external_id.to_string(),
            gpu_name: "Fake GPU".to_string(),
            cost_per_hr: Some(self.cost_per_hr),
        })
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        let mut instances = self.instances.lock().await;
        match instances.get_mut(external_id) {
            Some(inst) => match &mut inst.child {
                Some(child) => match child.try_wait()? {
                    None => Ok(InstanceStatus::Running),
                    Some(_) => Ok(InstanceStatus::Stopped),
                },
                None => Ok(InstanceStatus::Stopped),
            },
            None => Ok(InstanceStatus::Gone),
        }
    }

    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        match self.describe(external_id).await? {
            InstanceStatus::Running => self.get_handle(external_id).await,
            other => anyhow::bail!("fake instance is {other:?}, not running"),
        }
    }

    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        let inst = instances
            .get_mut(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        if let Some(mut child) = inst.child.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        let inst = instances
            .get_mut(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        if inst.child.is_none() {
            inst.child = Some(Self::spawn_jupyter(
                inst.port,
                &inst.token,
                inst.workdir.path(),
            )?);
        }
        Ok(())
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        if let Some(mut inst) = self.instances.lock().await.remove(external_id)
            && let Some(mut child) = inst.child.take()
        {
            let _ = child.kill().await;
        }
        Ok(())
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<FakeConnection> {
        let instances = self.instances.lock().await;
        let inst = instances
            .get(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        Ok(FakeConnection {
            jupyter: JupyterEndpoint {
                http_base: format!("http://127.0.0.1:{}", inst.port),
                ws_base: format!("ws://127.0.0.1:{}", inst.port),
                token: ctx.jupyter_token.clone(),
            },
            workdir: inst.workdir.path().to_path_buf(),
            last_budget_deadline: Arc::clone(&inst.last_budget_deadline),
        })
    }
}

pub struct FakeConnection {
    jupyter: JupyterEndpoint,
    workdir: std::path::PathBuf,
    last_budget_deadline: Arc<AtomicU64>,
}

impl FakeConnection {
    /// For tests: the last deadline (secs-from-now) the heartbeat pushed.
    pub fn last_budget_deadline(&self) -> u64 {
        self.last_budget_deadline.load(Ordering::Relaxed)
    }
}

impl Connection for FakeConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        let output = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&self.workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("command timed out"))??;

        if !output.status.success() {
            anyhow::bail!(
                "command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn wait_reachable(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        let mut args = vec![
            "-az".to_string(),
            "--no-owner".to_string(),
            "--no-group".to_string(),
            "--delete".to_string(),
        ];
        for include in extra_includes {
            args.push(format!("--include={include}"));
        }
        args.extend([
            "--filter=:- .gitignore".to_string(),
            "--exclude=.git".to_string(),
            "--exclude=.claude".to_string(),
            format!("{}/", project_dir.display()),
            format!("{}/", self.workdir.display()),
        ]);

        let output = tokio::process::Command::new("rsync")
            .args(&args)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("rsync failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok("Files synced successfully.".to_string())
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let source = self.workdir.join(remote_path.trim_start_matches('/'));
        let output = tokio::process::Command::new("rsync")
            .args([
                "-az".to_string(),
                source.display().to_string(),
                local_path.display().to_string(),
            ])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("rsync failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(format!("Downloaded to {}", local_path.display()))
    }

    async fn install_watchdog(&self, _policy: WatchdogPolicy) -> anyhow::Result<()> {
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        self.last_budget_deadline
            .store(secs_from_now, Ordering::Relaxed);
        Ok(())
    }
}
