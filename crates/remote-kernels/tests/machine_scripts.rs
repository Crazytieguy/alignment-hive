//! Behavior tests for the machine-side supervisor, run locally under bash. The
//! helpers in `scripts/machine/test-support` stand in for Jupyter's kernel list
//! and save commands, and the "provider" is a log file.

use std::ffi::OsString;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

fn machine_script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/machine")
        .join(name)
}

fn support_script(name: &str) -> PathBuf {
    machine_script("test-support").join(name)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn eventually(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

/// One simulated machine: a state dir, a Jupyter kernel list and a provider log.
struct Machine {
    temp: TempDir,
    path: OsString,
    jupyter: Child,
    kernel_state_file: PathBuf,
    port: u16,
    settings: Settings,
    env: Vec<(&'static str, &'static str)>,
}

#[derive(Clone)]
struct Settings {
    stale_secs: u64,
    grace_secs: u64,
    wait_secs: u64,
    save_timeout_secs: u64,
    policy: &'static str,
    /// The save for both actions, unless `terminate_save` overrides it.
    save: Option<String>,
    terminate_save: Option<String>,
    action: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stale_secs: 1,
            grace_secs: 2,
            wait_secs: 0,
            save_timeout_secs: 5,
            policy: "terminate",
            save: None,
            terminate_save: None,
            action: None,
        }
    }
}

