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
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

struct FakeInstance {
    /// The provider-side name of this "machine" — the machine id the create
    /// asked for. Only [`Runtime::find_by_name`] reads it.
    name: String,
    child: Option<tokio::process::Child>,
    port: u16,
    token: String,
    workdir: tempfile::TempDir,
    /// Last budget deadline (secs-from-now) pushed by the heartbeat, for test
    /// observation of the budget supervisor.
    last_budget_deadline: Arc<AtomicU64>,
    lease_no_flock: bool,
}

/// Kill the Jupyter server's whole process group. `child.kill()` (and
/// `kill_on_drop`) only signal the direct child — the launcher wrapper or
/// the Jupyter server itself — orphaning ipykernel grandchildren, which is
/// exactly what leaves stray jupyter/python processes after test runs.
/// [`FakeRuntime::spawn_jupyter`] puts the server in its own process group
/// (pgid == pid), so signaling the group reaps everything.
fn kill_group(child: &tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .status();
    }
}

fn kill_recorders(workdir: &Path) {
    kill_recorders_with(workdir, |pid| {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    });
}

fn kill_recorders_with(workdir: &Path, command_line: impl Fn(u32) -> Option<String>) {
    let output_dir = workdir.join(".remote-kernels/kernel-output");
    let Ok(entries) = std::fs::read_dir(output_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "pid")
            && let Ok(pid) = std::fs::read_to_string(entry.path())
            && let Ok(pid) = pid.trim().parse::<u32>()
            && command_line(pid).is_some_and(|command| command.contains("rk-output-recorder"))
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}

/// Group-kill, then reap the direct child. The group signal must land
/// BEFORE the child is reaped — reaping first frees the pid, and the pgid
/// could be reused by an unrelated process.
async fn kill_and_reap(mut child: tokio::process::Child) {
    kill_group(&child);
    let _ = child.kill().await;
}

/// Panic/drop safety net: a test that aborts mid-lifecycle still tears the
/// group down (`kill_on_drop` alone would orphan the kernels).
impl Drop for FakeInstance {
    fn drop(&mut self) {
        kill_recorders(self.workdir.path());
        if let Some(child) = &self.child {
            kill_group(child);
        }
    }
}

pub struct FakeRuntime {
    instances: Arc<Mutex<HashMap<String, FakeInstance>>>,
    cost_per_hr: f64,
}

type FakeInstances = Arc<Mutex<HashMap<String, FakeInstance>>>;
type FakeProjects = std::sync::Mutex<HashMap<PathBuf, FakeInstances>>;

impl FakeRuntime {
    pub fn new(project_dir: &Path) -> Self {
        static PROJECTS: std::sync::LazyLock<FakeProjects> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
        let cost_per_hr = std::env::var("REMOTE_KERNELS_FAKE_COST_PER_HR")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        Self {
            instances: PROJECTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(project_dir.to_path_buf())
                .or_insert_with(|| Arc::new(Mutex::new(HashMap::new())))
                .clone(),
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

        let mut command = tokio::process::Command::new(program);
        // Own process group so cleanup can signal the group: SIGKILL to the
        // server alone can't reach the ipykernel children it spawned.
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .args(args)
            .arg("--no-browser")
            .arg("--ip=127.0.0.1")
            .arg(format!("--port={port}"))
            .arg("--port-retries=0")
            .arg(format!("--ServerApp.token={token}"))
            // XSRF checking stays ON, mirroring the real launch script: our
            // client authenticates with the token header, which Jupyter
            // exempts from the XSRF check — the fake e2e suite is the free
            // regression proof of that.
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
        Self::new(Path::new("."))
    }
}

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see [`super::validate_config`]). Meteredness is
/// per-instance for the fake runtime (e2e tests simulate billing); validation
/// treats it as unmetered since fake machines never cost real money.
pub(crate) fn capabilities(metered: bool) -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Full,
        metered,
        provision_timeout: Some(std::time::Duration::from_mins(20)),
        account_ssh_keys: false,
    }
}

