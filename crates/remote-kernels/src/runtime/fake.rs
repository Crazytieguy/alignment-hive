//! Test backend: "machines" are local Jupyter server processes.
//!
//! Enables full-stack testing of the server (provision → kernel → execute →
//! sync → budget enforcement → cleanup) with zero cloud dependencies. The
//! Jupyter layer and the machine-side supervisor run for real; only the
//! provider is fake, and it behaves like one: commands take a while, a stopped
//! machine can't be reached, a stop wipes its disk except the state dir, and
//! the provider's answers lag behind what happened.
//!
//! Environment knobs:
//! - `REMOTE_KERNELS_FAKE_JUPYTER`: command to launch Jupyter (default
//!   `jupyter server`; whitespace-split, e.g. `uv run --with jupyter-server jupyter server`)
//! - `REMOTE_KERNELS_FAKE_COST_PER_HR`, `REMOTE_KERNELS_FAKE_STORAGE_PER_HR`:
//!   pretend rates (default 0.0) so budgets can be tested against wall-clock accrual
//! - `REMOTE_KERNELS_FAKE_EXEC_PAUSE_MS`: fixed exec latency (default a random 300–800 ms)
//! - `REMOTE_KERNELS_FAKE_DESCRIBE_LAG_MS`: how long `describe` keeps reporting
//!   the status from before a stop, resume or terminate
//! - `REMOTE_KERNELS_FAKE_DESCRIBE_PAUSE_MS`: how long each `describe` takes
//! - `REMOTE_KERNELS_FAKE_PROVISIONING_MS`: how long a machine reports
//!   Provisioning after a create or resume
//! - `REMOTE_KERNELS_FAKE_BEAT_MS`: beat interval (default 60 s)
//! - `REMOTE_KERNELS_FAKE_DESCRIBE_ERROR`: `describe` fails
//! - `REMOTE_KERNELS_FAKE_RESUME_ERROR`: `resume` fails
//! - `REMOTE_KERNELS_FAKE_STOP_REJECTED`: stop and terminate answer with a 4xx
//! - `REMOTE_KERNELS_FAKE_UNSUPERVISABLE`: the machine lacks what the supervisor needs
//! - `REMOTE_KERNELS_FAKE_RECORDER_READ_ERROR`: reading a kernel's recorder log fails
//!
//! Per-machine switches, as files in the state dir: `fake-no-exec` makes every
//! exec fail like a dropped connection; `fake-block-action` makes the machine's
//! own stop or terminate call fail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, RemoteDirs, Runtime, StopSupport,
};

/// Where a machine's own stop or terminate call lands.
const SELF_ACTION_FILE: &str = "fake-provider-action";
const BLOCK_ACTION_FILE: &str = "fake-block-action";
const NO_EXEC_FILE: &str = "fake-no-exec";

struct FakeInstance {
    /// The provider-side name of this "machine" — the machine id the create
    /// asked for. Only [`Runtime::find_by_name`] reads it.
    name: String,
    child: Option<tokio::process::Child>,
    port: u16,
    token: String,
    workdir: tempfile::TempDir,
    /// Seen by every connection to this machine.
    stopped: Arc<AtomicBool>,
    /// Status `describe` still reports until the instant.
    lagging: Option<(InstanceStatus, Instant)>,
    provisioning_until: Option<Instant>,
}

impl FakeInstance {
    fn state_dir(&self) -> PathBuf {
        self.workdir.path().join(".remote-kernels")
    }

    fn true_status(&mut self) -> InstanceStatus {
        match self.child.as_mut().map(tokio::process::Child::try_wait) {
            None | Some(Ok(Some(_)) | Err(_)) => InstanceStatus::Stopped,
            Some(Ok(None)) => {
                if self
                    .provisioning_until
                    .is_some_and(|until| Instant::now() < until)
                {
                    InstanceStatus::Provisioning
                } else {
                    InstanceStatus::Running
                }
            }
        }
    }