impl Machine {
    fn new(kernel_state: &str, settings: Settings) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).expect("create test bin");
        let mut path = OsString::from(bin.as_os_str());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let kernel_state_file = temp.path().join("kernel-state");
        fs::write(&kernel_state_file, kernel_state).expect("write kernel state");
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve local port");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let jupyter = Command::new("python3")
            .arg(support_script("fake-jupyter.py"))
            .arg(port.to_string())
            .arg(&kernel_state_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start fake Jupyter");
        eventually("fake Jupyter", Duration::from_secs(30), || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        Self {
            temp,
            path,
            jupyter,
            kernel_state_file,
            port,
            settings,
            env: Vec::new(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn workdir(&self) -> PathBuf {
        self.temp.path().join("work")
    }

    fn set_kernel_state(&self, state: &str) {
        fs::write(&self.kernel_state_file, state).expect("update kernel state");
    }

    fn stub_command(&self, name: &str, body: &str) {
        let path = self.temp.path().join("bin").join(name);
        fs::write(&path, body).expect("write command stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod command stub");
    }

    fn log_path(&self) -> PathBuf {
        self.temp.path().join("actions.log")
    }

    fn actions(&self) -> Vec<String> {
        fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    /// What the install prints, with `refuse_within` and a deadline.
    fn install_full(
        &self,
        owner: &str,
        refuse_within: Option<u64>,
        deadline_secs: Option<u64>,
        no_supervisor: bool,
    ) -> String {
        fs::create_dir_all(self.dir()).expect("state dir");
        fs::create_dir_all(self.workdir()).expect("workdir");
        let settings = &self.settings;
        let action = settings
            .action
            .clone()
            .unwrap_or_else(|| format!("printf \"%s\\n\" \"$1\" >> {}", self.log_path().display()));
        let output = Command::new("bash")
            .arg(machine_script("rk-supervisor.sh"))
            .arg(self.dir())
            .arg("install")
            .arg(owner)
            .arg(refuse_within.map_or("-".to_string(), |secs| secs.to_string()))
            .arg(deadline_secs.map_or("-".to_string(), |secs| secs.to_string()))
            .arg(settings.stale_secs.to_string())
            .arg(settings.grace_secs.to_string())
            .arg(settings.wait_secs.to_string())
            .arg(settings.save_timeout_secs.to_string())
            .arg(self.port.to_string())
            .arg("test-token")
            .arg(settings.policy)
            .arg(if no_supervisor { "-" } else { action.as_str() })
            .arg(self.workdir())
            .arg(settings.save.as_deref().unwrap_or("-"))
            .arg(
                settings
                    .terminate_save
                    .as_deref()
                    .or(settings.save.as_deref())
                    .unwrap_or("-"),
            )
            .env("PATH", &self.path)
            .env("BASH_ENV", self.temp.path().join("bash-env"))
            .env("RK_SUPERVISOR_POLL_SECS", "0.2")
            .env("RK_SUPERVISOR_RETRY_SECS", "1")
            .env("RK_SUPERVISOR_PARENT", std::process::id().to_string())
            .env("RK_FAKE_JUPYTER_STATE", &self.kernel_state_file)
            .env("RUNPOD_POD_ID", "test-pod")
            .env("RUNPOD_API_KEY", "test-key")
            .envs(self.env.iter().copied())
            .output()
            .expect("run install");
        assert!(
            output.status.success(),
            "install failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn install(&self) {
        // Non-interactive shells on the machine find the same stubs.
        fs::write(
            self.temp.path().join("bash-env"),
            format!("export PATH={}/bin:$PATH\n", self.temp.path().display()),
        )
        .expect("write bash environment");
        assert_eq!(self.install_full("me", None, None, false), "installed");
    }

    fn beat(&self) {
        let path = self.dir().join("alive");
        fs::write(&path, format!("me {}\n", now_epoch())).expect("write alive");
    }

    fn set_deadline(&self, epoch: u64) {
        fs::write(self.dir().join("deadline"), format!("{epoch}\n")).expect("write deadline");
    }

    fn write_intent(&self, downloads_pending: bool, then: &str) {
        fs::write(
            self.dir().join("intent.json"),
            format!("{{\"downloads_pending\":{downloads_pending},\"then\":\"{then}\"}}\n"),
        )
        .expect("write intent");
    }

    fn supervisor_pid(&self) -> String {
        fs::read_to_string(self.dir().join("supervisor.pid"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir().join("supervisor.log")).unwrap_or_default()
    }

    fn wait_for_action(&self, expected: &str) {
        let log = self.log_path();
        eventually(
            &format!("action {expected}"),
            Duration::from_secs(20),
            || fs::read_to_string(&log).is_ok_and(|content| content.lines().next().is_some()),
        );
        assert_eq!(
            self.actions()[0],
            expected,
            "supervisor log: {}",
            self.log()
        );
    }

    /// Nothing acts for this long.
    fn assert_no_action_for(&self, duration: Duration) {
        thread::sleep(duration);
        assert!(
            self.actions().is_empty(),
            "unexpected actions {:?}; supervisor log: {}",
            self.actions(),
            self.log()
        );
    }

    fn save_command(&self, script: &str) -> String {
        script.replace(
            "$MARK",
            &self.temp.path().join("save-ran").display().to_string(),
        )
    }

    fn save_ran(&self) -> bool {
        self.temp.path().join("save-ran").exists()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let pid = self.supervisor_pid();
        if !pid.is_empty() {
            let _ = Command::new("kill")
                .arg(&pid)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.jupyter.kill();
        let _ = self.jupyter.wait();
    }
}

fn is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

const SETTLE: Duration = Duration::from_millis(1500);

fn with(settings: impl FnOnce(&mut Settings)) -> Settings {
    let mut result = Settings::default();
    settings(&mut result);
    result
}

// ---- the stale path

#[test]
fn a_quiet_idle_machine_cleans_itself_up_per_its_policy() {
    let machine = Machine::new("idle", Settings::default());
    machine.install();
    machine.wait_for_action("terminate");

    let stopping = Machine::new("idle", with(|s| s.policy = "stop"));
    stopping.install();
    stopping.wait_for_action("stop");
}

#[test]
fn beats_keep_it_from_doing_anything() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    let end = Instant::now() + Duration::from_secs(4);
    while Instant::now() < end {
        machine.beat();
        thread::sleep(Duration::from_millis(200));
    }
    assert!(machine.actions().is_empty(), "{}", machine.log());
}

#[test]
fn a_busy_kernel_holds_the_drain_until_it_goes_idle() {
    let machine = Machine::new("busy", Settings::default());
    machine.install();
    machine.assert_no_action_for(Duration::from_secs(2));
    machine.set_kernel_state("idle");
    machine.wait_for_action("terminate");
}

#[test]
fn a_drain_gives_up_after_its_wait() {
    let machine = Machine::new("busy", with(|s| s.wait_secs = 2));
    machine.install();
    machine.wait_for_action("terminate");
}

/// The core promise to a laptop that closes and opens again.
#[test]
fn a_fresh_alive_during_the_drain_cancels_everything() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 2));
    machine.install();
    eventually("the drain", Duration::from_secs(10), || {
        machine.log().contains("no server")
    });
    machine.beat();
    machine.set_kernel_state("idle");
    for _ in 0..10 {
        machine.beat();
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(machine.actions().len(), 0, "{}", machine.log());
}

#[test]
fn a_fresh_alive_after_the_save_cancels_the_action() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 2;
            s.save = Some("sleep 2".to_string());
        }),
    );
    machine.install();
    eventually("the save", Duration::from_secs(10), || {
        machine.log().contains("save starting")
    });
    let end = Instant::now() + Duration::from_secs(4);
    while Instant::now() < end {
        machine.beat();
        thread::sleep(Duration::from_millis(100));
    }
    assert!(machine.actions().is_empty(), "{}", machine.log());
    assert!(machine.log().contains("save finished"), "{}", machine.log());
}