impl Runtime for FakeRuntime {
    type Conn = FakeConnection;

    fn name(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(self.cost_per_hr > 0.0)
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        let external_id = format!("fake-{}", uuid::Uuid::new_v4());
        let port = Self::free_port()?;
        let workdir = tempfile::tempdir()?;
        let bin_dir = workdir.path().join(".remote-kernels/fake-bin");
        std::fs::create_dir_all(&bin_dir)?;
        let flock = bin_dir.join("flock");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/machine/test-support/flock.py"),
            &flock,
        )?;
        #[cfg(unix)]
        std::fs::set_permissions(&flock, std::fs::Permissions::from_mode(0o755))?;
        let child = Self::spawn_jupyter(port, &req.jupyter_token, workdir.path())?;

        self.instances.lock().await.insert(
            external_id.clone(),
            FakeInstance {
                name: req.machine_id.clone(),
                child: Some(child),
                port,
                token: req.jupyter_token.clone(),
                workdir,
                last_budget_deadline: Arc::new(AtomicU64::new(u64::MAX)),
                lease_no_flock: std::env::var_os("REMOTE_KERNELS_FAKE_NO_FLOCK").is_some(),
            },
        );

        // The machine EXISTS from here on. Returning an unconfirmed-create
        // error instead of the handle is what a provider that answers a
        // committed create with a 5xx looks like from here — the one path
        // that leaves a machine with no local record.
        if std::env::var_os("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE").is_some() {
            return Err(anyhow::Error::new(super::UnconfirmedCreate {
                // Same shape as the real one: it must never send the reader
                // back to start(), which would mint a second billing machine.
                summary: format!(
                    "Creating the machine failed with an unclear outcome (simulated). No \
                     second machine was created. A machine named {} may exist and is \
                     tracked as machine {}: status() adopts it if it appears. Call \
                     status() before starting another machine.",
                    req.machine_id, req.machine_id
                ),
                cause: "simulated unconfirmed create".to_string(),
                expected_name: req.machine_id.clone(),
                self_halt_mins: None,
                noun: "machine",
                provider: "the fake provider",
            }));
        }