    /// Stopping a container: its processes die, and only the state dir stays.
    async fn power_off(&mut self) {
        kill_machine_processes(self.workdir.path());
        if let Some(child) = self.child.take() {
            kill_and_reap(child).await;
        }
        self.stopped.store(true, Ordering::Relaxed);
        if let Ok(entries) = std::fs::read_dir(self.workdir.path()) {
            for entry in entries
                .flatten()
                .filter(|e| e.file_name() != ".remote-kernels")
            {
                let path = entry.path();
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    }
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

/// What a real stop would take down with the container: the recorders and
/// the supervisor.
fn kill_machine_processes(workdir: &Path) {
    let state_dir = workdir.join(".remote-kernels");
    if let Ok(pid) = std::fs::read_to_string(state_dir.join("supervisor.pid"))
        && let Ok(pid) = pid.trim().parse::<u32>()
    {
        kill_if_command_contains(pid, "rk-supervisor");
    }
    kill_recorders(workdir);
}

fn kill_if_command_contains(pid: u32, needle: &str) {
    if command_line(pid).is_some_and(|command| command.contains(needle)) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

fn command_line(pid: u32) -> Option<String> {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn kill_recorders(workdir: &Path) {
    kill_recorders_with(workdir, command_line);
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
        kill_machine_processes(self.workdir.path());
        if let Some(child) = &self.child {
            kill_group(child);
        }
    }
}

/// The fake provider's whole view of one project.
#[derive(Default)]
struct Fleet {
    machines: HashMap<String, FakeInstance>,
    /// Terminated machines `describe` still reports for a while.
    ghosts: HashMap<String, (InstanceStatus, Instant)>,
}

impl Fleet {
    /// Carry out the stop or terminate calls machines made on themselves.
    async fn apply_self_actions(&mut self) {
        let ids = self.machines.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let path = self.machines[&id].state_dir().join(SELF_ACTION_FILE);
            let Ok(action) = std::fs::read_to_string(&path) else {
                continue;
            };
            let _ = std::fs::remove_file(&path);
            match action.trim() {
                "terminate" => self.remove(&id).await,
                "stop" => {
                    if let Some(machine) = self.machines.get_mut(&id) {
                        machine.power_off().await;
                    }
                }
                other => tracing::warn!("fake provider: unknown self-action {other:?}"),
            }
        }
    }

    async fn remove(&mut self, id: &str) {
        if let Some(mut machine) = self.machines.remove(id) {
            kill_machine_processes(machine.workdir.path());
            if let Some(child) = machine.child.take() {
                kill_and_reap(child).await;
            }
        }
    }
}

type FakeFleet = Arc<Mutex<Fleet>>;
type FakeProjects = std::sync::Mutex<HashMap<PathBuf, FakeFleet>>;

pub struct FakeRuntime {
    fleet: FakeFleet,
    cost_per_hr: f64,
    storage_per_hr: f64,
}

fn env_rate(name: &str) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0)
}

/// A simulated delay set in the environment, in milliseconds.
fn env_pause(name: &str) -> Option<Duration> {
    let millis = std::env::var(name).ok()?.parse().ok()?;
    Some(Duration::from_millis(millis))
}

fn stale_after(status: InstanceStatus) -> Option<(InstanceStatus, Instant)> {
    env_pause("REMOTE_KERNELS_FAKE_DESCRIBE_LAG_MS").map(|lag| (status, Instant::now() + lag))
}

fn exec_latency() -> Duration {
    env_pause("REMOTE_KERNELS_FAKE_EXEC_PAUSE_MS")
        .unwrap_or_else(|| Duration::from_millis(rand::random_range(300..800)))
}

impl FakeRuntime {
    pub fn new(project_dir: &Path) -> Self {
        static PROJECTS: std::sync::LazyLock<FakeProjects> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
        Self {
            fleet: PROJECTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(project_dir.to_path_buf())
                .or_default()
                .clone(),
            cost_per_hr: env_rate("REMOTE_KERNELS_FAKE_COST_PER_HR"),
            storage_per_hr: env_rate("REMOTE_KERNELS_FAKE_STORAGE_PER_HR"),
        }
    }