#[test]
fn a_damaged_alive_file_is_no_sooner_than_a_silent_server() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 3));
    machine.install();
    fs::write(machine.dir().join("alive"), "garbage\n").unwrap();
    machine.assert_no_action_for(Duration::from_secs(1));
    machine.wait_for_action("terminate");
}

// ---- one action-selection rule

#[test]
fn a_failed_save_keeps_the_machine() {
    let machine = Machine::new(
        "idle",
        with(|s| s.save = Some(support_script("finalize-fail.sh").display().to_string())),
    );
    machine.install();
    machine.wait_for_action("stop");
}

#[test]
fn a_save_that_times_out_keeps_the_machine() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.save = Some(support_script("finalize-hang.sh").display().to_string());
            s.save_timeout_secs = 1;
        }),
    );
    machine.install();
    machine.wait_for_action("stop");
    assert!(machine.log().contains("exit 124"), "{}", machine.log());
}

#[test]
fn a_successful_save_runs_first_and_then_terminates() {
    let mut machine = Machine::new("idle", Settings::default());
    machine.settings.save = Some(machine.save_command("touch $MARK"));
    machine.install();
    machine.wait_for_action("terminate");
    assert!(machine.save_ran());
}

#[test]
fn the_save_runs_in_the_workdir() {
    let mut machine = Machine::new("idle", Settings::default());
    machine.settings.save = Some(machine.save_command("pwd > $MARK"));
    machine.install();
    machine.wait_for_action("terminate");
    let ran_in = fs::read_to_string(machine.temp.path().join("save-ran")).expect("save output");
    assert_eq!(
        fs::canonicalize(ran_in.trim()).expect("pwd"),
        fs::canonicalize(machine.workdir()).expect("workdir")
    );
}

#[test]
fn the_save_matches_the_action_the_plan_chose() {
    let mut machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 2;
            s.policy = "terminate";
        }),
    );
    // The policy's own save would mark "terminate"; the plan's action is stop.
    let mark = machine.temp.path().join("save-ran").display().to_string();
    machine.settings.save = Some(format!("echo stop > {mark}"));
    machine.settings.terminate_save = Some(format!("echo terminate > {mark}"));
    machine.install();
    machine.write_intent(false, "stop");
    machine.wait_for_action("stop");
    assert_eq!(
        fs::read_to_string(&mark).expect("save output").trim(),
        "stop"
    );
}

