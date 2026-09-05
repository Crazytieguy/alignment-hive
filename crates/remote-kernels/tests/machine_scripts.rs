use std::ffi::OsString;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::TempDir;

const FENCED: i32 = 9;
const REFUSED: i32 = 10;
const NO_FLOCK: i32 = 12;

fn machine_script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/machine")
        .join(name)
}

fn support_script(name: &str) -> PathBuf {
    machine_script("test-support").join(name)
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

struct Harness {
    temp: TempDir,
    path: OsString,
    bash_env: PathBuf,
}

impl Harness {
    fn new(test_name: &str) -> Option<Self> {
        if !python_available() {
            eprintln!("skipping {test_name}: python3 is unavailable (needed by test support)");
            return None;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).expect("create test bin");
        let flock = bin.join("flock");
        fs::copy(support_script("flock.py"), &flock).expect("install flock test helper");
        fs::set_permissions(&flock, fs::Permissions::from_mode(0o755)).expect("chmod flock helper");
        let mut path = OsString::from(bin.as_os_str());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let bash_env = temp.path().join("bash-env");
        fs::write(&bash_env, format!("export PATH={}:$PATH\n", bin.display()))
            .expect("write bash environment");
        Some(Self {
            temp,
            path,
            bash_env,
        })
    }

    fn state_dir(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command.env("PATH", &self.path);
        command
    }

    fn install_command(&self, name: &str, body: &str) {
        let path = self.temp.path().join("bin").join(name);
        fs::write(&path, body).expect("write command stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod command stub");
    }

    fn lease(&self, args: &[&str]) -> Output {
        let mut command = self.command("bash");
        command.arg(machine_script("rk-lease.sh"));
        command.arg(self.state_dir());
        command.args(args);
        command.output().expect("run lease script")
    }

    fn lease_ok(&self, args: &[&str]) {
        let output = self.lease(args);
        assert!(
            output.status.success(),
            "lease command failed: status={:?}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn lease_json(&self, args: &[&str]) -> Value {
        let output = self.lease(args);
        assert!(
            output.status.success(),
            "lease command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("valid lease JSON output")
    }

    fn read_lease(&self) -> Value {
        let output = self.lease(&["read"]);
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).expect("valid lease JSON")
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_log_lines(path: &Path, count: usize, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        let contents = fs::read_to_string(path).unwrap_or_default();
        if contents.lines().count() >= count {
            return contents;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for action log; watchdog log: {}",
            fs::read_to_string(
                path.parent()
                    .expect("action log parent")
                    .join("state/watchdog.log")
            )
            .unwrap_or_else(|error| format!("<unavailable: {error}>"))
        );
        thread::sleep(Duration::from_millis(50));
    }
}

struct FakeJupyter {
    child: Option<Child>,
    state_file: PathBuf,
    port: u16,
}

impl FakeJupyter {
    fn start(harness: &Harness, state: &str, test_name: &str) -> Self {
        let state_file = harness.temp.path().join("kernel-state");
        fs::write(&state_file, state).expect("write kernel state");
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "{test_name}: loopback bind forbidden; using the local curl test transport"
                );
                let curl = harness.temp.path().join("bin/curl");
                fs::copy(support_script("curl.py"), &curl).expect("install curl test helper");
                fs::set_permissions(&curl, fs::Permissions::from_mode(0o755))
                    .expect("chmod curl helper");
                return Self {
                    child: None,
                    state_file,
                    port: 1,
                };
            }
            Err(error) => panic!("reserve local port: {error}"),
        };
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let child = harness
            .command("python3")
            .arg(support_script("fake-jupyter.py"))
            .arg(port.to_string())
            .arg(&state_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start fake Jupyter");
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "fake Jupyter did not start"
            );
            thread::sleep(Duration::from_millis(25));
        }
        Self {
            child: Some(child),
            state_file,
            port,
        }
    }

    fn set_state(&self, state: &str) {
        fs::write(&self.state_file, state).expect("update kernel state");
    }
}