    fn handle(&self, external_id: &str) -> InstanceHandle {
        InstanceHandle {
            external_id: external_id.to_string(),
            gpu_name: "Fake GPU".to_string(),
            cost_per_hr: Some(self.cost_per_hr),
            storage_rate_per_hr: self.storage_per_hr,
            storage_rate_note: (self.storage_per_hr == 0.0)
                .then(|| "fake runtime exposes no storage price".to_string()),
            note: None,
            proxy_port_mapped: false,
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

    fn check_not_rejected(action: &str) -> anyhow::Result<()> {
        if std::env::var_os("REMOTE_KERNELS_FAKE_STOP_REJECTED").is_some() {
            anyhow::bail!("API error (403): simulated rejection of {action}");
        }
        Ok(())
    }
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self::new(Path::new("."))
    }
}

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see
/// [`super::validate_config`]). Meteredness is
/// per-instance for the fake runtime (e2e tests simulate billing); validation
/// treats it as unmetered since fake machines never cost real money.
pub(crate) fn capabilities(metered: bool) -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Full,
        metered,
        provision_timeout: env_pause("REMOTE_KERNELS_FAKE_PROVISION_TIMEOUT_MS")
            .or(Some(Duration::from_mins(20))),
        account_ssh_keys: false,
    }
}

impl Runtime for FakeRuntime {
    type Conn = FakeConnection;

    fn capabilities(&self) -> Capabilities {
        capabilities(self.cost_per_hr > 0.0)
    }

    /// Derived from config on every connect; nothing to record.
    fn remote_dirs(&self, _machine_id: &str, _image: Option<&str>) -> Option<RemoteDirs> {
        None
    }