#[test]
fn pending_downloads_downgrade_terminate_and_nothing_else() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    machine.write_intent(true, "terminate");
    machine.wait_for_action("stop");

    let clean = Machine::new("idle", with(|s| s.stale_secs = 2));
    clean.install();
    clean.write_intent(false, "terminate");
    clean.wait_for_action("terminate");
}

#[test]
fn the_plan_outranks_the_policy() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 2;
            s.policy = "terminate";
        }),
    );
    machine.install();
    machine.write_intent(false, "stop");
    machine.wait_for_action("stop");
}

#[test]
fn an_unreadable_plan_means_stop() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    fs::write(machine.dir().join("intent.json"), "{oops").unwrap();
    machine.wait_for_action("stop");
}

#[test]
fn a_keep_plan_does_nothing_until_a_server_or_the_budget_says_otherwise() {
    let machine = Machine::new("idle", with(|s| s.grace_secs = 1));
    machine.install();
    machine.write_intent(false, "keep");
    machine.assert_no_action_for(Duration::from_secs(3));
    assert!(
        machine.log().contains("waiting for a server"),
        "{}",
        machine.log()
    );
    // Under a budget, keep becomes stop.
    machine.set_deadline(now_epoch());
    machine.wait_for_action("stop");
}

#[test]
fn queued_downloads_are_staged_before_a_stop_only() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    let staged = machine.temp.path().join("staged");
    fs::write(
        machine.dir().join("stage.sh"),
        format!("touch {}\n", staged.display()),
    )
    .unwrap();
    machine.write_intent(true, "stop");
    machine.wait_for_action("stop");
    assert!(staged.exists());

    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    let staged = machine.temp.path().join("staged");
    fs::write(
        machine.dir().join("stage.sh"),
        format!("touch {}\n", staged.display()),
    )
    .unwrap();
    machine.write_intent(false, "terminate");
    machine.wait_for_action("terminate");
    assert!(!staged.exists());
}

// ---- the budget path

#[test]
fn a_passed_deadline_acts_after_the_grace_even_with_a_live_server() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 2;
        }),
    );
    machine.install_full("me", None, Some(0), false);
    let started = Instant::now();
    machine.wait_for_action("terminate");
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "acted before the grace ended"
    );
}

#[test]
fn grace_expiry_stops_a_busy_machine_rather_than_deleting_unsaved_work() {
    let machine = Machine::new(
        "busy",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 2;
            s.save = Some("true".to_string());
        }),
    );
    machine.install_full("me", None, Some(0), false);
    machine.wait_for_action("stop");
    assert!(machine.log().contains("save skipped"), "{}", machine.log());
}

#[test]
fn a_busy_machine_with_no_save_configured_still_follows_its_policy() {
    let machine = Machine::new(
        "busy",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 1;
        }),
    );
    machine.install_full("me", None, Some(0), false);
    machine.wait_for_action("terminate");
}

#[test]
fn the_save_is_bounded_by_what_is_left_of_the_grace() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 2;
            s.save_timeout_secs = 60;
            s.save = Some(support_script("finalize-hang.sh").display().to_string());
        }),
    );
    machine.install_full("me", None, Some(0), false);
    let started = Instant::now();
    machine.wait_for_action("stop");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_moved_deadline_is_the_only_cancel() {
    for moment_ms in [200, 1200] {
        let machine = Machine::new(
            "idle",
            with(|s| {
                s.stale_secs = 100;
                s.grace_secs = 3;
            }),
        );
        machine.install_full("me", None, Some(0), false);
        thread::sleep(Duration::from_millis(moment_ms));
        machine.set_deadline(now_epoch() + 1000);
        machine.assert_no_action_for(Duration::from_secs(4));
    }
}