impl Drop for FakeJupyter {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct WatchdogGuard {
    state_dir: PathBuf,
}

impl Drop for WatchdogGuard {
    fn drop(&mut self) {
        let Ok(pid) = fs::read_to_string(self.state_dir.join("watchdog.pid")) else {
            return;
        };
        let _ = Command::new("kill")
            .arg(pid.trim())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct WatchdogConfig<'a> {
    stale_secs: u64,
    grace_secs: u64,
    finalize_wait_secs: u64,
    finalize_timeout_secs: u64,
    default_action: &'a str,
    finalize_cmd: &'a Path,
}

#[derive(Default)]
struct InstallOptions<'a> {
    storage_rate: Option<&'a str>,
    enter_pause: Option<(u64, &'a Path)>,
    action_command: Option<&'a str>,
}

fn install_watchdog(
    harness: &Harness,
    jupyter: &FakeJupyter,
    action_log: &Path,
    config: &WatchdogConfig<'_>,
) -> WatchdogGuard {
    install_watchdog_with_options(
        harness,
        jupyter,
        action_log,
        config,
        &InstallOptions::default(),
    )
}

fn install_watchdog_with_options(
    harness: &Harness,
    jupyter: &FakeJupyter,
    action_log: &Path,
    config: &WatchdogConfig<'_>,
    options: &InstallOptions<'_>,
) -> WatchdogGuard {
    let state_dir = harness.state_dir();
    let mut command = harness.command("bash");
    command
        .arg(machine_script("rk-watchdog.sh"))
        .arg(&state_dir)
        .arg("install")
        .arg(machine_script("rk-lease.sh"))
        .arg(config.stale_secs.to_string())
        .arg(config.grace_secs.to_string())
        .arg(config.finalize_wait_secs.to_string())
        .arg(config.finalize_timeout_secs.to_string())
        .arg(jupyter.port.to_string())
        .arg("test-token")
        .arg(config.default_action)
        .arg(config.finalize_cmd)
        .arg(options.action_command.map_or_else(
            || format!("{} \"$1\"", support_script("provider-action.sh").display()),
            ToString::to_string,
        ))
        .env("RK_WATCHDOG_POLL_SECS", "1")
        .env("RK_ACTION_LOG", action_log)
        .env("RK_STATE_DIR", &state_dir)
        .env("BASH_ENV", &harness.bash_env)
        .env("RUNPOD_POD_ID", "test-pod")
        .env("RUNPOD_API_KEY", "test-key")
        .env("RK_FAKE_JUPYTER_STATE", &jupyter.state_file);
    if let Some(storage_rate) = options.storage_rate {
        command.arg(storage_rate);
    }
    if let Some((seconds, marker)) = options.enter_pause {
        command
            .env(
                "RK_WATCHDOG_TEST_PAUSE_AFTER_ENTER_SECS",
                seconds.to_string(),
            )
            .env("RK_WATCHDOG_TEST_PAUSE_AFTER_ENTER_MARKER", marker);
    }
    let output = command.output().expect("install watchdog");
    assert!(
        output.status.success(),
        "watchdog install failed: status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for(&state_dir.join("watchdog.pid"), Duration::from_secs(2));
    WatchdogGuard { state_dir }
}

fn stale_heartbeat(harness: &Harness, generation: u64) {
    fs::write(
        harness.state_dir().join("heartbeat"),
        format!("{generation} {}\n", now_epoch().saturating_sub(30)),
    )
    .expect("write heartbeat");
}

#[test]
fn acquire_refresh_and_stale_writer_is_fenced_without_mutation() {
    let Some(harness) = Harness::new("acquire_refresh_and_stale_writer_is_fenced_without_mutation")
    else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["refresh", "1", "owner-a"]);
    harness.lease_ok(&["acquire", "owner-b"]);
    let before = fs::read(harness.state_dir().join("lease.json")).expect("lease before fence");
    let fenced = harness.lease(&["refresh", "1", "owner-a"]);
    assert_eq!(fenced.status.code(), Some(FENCED));
    let after = fs::read(harness.state_dir().join("lease.json")).expect("lease after fence");
    assert_eq!(before, after);
}

#[test]
fn acquire_returns_owned_lease_from_the_same_critical_section() {
    let Some(harness) = Harness::new("acquire_returns_owned_lease_from_the_same_critical_section")
    else {
        return;
    };
    let before = now_epoch();
    let lease = harness.lease_json(&["acquire", "owner-a"]);
    let after = now_epoch();
    assert_eq!(lease["generation"], 1);
    assert_eq!(lease["owner_uuid"], "owner-a");
    assert!(
        lease["now"]
            .as_u64()
            .is_some_and(|now| now >= before && now <= after)
    );
    let ts = lease["ts"].as_u64().unwrap();
    let now = lease["now"].as_u64().unwrap();
    assert!(now.saturating_sub(ts) <= 1);
}

#[test]
fn read_reports_machine_now_without_changing_stored_shape() {
    let Some(harness) = Harness::new("read_reports_machine_now_without_changing_stored_shape")
    else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    let read = harness.read_lease();
    assert!(read["now"].as_u64().is_some());
    let stored: Value = serde_json::from_slice(
        &fs::read(harness.state_dir().join("lease.json")).expect("stored lease"),
    )
    .expect("stored lease JSON");
    assert!(stored.get("now").is_none(), "machine now is response-only");
}

#[test]
fn acquire_during_disconnect_arm_cancels_finalize() {
    let Some(harness) = Harness::new("acquire_during_disconnect_arm_cancels_finalize") else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    harness.lease_ok(&["acquire", "owner-b"]);
    let lease = harness.read_lease();
    assert_eq!(lease["generation"], 2);
    assert_eq!(lease["owner_uuid"], "owner-b");
    assert_eq!(lease["state"], "active");
    assert_eq!(lease["arm_reason"], "");
}

#[test]
fn acquire_during_budget_arm_preserves_deadline_and_action() {
    let Some(harness) = Harness::new("acquire_during_budget_arm_preserves_deadline_and_action")
    else {
        return;
    };
    let deadline = now_epoch().saturating_sub(1).to_string();
    let extended_deadline = (now_epoch() + 300).to_string();
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "budget", &deadline]);
    harness.lease_ok(&["refresh", "1", "owner-a"]);
    assert_eq!(harness.read_lease()["state"], "armed");
    harness.lease_ok(&["arm", "1", "disconnect"]);
    harness.lease_ok(&["arm", "1", "budget", &extended_deadline]);
    harness.lease_ok(&["enter-finalizing", "budget", "op-a", "terminate"]);
    harness.lease_ok(&["revert-to-armed", "op-a"]);
    harness.lease_ok(&["acquire", "owner-b"]);
    let lease = harness.read_lease();
    assert_eq!(lease["generation"], 2);
    assert_eq!(lease["state"], "armed");
    assert_eq!(lease["arm_reason"], "budget");
    assert_eq!(lease["arm_deadline"], deadline.parse::<u64>().unwrap());
    assert_eq!(lease["action"], "");
}