        Ok(InstanceHandle {
            external_id,
            gpu_name: "Fake GPU".to_string(),
            cost_per_hr: Some(self.cost_per_hr),
            storage_rate_per_hr: 0.0,
            storage_rate_note: Some("fake runtime exposes no storage price".to_string()),
            note: None,
            proxy_port_mapped: false,
        })
    }

    async fn find_by_name(&self, name: &str) -> anyhow::Result<Vec<String>> {
        Ok(self
            .instances
            .lock()
            .await
            .iter()
            .filter(|(_, instance)| instance.name == name)
            .map(|(external_id, _)| external_id.clone())
            .collect())
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let instances = self.instances.lock().await;
        anyhow::ensure!(instances.contains_key(external_id), "unknown fake instance");
        Ok(InstanceHandle {
            external_id: external_id.to_string(),
            gpu_name: "Fake GPU".to_string(),
            cost_per_hr: Some(self.cost_per_hr),
            storage_rate_per_hr: 0.0,
            storage_rate_note: Some("fake runtime exposes no storage price".to_string()),
            note: None,
            proxy_port_mapped: false,
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
        if std::env::var_os("REMOTE_KERNELS_FAKE_STOP_ERROR_BEFORE_ACTION").is_some() {
            anyhow::bail!("simulated ambiguous stop failure before provider action");
        }
        if let Some(delay_ms) = std::env::var("REMOTE_KERNELS_FAKE_STOP_PAUSE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let mut instances = self.instances.lock().await;
        let inst = instances
            .get_mut(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        if let Some(child) = inst.child.take() {
            kill_and_reap(child).await;
        }
        if std::env::var_os("REMOTE_KERNELS_FAKE_STOP_ERROR_AFTER_ACTION").is_some() {
            anyhow::bail!("simulated ambiguous stop failure after provider action");
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
        if let Some(mut inst) = self.instances.lock().await.remove(external_id) {
            kill_recorders(inst.workdir.path());
            if let Some(child) = inst.child.take() {
                kill_and_reap(child).await;
            }
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
        let workdir = inst.workdir.path().to_path_buf();
        Ok(FakeConnection {
            jupyter: JupyterEndpoint::loopback(inst.port, ctx.jupyter_token.clone()),
            workdir_string: workdir.display().to_string(),
            workdir,
            bin_dir: inst.workdir.path().join(".remote-kernels/fake-bin"),
            last_budget_deadline: Arc::clone(&inst.last_budget_deadline),
            lease_no_flock: inst.lease_no_flock,
        })
    }
}

pub struct FakeConnection {
    jupyter: JupyterEndpoint,
    workdir: std::path::PathBuf,
    workdir_string: String,
    bin_dir: std::path::PathBuf,
    last_budget_deadline: Arc<AtomicU64>,
    lease_no_flock: bool,
}

impl FakeConnection {
    /// For tests: the last deadline (secs-from-now) the heartbeat pushed.
    pub fn last_budget_deadline(&self) -> u64 {
        self.last_budget_deadline.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn for_test(workdir: &Path, lease_no_flock: bool) -> anyhow::Result<Self> {
        let bin_dir = workdir.join(".remote-kernels/fake-bin");
        std::fs::create_dir_all(&bin_dir)?;
        let flock = bin_dir.join("flock");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/machine/test-support/flock.py"),
            &flock,
        )?;
        #[cfg(unix)]
        std::fs::set_permissions(&flock, std::fs::Permissions::from_mode(0o755))?;
        Ok(Self {
            jupyter: JupyterEndpoint::loopback(1, "test-token".to_string()),
            workdir: workdir.to_path_buf(),
            workdir_string: workdir.display().to_string(),
            bin_dir,
            last_budget_deadline: Arc::new(AtomicU64::new(u64::MAX)),
            lease_no_flock,
        })
    }
}

impl Connection for FakeConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn workdir(&self) -> &str {
        &self.workdir_string
    }

    fn recorder_ws_url(&self) -> String {
        self.jupyter.ws_base.clone()
    }

    fn watchdog_port(&self) -> u16 {
        self.jupyter
            .http_base
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or(8888)
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        if self.lease_no_flock && command.contains("flock is required") {
            return Ok("flock is required\n__RK_LEASE_EXIT__=11\n".to_string());
        }
        let mut path = std::ffi::OsString::from(self.bin_dir.as_os_str());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let output = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&self.workdir)
                .env("PATH", path)
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

    async fn wait_reachable(
        &self,
        _diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        let mut args = crate::sync::rsync_upload_args(extra_includes);
        args.extend([
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
        // Same path semantics as production backends (workdir-relative,
        // absolute honored) so the e2e suite tests the real contract.
        let source = crate::sync::resolve_remote_path(&self.workdir.to_string_lossy(), remote_path);
        let output = tokio::process::Command::new("rsync")
            .args(["-az".to_string(), source, local_path.display().to_string()])
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

#[cfg(test)]
mod tests {
    use super::kill_recorders_with;

    fn write_pid(workdir: &std::path::Path, pid: u32) {
        let output_dir = workdir.join(".remote-kernels/kernel-output");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join("kernel.pid"), pid.to_string()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn kill_recorders_ignores_reused_non_recorder_pid() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        write_pid(dir.path(), child.id());
        kill_recorders_with(dir.path(), |_| Some("sleep 30".to_string()));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn kill_recorders_kills_matching_dead_recorder_process() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("rk-output-recorder-sleeper.py");
        std::fs::write(&script, "import time; time.sleep(30)\n").unwrap();
        let mut child = std::process::Command::new("python3")
            .arg(&script)
            .spawn()
            .unwrap();
        write_pid(dir.path(), child.id());
        kill_recorders_with(dir.path(), |_| {
            Some("python3 rk-output-recorder-sleeper.py".to_string())
        });
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        panic!("matching recorder process was not killed");
    }
}