#[test]
fn a_rewritten_deadline_that_has_still_passed_does_not_restart_the_grace() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 2;
        }),
    );
    machine.install_full("me", None, Some(0), false);
    let started = Instant::now();
    while machine.actions().is_empty() {
        // What a beat does once the budget has run out.
        machine.set_deadline(now_epoch());
        thread::sleep(Duration::from_millis(100));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{}",
            machine.log()
        );
    }
    assert_eq!(machine.actions()[0], "terminate");
}

/// A laptop closed on a long run: the drain has no limit of its own, but the
/// budget does.
#[test]
fn a_stale_drain_yields_to_a_passed_deadline() {
    let machine = Machine::new(
        "busy",
        with(|s| {
            s.stale_secs = 1;
            s.grace_secs = 1;
            s.save = Some("true".to_string());
        }),
    );
    machine.install();
    thread::sleep(SETTLE);
    assert!(machine.actions().is_empty());
    machine.set_deadline(now_epoch());
    machine.wait_for_action("stop");
    assert!(
        machine.log().contains("budget deadline passed"),
        "{}",
        machine.log()
    );
}

#[test]
fn a_fresh_alive_does_not_cancel_the_budget_path() {
    let machine = Machine::new(
        "idle",
        with(|s| {
            s.stale_secs = 100;
            s.grace_secs = 1;
        }),
    );
    machine.install_full("me", None, Some(0), false);
    machine.beat();
    machine.wait_for_action("terminate");
}

// ---- acting

#[test]
fn a_failed_action_is_retried_while_the_process_lives() {
    let flag = tempfile::NamedTempFile::new().unwrap();
    let mut machine = Machine::new("idle", Settings::default());
    machine.settings.action = Some(format!(
        "printf \"%s\\n\" \"$1\" >> {}; [ ! -e {} ]",
        machine.log_path().display(),
        flag.path().display()
    ));
    machine.install();
    eventually("a second attempt", Duration::from_secs(15), || {
        machine.actions().len() >= 2
    });
    assert!(machine.log().contains("call failed"), "{}", machine.log());
    // The process carries on until the container dies under it.
    assert!(is_alive(&machine.supervisor_pid()));
}

#[test]
fn a_retry_is_cancelled_when_the_server_comes_back() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 2));
    machine.install();
    machine.wait_for_action("terminate");
    machine.beat();
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        machine.beat();
        thread::sleep(Duration::from_millis(100));
    }
    let count = machine.actions().len();
    assert!(machine.log().contains("cancelled"), "{}", machine.log());
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        machine.beat();
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(machine.actions().len(), count);
}

/// The chain deployed to `RunPod` pods takes the action in `$1`.
#[test]
fn the_real_runpod_action_command_reads_its_action() {
    let mut machine = Machine::new("idle", with(|s| s.policy = "stop"));
    machine.stub_command(
        "runpodctl",
        &format!(
            "#!/bin/sh\n[ \"$1\" = config ] && exit 0\necho \"$*\" >> {}\n",
            machine.log_path().display()
        ),
    );
    machine.settings.action = Some(remote_kernels::runtime::runpod::supervisor_action_command());
    machine.install();
    eventually("runpodctl", Duration::from_secs(15), || {
        !machine.actions().is_empty()
    });
    assert_eq!(
        machine.actions()[0],
        "stop pod test-pod",
        "{}",
        machine.log()
    );
}

/// Vast machines end themselves through vast's API with the key vast injects.
#[test]
fn the_real_vast_action_calls_the_api() {
    let real_curl = which("curl").unwrap();
    for (policy, method) in [("terminate", "DELETE"), ("stop", "PUT")] {
        let mut machine = Machine::new("idle", with(|s| s.policy = policy));
        machine.stub_command(
            "curl",
            &format!(
                "#!/bin/sh\ncase \"$*\" in *console.vast.ai*) echo \"$*\" >> {};; *) exec {} \"$@\";; esac\n",
                machine.log_path().display(),
                real_curl.display()
            ),
        );
        machine.env = vec![("CONTAINER_API_KEY", "the-key"), ("CONTAINER_ID", "123")];
        machine.settings.action =
            Some(remote_kernels::runtime::vast::VastRuntime::self_action_command("root"));
        machine.install();
        eventually("the API call", Duration::from_secs(15), || {
            !machine.actions().is_empty()
        });
        let call = &machine.actions()[0];
        assert!(call.contains(&format!("-X {method}")), "{call}");
        assert!(call.contains("Bearer the-key"), "{call}");
        assert!(
            call.contains("https://console.vast.ai/api/v0/instances/123/"),
            "{call}"
        );
        if policy == "stop" {
            assert!(call.contains("{\"state\":\"stopped\"}"), "{call}");
        }
    }
}