#[test]
fn enter_finalizing_is_fenced_after_generation_rotation() {
    let Some(harness) = Harness::new("enter_finalizing_is_fenced_after_generation_rotation") else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    harness.lease_ok(&["acquire", "owner-b"]);
    let output = harness.lease(&["enter-finalizing", "1", "op-a", "terminate"]);
    assert_eq!(output.status.code(), Some(FENCED));
    assert_eq!(harness.read_lease()["state"], "active");
}

#[test]
fn acquire_refuses_a_finalizing_lease_while_its_finalizer_is_alive() {
    let Some(harness) =
        Harness::new("acquire_refuses_a_finalizing_lease_while_its_finalizer_is_alive")
    else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    harness.lease_ok(&["enter-finalizing", "1", "op-a", "stop"]);
    // A stand-in for the detached watchdog: a live process whose command
    // line names rk-watchdog, recorded in watchdog.pid as the real one is.
    let mut finalizer = Command::new("/bin/bash")
        .args(["-c", "exec -a rk-watchdog-stand-in sleep 30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stand-in finalizer");
    fs::write(
        harness.state_dir().join("watchdog.pid"),
        format!("{}\n", finalizer.id()),
    )
    .expect("write watchdog.pid");
    let output = harness.lease(&["acquire", "owner-b"]);
    let _ = finalizer.kill();
    let _ = finalizer.wait();
    assert_eq!(output.status.code(), Some(REFUSED));
    assert_eq!(harness.read_lease()["state"], "finalizing");
}

#[test]
fn acquire_reclaims_a_finalizing_lease_whose_finalizer_is_gone() {
    // The state directory can outlive the machine (a RunPod network volume
    // mounted at the workdir): a pod that died mid-finalize must not wedge
    // every later pod as "running its automatic cleanup".
    let Some(harness) = Harness::new("acquire_reclaims_a_finalizing_lease_whose_finalizer_is_gone")
    else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    harness.lease_ok(&["enter-finalizing", "1", "op-a", "stop"]);
    fs::write(harness.state_dir().join("outcome.json"), "{}").expect("stale outcome");
    // No watchdog.pid at all (fresh container) ...
    let lease = harness.lease_json(&["acquire", "owner-b"]);
    assert_eq!(lease["state"], "active");
    assert_eq!(lease["owner_uuid"], "owner-b");
    assert_eq!(lease["generation"], 2);
    assert!(!harness.state_dir().join("outcome.json").exists());
    // ... and a pid file naming a process that no longer exists.
    harness.lease_ok(&["arm", "2", "disconnect"]);
    harness.lease_ok(&["enter-finalizing", "2", "op-b", "terminate"]);
    let mut dead = Command::new("/bin/true").spawn().expect("spawn");
    let _ = dead.wait();
    fs::write(
        harness.state_dir().join("watchdog.pid"),
        format!("{}\n", dead.id()),
    )
    .expect("write watchdog.pid");
    let lease = harness.lease_json(&["acquire", "owner-c"]);
    assert_eq!(lease["state"], "active");
    assert_eq!(lease["owner_uuid"], "owner-c");
}

#[test]
fn critical_section_pause_blocks_concurrent_acquire() {
    let Some(harness) = Harness::new("critical_section_pause_blocks_concurrent_acquire") else {
        return;
    };
    harness.lease_ok(&["acquire", "owner-a"]);
    let marker = harness.temp.path().join("paused");
    let mut refresh = harness
        .command("bash")
        .arg(machine_script("rk-lease.sh"))
        .arg(harness.state_dir())
        .args(["refresh", "1", "owner-a"])
        .env("RK_LEASE_TEST_PAUSE_AFTER_READ_SECS", "2")
        .env("RK_LEASE_TEST_PAUSE_MARKER", &marker)
        .spawn()
        .expect("spawn paused refresh");
    wait_for(&marker, Duration::from_secs(2));
    let mut acquire = harness
        .command("bash")
        .arg(machine_script("rk-lease.sh"))
        .arg(harness.state_dir())
        .args(["acquire", "owner-b"])
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn concurrent acquire");
    thread::sleep(Duration::from_millis(250));
    assert!(acquire.try_wait().expect("poll acquire").is_none());
    assert!(refresh.wait().expect("wait refresh").success());
    assert!(acquire.wait().expect("wait acquire").success());
    let lease = harness.read_lease();
    assert_eq!(lease["generation"], 2);
    assert_eq!(lease["owner_uuid"], "owner-b");
}

#[test]
fn watchdog_waits_for_busy_kernel_to_become_idle() {
    let Some(harness) = Harness::new("watchdog_waits_for_busy_kernel_to_become_idle") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "busy",
        "watchdog_waits_for_busy_kernel_to_become_idle",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 3,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    thread::sleep(Duration::from_secs(3));
    assert!(
        !action_log.exists(),
        "provider action ran while the kernel was busy"
    );
    jupyter.set_state("idle");
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("action=terminate"));
}