    fn machine_name(&self, machine_id: &str) -> String {
        machine_id.to_string()
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        if let Some(pause) = env_pause("REMOTE_KERNELS_FAKE_CREATE_PAUSE_MS") {
            tokio::time::sleep(pause).await;
        }
        if std::env::var_os("REMOTE_KERNELS_FAKE_CREATE_REJECTED").is_some() {
            anyhow::bail!("API error (400): simulated rejection of the create");
        }
        let external_id = format!("fake-{}", uuid::Uuid::new_v4());
        let port = Self::free_port()?;
        let workdir = tempfile::tempdir()?;
        std::fs::create_dir_all(workdir.path().join(".remote-kernels"))?;
        let child = Self::spawn_jupyter(port, &req.jupyter_token, workdir.path())?;

        self.fleet.lock().await.machines.insert(
            external_id.clone(),
            FakeInstance {
                name: req.machine_id.clone(),
                child: Some(child),
                port,
                token: req.jupyter_token.clone(),
                workdir,
                stopped: Arc::new(AtomicBool::new(false)),
                lagging: None,
                provisioning_until: env_pause("REMOTE_KERNELS_FAKE_PROVISIONING_MS")
                    .map(|pause| Instant::now() + pause),
            },
        );

        // The machine EXISTS from here on. Returning an unconfirmed-create
        // error instead of the handle is what a provider that answers a
        // committed create with a 5xx looks like from here.
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
            }));
        }

        Ok(self.handle(&external_id))
    }

    async fn find_by_name(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let mut fleet = self.fleet.lock().await;
        fleet.apply_self_actions().await;
        Ok(fleet
            .machines
            .iter()
            .filter(|(_, instance)| instance.name == name)
            .map(|(external_id, _)| external_id.clone())
            .collect())
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let fleet = self.fleet.lock().await;
        anyhow::ensure!(
            fleet.machines.contains_key(external_id),
            "unknown fake instance"
        );
        Ok(self.handle(external_id))
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        if std::env::var_os("REMOTE_KERNELS_FAKE_DESCRIBE_ERROR").is_some() {
            anyhow::bail!("simulated provider outage");
        }
        if let Some(pause) = env_pause("REMOTE_KERNELS_FAKE_DESCRIBE_PAUSE_MS") {
            tokio::time::sleep(pause).await;
        }
        let mut fleet = self.fleet.lock().await;
        fleet.apply_self_actions().await;
        if let Some((status, until)) = fleet.ghosts.get(external_id) {
            if Instant::now() < *until {
                return Ok(status.clone());
            }
            fleet.ghosts.remove(external_id);
        }
        let Some(machine) = fleet.machines.get_mut(external_id) else {
            return Ok(InstanceStatus::Gone);
        };
        if let Some((status, until)) = &machine.lagging {
            if Instant::now() < *until {
                return Ok(status.clone());
            }
            machine.lagging = None;
        }
        Ok(machine.true_status())
    }

    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        loop {
            match self.describe(external_id).await? {
                InstanceStatus::Running => return self.get_handle(external_id).await,
                InstanceStatus::Provisioning => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => anyhow::bail!("fake instance is {other:?}, not running"),
            }
        }
    }

    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        Self::check_not_rejected("stop")?;
        if std::env::var_os("REMOTE_KERNELS_FAKE_STOP_ERROR_BEFORE_ACTION").is_some() {
            anyhow::bail!("simulated ambiguous stop failure before provider action");
        }
        if let Some(pause) = env_pause("REMOTE_KERNELS_FAKE_STOP_PAUSE_MS") {
            tokio::time::sleep(pause).await;
        }
        let mut fleet = self.fleet.lock().await;
        let inst = fleet
            .machines
            .get_mut(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        let before = inst.true_status();
        inst.lagging = stale_after(before);
        inst.power_off().await;
        if std::env::var_os("REMOTE_KERNELS_FAKE_STOP_ERROR_AFTER_ACTION").is_some() {
            anyhow::bail!("simulated ambiguous stop failure after provider action");
        }
        Ok(())
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        if std::env::var_os("REMOTE_KERNELS_FAKE_RESUME_ERROR").is_some() {
            anyhow::bail!("simulated capacity failure");
        }
        let mut fleet = self.fleet.lock().await;
        let inst = fleet
            .machines
            .get_mut(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        if inst.child.is_none() {
            inst.lagging = stale_after(InstanceStatus::Stopped);
            inst.provisioning_until = env_pause("REMOTE_KERNELS_FAKE_PROVISIONING_MS")
                .map(|pause| Instant::now() + pause);
            inst.child = Some(Self::spawn_jupyter(
                inst.port,
                &inst.token,
                inst.workdir.path(),
            )?);
            inst.stopped.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        Self::check_not_rejected("terminate")?;
        let mut fleet = self.fleet.lock().await;
        if let Some(ghost) = fleet
            .machines
            .get_mut(external_id)
            .map(FakeInstance::true_status)
            .and_then(stale_after)
        {
            fleet.ghosts.insert(external_id.to_string(), ghost);
        }
        fleet.remove(external_id).await;
        Ok(())
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<FakeConnection> {
        // A fresh machine's slow open (RunPod's wait while its image pulls),
        // optionally ending the way that wait can. The instance map stays free.
        if ctx.fresh
            && let Some(pause) = env_pause("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS")
        {
            tokio::time::sleep(pause).await;
            if std::env::var_os("REMOTE_KERNELS_FAKE_FRESH_OPEN_STILL_PROVISIONING").is_some() {
                return Err(super::StillProvisioning.into());
            }
            if std::env::var_os("REMOTE_KERNELS_FAKE_FRESH_OPEN_ERROR").is_some() {
                anyhow::bail!("simulated open failure");
            }
        }
        // An attach whose SSH isn't back yet (RunPod's wait for a tunnel).
        if !ctx.fresh
            && std::env::var_os("REMOTE_KERNELS_FAKE_ATTACH_OPEN_STILL_PROVISIONING").is_some()
        {
            return Err(super::StillProvisioning.into());
        }
        let fleet = self.fleet.lock().await;
        let inst = fleet
            .machines
            .get(external_id)
            .ok_or_else(|| anyhow::anyhow!("unknown fake instance"))?;
        // Like a real machine: nothing to connect to while it is stopped.
        anyhow::ensure!(inst.child.is_some(), "fake instance is stopped");
        let workdir = inst.workdir.path().to_path_buf();
        let state_dir = crate::machine_scripts::state_dir(&workdir.display().to_string());
        Ok(FakeConnection {
            jupyter: JupyterEndpoint::loopback(inst.port, ctx.jupyter_token.clone()),
            workdir_string: workdir.display().to_string(),
            workdir,
            stage_dir: format!("{state_dir}/staged"),
            state_dir,
            stopped: Arc::clone(&inst.stopped),
        })
    }
}

pub struct FakeConnection {
    jupyter: JupyterEndpoint,
    workdir: std::path::PathBuf,
    workdir_string: String,
    state_dir: String,
    stage_dir: String,
    stopped: Arc<AtomicBool>,
}

impl FakeConnection {
    #[cfg(test)]
    pub(crate) fn for_test(workdir: &Path) -> Self {
        let workdir_string = workdir.display().to_string();
        let state_dir = crate::machine_scripts::state_dir(&workdir_string);
        Self {
            jupyter: JupyterEndpoint::loopback(1, "test-token".to_string()),
            workdir: workdir.to_path_buf(),
            workdir_string,
            stage_dir: format!("{state_dir}/staged"),
            state_dir,
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn check_reachable(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.stopped.load(Ordering::Relaxed),
            "simulated transport failure: the machine is stopped"
        );
        anyhow::ensure!(
            !Path::new(&self.state_dir).join(NO_EXEC_FILE).exists(),
            "simulated transport failure"
        );
        Ok(())
    }
}

impl Connection for FakeConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn workdir(&self) -> &str {
        &self.workdir_string
    }

    fn state_dir(&self) -> String {
        self.state_dir.clone()
    }

    /// This runtime launches Jupyter itself with its root at the workdir.
    fn kernel_path(&self) -> Option<&str> {
        None
    }

    /// The state dir is what survives a stop here, like a volume.
    fn stage_dir(&self) -> Option<&str> {
        Some(&self.stage_dir)
    }

    fn machine_jupyter_port(&self) -> u16 {
        self.jupyter
            .http_base
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or(8888)
    }

    fn beat_interval(&self) -> Duration {
        env_pause("REMOTE_KERNELS_FAKE_BEAT_MS").unwrap_or(Duration::from_mins(1))
    }

    fn action_command(&self) -> Option<String> {
        let dir = crate::machine_scripts::shell_quote(&self.state_dir);
        Some(format!(
            "[ ! -e {dir}/{BLOCK_ACTION_FILE} ] && printf %s \"$1\" > {dir}/.action.tmp && \
             mv -f {dir}/.action.tmp {dir}/{SELF_ACTION_FILE}"
        ))
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.check_reachable()?;
        tokio::time::sleep(exec_latency()).await;
        self.check_reachable()?;
        if std::env::var_os("REMOTE_KERNELS_FAKE_RECORDER_READ_ERROR").is_some()
            && command.contains(".jsonl")
        {
            anyhow::bail!("ssh: connect to host: Connection timed out");
        }
        if std::env::var_os("REMOTE_KERNELS_FAKE_UNSUPERVISABLE").is_some()
            && command.contains("rk-supervisor.sh")
        {
            return Ok("unsupervisable the machine lacks curl\n".to_string());
        }
        let output = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&self.workdir)
                // Nothing this suite leaves behind outlives the test binary.
                .env("RK_SUPERVISOR_PARENT", std::process::id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // An abandoned exec must not leave stray local processes
                // behind the test suite.
                .kill_on_drop(true)
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
        self.check_reachable()?;
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
        self.check_reachable()?;
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Same path semantics as production backends (workdir-relative,
        // absolute honored) so the e2e suite tests the real contract.
        let source = crate::sync::resolve_remote_path(&self.workdir.to_string_lossy(), remote_path);
        // Some rsyncs create the destination before they find the source gone.
        anyhow::ensure!(
            Path::new(&source).exists(),
            "rsync failed: no such file {source}"
        );
        let output = tokio::process::Command::new("rsync")
            .args(["-az".to_string(), source, local_path.display().to_string()])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("rsync failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(format!("Downloaded to {}", local_path.display()))
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