/// Without the key it falls back to halting, and terminate tries everything.
#[test]
fn the_vast_action_falls_back_to_a_halt() {
    let mut machine = Machine::new("idle", Settings::default());
    machine.stub_command(
        "shutdown",
        &format!(
            "#!/bin/sh\necho \"shutdown $*\" >> {}\n",
            machine.log_path().display()
        ),
    );
    machine.settings.action =
        Some(remote_kernels::runtime::vast::VastRuntime::self_action_command("root"));
    machine.install();
    eventually("shutdown", Duration::from_secs(15), || {
        !machine.actions().is_empty()
    });
    assert_eq!(machine.actions()[0], "shutdown -h now");
}

// ---- install

#[test]
fn installing_again_replaces_the_supervisor_and_keeps_one_action() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 2));
    machine.install();
    eventually("the pid file", Duration::from_secs(5), || {
        !machine.supervisor_pid().is_empty()
    });
    let first = machine.supervisor_pid();
    machine.install();
    let second = machine.supervisor_pid();
    assert_ne!(first, second);
    eventually("the old supervisor to exit", Duration::from_secs(5), || {
        !is_alive(&first)
    });
    machine.set_kernel_state("idle");
    machine.wait_for_action("terminate");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(machine.actions().len(), 1, "{}", machine.log());
}

#[test]
fn installing_over_an_old_machine_kills_its_watchdog_and_clears_its_files() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 100));
    let dir = machine.dir();
    fs::create_dir_all(&dir).unwrap();
    // A process that looks like the old watchdog on its command line.
    let watchdog = dir.join("rk-watchdog.sh");
    fs::write(&watchdog, "sleep 300\n").unwrap();
    let mut old = Command::new("bash").arg(&watchdog).spawn().unwrap();
    fs::write(dir.join("watchdog.pid"), old.id().to_string()).unwrap();
    for file in [
        "lease.json",
        "lease.lock",
        "heartbeat",
        "budget_deadline",
        "outcome.json",
        "outcome.abc.json",
        "rk-lease.sh",
    ] {
        fs::write(dir.join(file), "{\"state\":\"finalizing\"}").unwrap();
    }

    machine.install();

    eventually("the old watchdog to die", Duration::from_secs(5), || {
        old.try_wait().unwrap().is_some()
    });
    for file in [
        "watchdog.pid",
        "lease.json",
        "lease.lock",
        "heartbeat",
        "budget_deadline",
        "outcome.json",
        "outcome.abc.json",
        "rk-lease.sh",
        "rk-watchdog.sh",
    ] {
        assert!(!dir.join(file).exists(), "{file} survived");
    }
    assert!(!machine.supervisor_pid().is_empty());
}

/// A pid from before a stop can belong to anything now.
#[test]
fn a_pid_file_naming_someone_else_kills_nobody() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 100));
    fs::create_dir_all(machine.dir()).unwrap();
    let mut innocent = Command::new("sleep").arg("30").spawn().unwrap();
    fs::write(
        machine.dir().join("watchdog.pid"),
        innocent.id().to_string(),
    )
    .unwrap();
    fs::write(
        machine.dir().join("supervisor.pid"),
        innocent.id().to_string(),
    )
    .unwrap();
    machine.install();
    thread::sleep(Duration::from_millis(200));
    assert!(innocent.try_wait().unwrap().is_none());
    innocent.kill().unwrap();
    innocent.wait().unwrap();
}