#[test]
fn watchdog_executes_real_runpod_action_command_with_action_in_dollar_one() {
    let Some(harness) =
        Harness::new("watchdog_executes_real_runpod_action_command_with_action_in_dollar_one")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(&harness, "idle", "real_runpod_action_command");
    let action_log = harness.temp.path().join("runpod-actions.log");
    harness.install_command(
        "runpodctl",
        "#!/bin/sh\n[ \"$1\" = config ] && exit 0\nprintf '%s\\n' \"$*\" >> \"$RK_ACTION_LOG\"\n",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_command = remote_kernels::runtime::runpod::watchdog_action_command();
    let options = InstallOptions {
        action_command: Some(&action_command),
        ..InstallOptions::default()
    };
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog =
        install_watchdog_with_options(&harness, &jupyter, &action_log, &config, &options);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("remove pod test-pod"), "{log}");
}

#[test]
fn watchdog_executes_real_vast_compound_halt_command() {
    let Some(harness) = Harness::new("watchdog_executes_real_vast_compound_halt_command") else {
        return;
    };
    let jupyter = FakeJupyter::start(&harness, "idle", "real_vast_halt_command");
    let action_log = harness.temp.path().join("vast-actions.log");
    harness.install_command(
        "shutdown",
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RK_ACTION_LOG\"\n",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_command = remote_kernels::runtime::vast::VastRuntime::halt_command("root");
    let options = InstallOptions {
        action_command: Some(&action_command),
        ..InstallOptions::default()
    };
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "stop",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog =
        install_watchdog_with_options(&harness, &jupyter, &action_log, &config, &options);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("-h now"), "{log}");
}

#[test]
fn stale_generation_heartbeat_cannot_keep_new_owner_alive() {
    let Some(harness) = Harness::new("stale_generation_heartbeat_cannot_keep_new_owner_alive")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "stale_generation_heartbeat_cannot_keep_new_owner_alive",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["acquire", "owner-b"]);
    fs::write(
        harness.state_dir().join("heartbeat"),
        format!("1 {}\n", now_epoch()),
    )
    .expect("write stale-generation heartbeat");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let _ = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert_eq!(harness.read_lease()["state"], "finalizing");
}

#[test]
fn finalize_failure_downgrades_terminate_to_stop_in_outcome() {
    let Some(harness) = Harness::new("finalize_failure_downgrades_terminate_to_stop_in_outcome")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "finalize_failure_downgrades_terminate_to_stop_in_outcome",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_log = harness.temp.path().join("actions.log");
    let finalize = support_script("finalize-fail.sh");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 3,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: &finalize,
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("action=stop"));
    let outcome: Value = serde_json::from_slice(
        &fs::read(harness.state_dir().join("outcome.json")).expect("read outcome"),
    )
    .expect("valid outcome JSON");
    assert_eq!(outcome["action"], "stop");
    assert_eq!(outcome["finalize_exit"], 42);
}

#[test]
fn pending_downloads_downgrade_terminate_intent_to_stop() {
    let Some(harness) = Harness::new("pending_downloads_downgrade_terminate_intent_to_stop") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "pending_downloads_downgrade_terminate_intent_to_stop",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    fs::write(
        harness.state_dir().join("intent.json"),
        b"{\"downloads_pending\":true,\"then\":\"terminate\"}\n",
    )
    .expect("write intent");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("action=stop"));
}

#[test]
fn budget_grace_expiry_stops_even_while_kernel_is_busy() {
    let Some(harness) = Harness::new("budget_grace_expiry_stops_even_while_kernel_is_busy") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "busy",
        "budget_grace_expiry_stops_even_while_kernel_is_busy",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    fs::write(
        harness.state_dir().join("budget_deadline"),
        format!("{}\n", now_epoch().saturating_sub(1)),
    )
    .expect("write budget deadline");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 300,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 5,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(8));
    assert!(log.contains("action=stop"));
    let lease = harness.read_lease();
    assert_eq!(lease["state"], "finalizing");
    assert_eq!(lease["arm_reason"], "budget");
}

#[test]
fn outcome_marker_exists_before_provider_action_runs() {
    let Some(harness) = Harness::new("outcome_marker_exists_before_provider_action_runs") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "outcome_marker_exists_before_provider_action_runs",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("outcome=present action=terminate"));
}

#[test]
fn watchdog_reinstall_keeps_one_supervisor_and_one_action() {
    let Some(harness) = Harness::new("watchdog_reinstall_keeps_one_supervisor_and_one_action")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "busy",
        "watchdog_reinstall_keeps_one_supervisor_and_one_action",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    stale_heartbeat(&harness, 1);
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _first = install_watchdog(&harness, &jupyter, &action_log, &config);
    let _second = install_watchdog(&harness, &jupyter, &action_log, &config);
    thread::sleep(Duration::from_secs(2));
    jupyter.set_state("idle");
    let _ = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    thread::sleep(Duration::from_secs(2));
    let log = fs::read_to_string(&action_log).expect("action log");
    assert_eq!(
        log.lines().count(),
        1,
        "reinstall launched a duplicate actor"
    );
}