#[test]
fn install_refuses_a_fresh_foreign_alive_and_changes_nothing() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 100));
    fs::create_dir_all(machine.dir()).unwrap();
    let alive = format!("other {}\n", now_epoch() - 5);
    fs::write(machine.dir().join("alive"), &alive).unwrap();

    let output = machine.install_full("me", Some(180), None, false);
    assert!(output.starts_with("foreign other "), "{output}");
    assert_eq!(
        fs::read_to_string(machine.dir().join("alive")).unwrap(),
        alive
    );
    assert!(machine.supervisor_pid().is_empty());

    // Its own alive, a stale one, or no refusal asked for: all install.
    assert_eq!(
        machine.install_full("other", Some(180), None, false),
        "installed"
    );
    fs::write(
        machine.dir().join("alive"),
        format!("other {}\n", now_epoch() - 500),
    )
    .unwrap();
    assert_eq!(
        machine.install_full("me", Some(180), None, false),
        "installed"
    );
    fs::write(
        machine.dir().join("alive"),
        format!("other {}\n", now_epoch()),
    )
    .unwrap();
    assert_eq!(machine.install_full("me", None, None, false), "installed");
}

#[test]
fn install_writes_alive_and_deadline_from_the_machine_clock() {
    let machine = Machine::new("busy", with(|s| s.stale_secs = 100));
    fs::create_dir_all(machine.dir()).unwrap();
    machine.install_full("me", None, Some(1000), false);
    let alive = fs::read_to_string(machine.dir().join("alive")).unwrap();
    let (owner, ts) = alive.trim().split_once(' ').unwrap();
    assert_eq!(owner, "me");
    let ts: u64 = ts.parse().unwrap();
    assert!(now_epoch().abs_diff(ts) < 5);
    let deadline: u64 = fs::read_to_string(machine.dir().join("deadline"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(deadline.abs_diff(ts + 1000) < 5);

    // No budget on the next install: the old deadline must not survive.
    machine.install_full("me", None, None, false);
    assert!(!machine.dir().join("deadline").exists());
}

/// `cleanup = disabled` and Kubernetes: `alive` for takeover detection, nothing
/// that could act.
#[test]
fn no_action_command_means_alive_only_and_no_supervisor() {
    let machine = Machine::new("idle", with(|s| s.stale_secs = 1));
    fs::create_dir_all(machine.dir()).unwrap();
    machine.install_full("me", None, None, true);
    assert!(machine.dir().join("alive").exists());
    assert!(!machine.dir().join("supervisor.pid").exists());
    assert!(!machine.dir().join("deadline").exists());
    machine.assert_no_action_for(SETTLE);
}

#[test]
fn a_machine_missing_what_it_needs_says_so_and_flock_is_not_on_the_list() {
    // A PATH with bash, but no curl or flock.
    let mut machine = Machine::new("idle", with(|s| s.stale_secs = 100));
    fs::create_dir_all(machine.dir()).unwrap();
    let bin = machine.temp.path().join("bare-bin");
    fs::create_dir(&bin).unwrap();
    for tool in [
        "bash", "cat", "date", "mkdir", "mv", "rm", "tr", "ps", "touch", "basename", "nohup",
        "sed", "sleep",
    ] {
        if let Ok(found) = which(tool) {
            std::os::unix::fs::symlink(found, bin.join(tool)).unwrap();
        }
    }
    machine.path = bin.into_os_string();
    let output = machine.install_full("me", None, None, false);
    assert_eq!(output, "unsupervisable the machine lacks curl");

    let curl = which("curl").unwrap();
    std::os::unix::fs::symlink(curl, Path::new(&machine.path).join("curl")).unwrap();
    assert_eq!(machine.install_full("me", None, None, false), "installed");
}

fn which(tool: &str) -> Result<PathBuf, ()> {
    let output = Command::new("which").arg(tool).output().map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}