#[test]
fn refresh_cancels_disconnect_arm_and_prevents_finalize() {
    let Some(harness) = Harness::new("refresh_cancels_disconnect_arm_and_prevents_finalize") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "busy",
        "refresh_cancels_disconnect_arm_and_prevents_finalize",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 3,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    thread::sleep(Duration::from_millis(1_200));
    harness.lease_ok(&["refresh", "1", "owner-a"]);
    assert_eq!(harness.read_lease()["state"], "active");
    jupyter.set_state("idle");
    for _ in 0..4 {
        harness.lease_ok(&["refresh", "1", "owner-a"]);
        thread::sleep(Duration::from_millis(800));
    }
    assert_eq!(harness.read_lease()["state"], "active");
    assert!(
        !action_log.exists(),
        "a refreshing owner was finalized after a transient disconnect arm"
    );
}

#[test]
fn budget_escalates_during_busy_disconnect_drain() {
    let Some(harness) = Harness::new("budget_escalates_during_busy_disconnect_drain") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "busy",
        "budget_escalates_during_busy_disconnect_drain",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    harness.lease_ok(&["arm", "1", "disconnect"]);
    fs::write(
        harness.state_dir().join("budget_deadline"),
        format!("{}\n", now_epoch() + 2),
    )
    .expect("write budget deadline");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 300,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(10));
    assert!(log.contains("action=stop"));
    let lease = harness.read_lease();
    assert_eq!(lease["state"], "finalizing");
    assert_eq!(lease["arm_reason"], "budget");
}

#[test]
fn budget_keep_intent_floors_to_stop_and_preserves_prior_outcome() {
    let Some(harness) =
        Harness::new("budget_keep_intent_floors_to_stop_and_preserves_prior_outcome")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "budget_keep_intent_floors_to_stop_and_preserves_prior_outcome",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    fs::write(
        harness.state_dir().join("budget_deadline"),
        format!("{}\n", now_epoch().saturating_sub(1)),
    )
    .expect("write budget deadline");
    fs::write(
        harness.state_dir().join("intent.json"),
        b"{\"downloads_pending\":false,\"then\":\"keep\"}\n",
    )
    .expect("write keep intent");
    let prior_outcome = b"{\"op_id\":\"prior-ambiguous\"}\n";
    fs::write(harness.state_dir().join("outcome.json"), prior_outcome)
        .expect("write prior outcome");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 300,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let options = InstallOptions {
        storage_rate: Some("0.125"),
        enter_pause: None,
        action_command: None,
    };
    let _watchdog =
        install_watchdog_with_options(&harness, &jupyter, &action_log, &config, &options);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(8));
    assert!(log.contains("action=stop"));
    assert_eq!(
        fs::read(harness.state_dir().join("outcome.json")).expect("read prior outcome"),
        prior_outcome
    );
    let secondary = fs::read_dir(harness.state_dir())
        .expect("read state directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name != "outcome.json" && name.starts_with("outcome.") && name.ends_with(".json")
            })
        })
        .expect("secondary budget outcome");
    let marker: Value =
        serde_json::from_slice(&fs::read(secondary).expect("read secondary outcome"))
            .expect("valid secondary outcome");
    assert_eq!(marker["action"], "stop");
    assert_eq!(marker["post_action_rate"].as_f64(), Some(0.125));
}

#[test]
fn downloads_false_terminate_intent_stays_terminate() {
    let Some(harness) = Harness::new("downloads_false_terminate_intent_stays_terminate") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "downloads_false_terminate_intent_stays_terminate",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    fs::write(
        harness.state_dir().join("intent.json"),
        b"{ \"downloads_pending\" : false, \"then\" : \"terminate\" }\n",
    )
    .expect("write terminate intent");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "stop",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(6));
    assert!(log.contains("action=terminate"));
    let outcome: Value = serde_json::from_slice(
        &fs::read(harness.state_dir().join("outcome.json")).expect("read outcome"),
    )
    .expect("valid outcome");
    assert_eq!(outcome["post_action_rate"], 0);
}

#[test]
fn finalize_timeout_downgrades_terminate_to_stop() {
    let Some(harness) = Harness::new("finalize_timeout_downgrades_terminate_to_stop") else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "finalize_timeout_downgrades_terminate_to_stop",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    let action_log = harness.temp.path().join("actions.log");
    let finalize = support_script("finalize-hang.sh");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 1,
        default_action: "terminate",
        finalize_cmd: &finalize,
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    let log = wait_for_log_lines(&action_log, 1, Duration::from_secs(7));
    assert!(log.contains("action=stop"));
    let outcome: Value = serde_json::from_slice(
        &fs::read(harness.state_dir().join("outcome.json")).expect("read outcome"),
    )
    .expect("valid outcome");
    assert_eq!(outcome["finalize_exit"], 124);
}

#[test]
fn fresh_install_without_lease_does_not_arm() {
    let Some(harness) = Harness::new("fresh_install_without_lease_does_not_arm") else {
        return;
    };
    let jupyter = FakeJupyter::start(&harness, "idle", "fresh_install_without_lease_does_not_arm");
    let action_log = harness.temp.path().join("actions.log");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 1,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 2,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let _watchdog = install_watchdog(&harness, &jupyter, &action_log, &config);
    thread::sleep(Duration::from_secs(3));
    assert!(!harness.state_dir().join("lease.json").exists());
    assert!(!action_log.exists());
}

#[test]
fn explicit_enter_finalizing_accepts_active_and_budget_armed() {
    let Some(active) = Harness::new("explicit_enter_finalizing_accepts_active_and_budget_armed")
    else {
        return;
    };
    active.lease_ok(&["acquire", "owner-a"]);
    active.lease_ok(&["enter-finalizing", "1", "active-op", "stop"]);
    assert_eq!(active.read_lease()["state"], "finalizing");

    let Some(budget) = Harness::new("explicit_enter_finalizing_accepts_active_and_budget_armed")
    else {
        return;
    };
    let future_deadline = (now_epoch() + 300).to_string();
    budget.lease_ok(&["acquire", "owner-a"]);
    budget.lease_ok(&["arm", "1", "budget", &future_deadline]);
    budget.lease_ok(&["enter-finalizing", "1", "budget-op", "terminate"]);
    let lease = budget.read_lease();
    assert_eq!(lease["state"], "finalizing");
    assert_eq!(lease["action"], "terminate");
}

#[test]
fn acquire_is_refused_between_enter_finalizing_and_provider_exec() {
    let Some(harness) =
        Harness::new("acquire_is_refused_between_enter_finalizing_and_provider_exec")
    else {
        return;
    };
    let jupyter = FakeJupyter::start(
        &harness,
        "idle",
        "acquire_is_refused_between_enter_finalizing_and_provider_exec",
    );
    harness.lease_ok(&["acquire", "owner-a"]);
    let action_log = harness.temp.path().join("actions.log");
    let pause_marker = harness.temp.path().join("entered-finalizing");
    let config = WatchdogConfig {
        stale_secs: 1,
        grace_secs: 2,
        finalize_wait_secs: 0,
        finalize_timeout_secs: 3,
        default_action: "terminate",
        finalize_cmd: Path::new("-"),
    };
    let options = InstallOptions {
        storage_rate: None,
        enter_pause: Some((3, &pause_marker)),
        action_command: None,
    };
    let _watchdog =
        install_watchdog_with_options(&harness, &jupyter, &action_log, &config, &options);
    wait_for(&pause_marker, Duration::from_secs(6));
    assert!(!action_log.exists());
    let acquire = harness.lease(&["acquire", "owner-b"]);
    assert_eq!(acquire.status.code(), Some(REFUSED));
    assert_eq!(harness.read_lease()["state"], "finalizing");
    let _ = wait_for_log_lines(&action_log, 1, Duration::from_secs(5));
}

#[test]
fn prerequisite_check_has_distinct_missing_flock_exit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).expect("empty PATH dir");
    let output = Command::new("/bin/bash")
        .arg(machine_script("rk-watchdog.sh"))
        .arg(temp.path().join("state"))
        .arg("check-prereqs")
        .env("PATH", &empty_path)
        .output()
        .expect("run prerequisite check");
    assert_eq!(output.status.code(), Some(NO_FLOCK));
}
