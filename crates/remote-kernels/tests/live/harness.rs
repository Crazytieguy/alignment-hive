//! Scenario scaffolding: isolation (a scratch dir and a machine-name prefix per
//! scenario), the money rails (a spend meter per scenario, a ledger across
//! runs, a hard limit), guaranteed cleanup at the provider, and the results a
//! run leaves behind.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::mcp::{Launch, Reply, Server, binary_under_test};
use crate::providers::{self, Machine, Provider, scrub};

pub const MIN: Duration = Duration::from_secs(60);

/// The hard rule: stop once all live runs together pass this. Overridable with
/// `RK_LIVE_SPEND_LIMIT`, but only ever downward by default usage.
const DEFAULT_SPEND_LIMIT: f64 = 2.50;

/// How often the meter re-reads the provider's inventory.
const METER_EVERY: Duration = Duration::from_secs(20);

/// Secure-cloud GPUs, cheapest first (about $0.24-0.28/hr), with one dearer
/// card at the end ($0.50/hr) because the cheap ones run out together.
pub const RUNPOD_GPUS: &[&str] = &[
    "NVIDIA RTX 2000 Ada Generation",
    "NVIDIA RTX A4000",
    "NVIDIA RTX A4500",
    "NVIDIA RTX A5000",
    "NVIDIA RTX 4000 Ada Generation",
    "NVIDIA GeForce RTX 3090",
];

/// A large image few hosts should hold, for the pull scenarios. Override with
/// `RK_LIVE_COLD_IMAGE` when a host has it cached and the pull ends too soon.
pub const COLD_IMAGE: &str = "runpod/pytorch:1.6.0-rc.146-cu1290-torch2130-ubuntu2404";

/// Where runs keep logs, scratch projects, the ledger and results.
pub fn live_dir() -> PathBuf {
    std::env::var_os("RK_LIVE_DIR")
        .map_or_else(|| std::env::temp_dir().join("rk-live"), PathBuf::from)
}

/// One id per test process. Machine names start `rklive-<run>-`, which is what
/// makes them this run's and nobody else's.
pub fn run_id() -> &'static str {
    static RUN: OnceLock<String> = OnceLock::new();
    RUN.get_or_init(|| {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let _ = std::fs::create_dir_all(live_dir());
        append_jsonl("runs.jsonl", &json!({"run": id, "at": now_epoch()}));
        id
    })
}

pub fn run_prefix(run: &str) -> String {
    format!("rklive-{run}-")
}

pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn append_jsonl(file: &str, value: &Value) {
    let path = live_dir().join(file);
    if let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(out, "{value}");
    }
}

pub fn read_jsonl(file: &str) -> Vec<Value> {
    std::fs::read_to_string(live_dir().join(file))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub fn spend_limit() -> f64 {
    std::env::var("RK_LIVE_SPEND_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SPEND_LIMIT)
}

/// Spend across every live run so far: finished scenarios from the ledger,
/// plus each scenario still running, whatever process it is in.
pub fn total_spend() -> f64 {
    let settled: f64 = read_jsonl("ledger.jsonl")
        .iter()
        .filter_map(|row| row.get("usd").and_then(Value::as_f64))
        .sum();
    let inflight: f64 = std::fs::read_dir(live_dir().join("inflight"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                .filter_map(|text| text.trim().parse::<f64>().ok())
                .sum()
        })
        .unwrap_or(0.0);
    settled + inflight
}

/// Record spend that happened outside a scenario (the provider spikes).
pub fn add_to_ledger(what: &str, usd: f64) {
    let _ = std::fs::create_dir_all(live_dir());
    append_jsonl(
        "ledger.jsonl",
        &json!({"run": "manual", "scenario": what, "usd": usd, "at": now_epoch()}),
    );
}

/// A scenario that could not reach the thing it tests (no capacity).
struct Inconclusive(String);

/// Keep panic messages by thread, so the results file carries the failure text.
fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut map) = panic_messages().lock() {
                map.insert(std::thread::current().id(), info.to_string());
            }
            default_hook(info);
        }));
    });
}

fn panic_messages() -> &'static Mutex<HashMap<std::thread::ThreadId, String>> {
    static MESSAGES: OnceLock<Mutex<HashMap<std::thread::ThreadId, String>>> = OnceLock::new();
    MESSAGES.get_or_init(Mutex::default)
}

/// A scenario's limits. The deadline ends a hung run's machines.
#[derive(Clone, Copy)]
pub struct Limits {
    pub minutes: u64,
    pub providers: &'static [Provider],
}

pub const RUNPOD_ONLY: &[Provider] = &[Provider::Runpod];
pub const VAST_ONLY: &[Provider] = &[Provider::Vast];

struct Meter {
    spent: Mutex<f64>,
    per_machine: Mutex<HashMap<String, (String, f64, f64)>>,
    stop: AtomicBool,
    tripped: Mutex<Option<String>>,
}

pub struct Scenario {
    pub name: String,
    /// Every machine this scenario creates has a name starting with this.
    pub prefix: String,
    pub dir: PathBuf,
    pub log_dir: PathBuf,
    pub keys_root: PathBuf,
    providers: &'static [Provider],
    t0: Instant,
    failures: Mutex<Vec<String>>,
    timings: Mutex<Vec<(String, f64)>>,
    notes: Mutex<Vec<String>>,
    seen: Mutex<HashMap<String, (Provider, String)>>,
    meter: Arc<Meter>,
    meter_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    servers: Mutex<Vec<Arc<Server>>>,
    volumes: Mutex<Vec<String>>,
    cleaned: AtomicBool,
}

/// Run one scenario under the rails, then clean up whatever happened: a panic,
/// recorded failures or an early "inconclusive" all land here.
pub fn run_scenario(name: &str, limits: Limits, body: impl FnOnce(&Scenario)) {
    install_panic_hook();
    let scenario = Scenario::new(name, limits);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&scenario)));
    scenario.finish(outcome);
}

impl Scenario {
    fn new(name: &str, limits: Limits) -> Self {
        let run = run_id();
        let total = total_spend();
        let limit = spend_limit();
        assert!(
            total < limit,
            "SPEND LIMIT: live runs have spent ${total:.3} of ${limit:.2}; refusing to start {name}"
        );
        let slug = name.to_lowercase();
        // The trailing dash keeps L1's machines apart from L10's within a run.
        let prefix = format!("{}{slug}-", run_prefix(run));
        let dir = live_dir().join(run).join(&slug);
        let log_dir = dir.join("logs");
        let keys_root = dir.join("keys");
        for path in [&log_dir, &keys_root, &live_dir().join("inflight")] {
            std::fs::create_dir_all(path).expect("scratch dirs");
        }
        let meter = Arc::new(Meter {
            spent: Mutex::new(0.0),
            per_machine: Mutex::default(),
            stop: AtomicBool::new(false),
            tripped: Mutex::new(None),
        });
        let scenario = Self {
            name: name.to_string(),
            prefix,
            dir,
            log_dir,
            keys_root,
            providers: limits.providers,
            t0: Instant::now(),
            failures: Mutex::default(),
            timings: Mutex::default(),
            notes: Mutex::default(),
            seen: Mutex::default(),
            meter,
            meter_thread: Mutex::new(None),
            servers: Mutex::default(),
            volumes: Mutex::default(),
            cleaned: AtomicBool::new(false),
        };
        scenario.start_meter(limits.minutes);
        scenario.say(&format!(
            "start: prefix {} binary {} ledger total ${total:.3}",
            scenario.prefix,
            binary_under_test().display()
        ));
        scenario
    }

    fn inflight_path(&self) -> PathBuf {
        live_dir()
            .join("inflight")
            .join(format!("{}-{}", run_id(), self.name))
    }

    fn start_meter(&self, minutes: u64) {
        let meter = Arc::clone(&self.meter);
        let prefix = self.prefix.clone();
        let providers = self.providers;
        let inflight = self.inflight_path();
        let name = self.name.clone();
        let deadline = Instant::now() + Duration::from_secs(minutes * 60);
        let handle = std::thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                // Wake often so finish() never waits long, tick on the interval.
                for _ in 0..METER_EVERY.as_secs() {
                    if meter.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                let dt = last.elapsed().as_secs_f64();
                last = Instant::now();
                let mut live = Vec::new();
                for provider in providers {
                    match providers::list(*provider, &prefix) {
                        Ok(machines) => live.extend(machines),
                        Err(error) => eprintln!("[{name}] meter: list failed: {error}"),
                    }
                }
                accrue(&meter, &live, dt);
                let spent = *meter.spent.lock().expect("meter");
                let _ = std::fs::write(&inflight, format!("{spent}"));
                let tripped = if total_spend() > spend_limit() {
                    Some(format!(
                        "SPEND LIMIT passed (${:.3} of ${:.2})",
                        total_spend(),
                        spend_limit()
                    ))
                } else if Instant::now() > deadline {
                    Some(format!("scenario deadline of {minutes} min passed"))
                } else {
                    None
                };
                if let Some(reason) = tripped {
                    eprintln!("[{name}] ABORT: {reason}; terminating this scenario's machines");
                    *meter.tripped.lock().expect("meter") = Some(reason);
                    for machine in live {
                        if let Err(error) = providers::destroy(&machine) {
                            eprintln!("[{name}] abort: destroy failed: {error}");
                        }
                    }
                }
            }
        });
        *self.meter_thread.lock().expect("meter thread") = Some(handle);
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    /// Print one timestamped line for this scenario.
    pub fn say(&self, message: &str) {
        let elapsed = self.t0.elapsed().as_secs_f64();
        eprintln!("[{} {elapsed:7.1}s] {}", self.name, scrub(message));
    }

    /// Record a timing for the results table.
    pub fn timing(&self, step: &str, secs: f64) {
        self.say(&format!("timing {step}: {secs:.1}s"));
        self.timings
            .lock()
            .expect("timings")
            .push((step.to_string(), secs));
    }

    pub fn note(&self, note: &str) {
        self.say(&format!("note: {note}"));
        self.notes.lock().expect("notes").push(scrub(note));
    }

    /// A behavior check. A failure is recorded and the scenario carries on, so
    /// one finding doesn't throw away the money already spent on the rest.
    pub fn check(&self, ok: bool, what: &str) {
        if ok {
            self.say(&format!("ok: {what}"));
        } else {
            self.say(&format!("FAILED: {what}"));
            self.failures.lock().expect("failures").push(scrub(what));
        }
    }

    /// Give up on this scenario with a clear reason, without failing it.
    pub fn inconclusive(&self, reason: &str) -> ! {
        self.say(&format!("INCONCLUSIVE: {reason}"));
        std::panic::resume_unwind(Box::new(Inconclusive(scrub(reason))));
    }

    /// Panic if the meter ended this scenario's machines.
    pub fn checkpoint(&self) {
        if let Some(reason) = self.meter.tripped.lock().expect("meter").clone() {
            panic!("{reason}");
        }
    }

    /// Sleep in slices, staying responsive to the rails.
    pub fn sleep(&self, how_long: Duration) {
        let end = Instant::now() + how_long;
        while Instant::now() < end {
            self.checkpoint();
            std::thread::sleep(
                end.saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(5)),
            );
        }
    }

    /// Poll until `probe` returns a value, or `None` at the timeout.
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        every: Duration,
        mut probe: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let end = Instant::now() + timeout;
        loop {
            self.checkpoint();
            if let Some(value) = probe() {
                return Some(value);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(every);
        }
    }

    // --- projects and servers ---

    /// A scratch project with its own config. `variant` keeps variants' machine
    /// names apart under the scenario's prefix.
    pub fn project(&self, variant: &str, config: &Config) -> PathBuf {
        let path = self.dir.join(format!("project-{variant}"));
        std::fs::create_dir_all(&path).expect("project dir");
        let name = format!("{}{variant}", self.prefix);
        std::fs::write(path.join("remote-kernels.toml"), config.render(&name))
            .expect("project config");
        path
    }

    pub fn server(&self, project: &Path, tag: &str, session: &str) -> Arc<Server> {
        self.server_with(project, tag, session, &Launch::default())
    }

    pub fn server_with(
        &self,
        project: &Path,
        tag: &str,
        session: &str,
        launch: &Launch,
    ) -> Arc<Server> {
        let server = Server::spawn(
            project,
            &self.log_dir,
            &self.keys_root,
            tag,
            session,
            launch,
        )
        .unwrap_or_else(|error| panic!("server {tag}: {error}"));
        self.say(&format!(
            "server {tag}: pid {} session {session}, handshake {:.2}s",
            server.pid(),
            server.handshake_secs
        ));
        let server = Arc::new(server);
        self.servers
            .lock()
            .expect("servers")
            .push(Arc::clone(&server));
        server
    }

    pub fn session(&self, who: &str) -> String {
        format!(
            "{}-{who}-{}",
            self.prefix,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        )
    }

    // --- provider views ---

    /// Every machine of this scenario the providers list right now.
    pub fn machines(&self) -> Vec<Machine> {
        let mut all = Vec::new();
        for provider in self.providers {
            match providers::list(*provider, &self.prefix) {
                Ok(machines) => all.extend(machines),
                Err(error) => self.say(&format!("provider list failed: {error}")),
            }
        }
        let mut seen = self.seen.lock().expect("seen");
        for machine in &all {
            seen.insert(machine.id.clone(), (machine.provider, machine.name.clone()));
        }
        all
    }

    pub fn machines_named(&self, variant: &str) -> Vec<Machine> {
        let name = format!("{}{variant}", self.prefix);
        self.machines()
            .into_iter()
            .filter(|machine| machine.name.starts_with(&name))
            .collect()
    }

    /// The machine this variant started, once the provider lists it.
    pub fn wait_machine(&self, variant: &str, timeout: Duration) -> Option<Machine> {
        self.wait_for(timeout, Duration::from_secs(3), || {
            self.machines_named(variant).into_iter().next()
        })
    }

    /// Re-read one machine. `None` means the provider says it is gone; a failed
    /// query retries rather than reading as gone.
    pub fn refresh(&self, machine: &Machine) -> Option<Machine> {
        for _ in 0..5 {
            match providers::get(machine.provider, &machine.id) {
                Ok(found) => return found,
                Err(error) => {
                    self.say(&format!("get {} failed: {error}", machine.id));
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
        panic!("provider unreadable for {}", machine.id)
    }

    pub fn wait_gone(&self, machine: &Machine, timeout: Duration) -> bool {
        self.wait_for(timeout, Duration::from_secs(5), || {
            self.refresh(machine).is_none().then_some(())
        })
        .is_some()
    }

    /// Wait until the machine no longer holds a GPU (stopped or gone). Returns
    /// the seconds waited and the machine's last state.
    pub fn wait_not_billing(
        &self,
        machine: &Machine,
        timeout: Duration,
    ) -> Option<(f64, Option<Machine>)> {
        let began = Instant::now();
        self.wait_for(timeout, Duration::from_secs(5), || {
            let state = self.refresh(machine);
            let settled = state.as_ref().is_none_or(Machine::is_stopped);
            settled.then(|| (began.elapsed().as_secs_f64(), state))
        })
    }

    pub fn add_volume(&self, id: &str) {
        self.volumes.lock().expect("volumes").push(id.to_string());
    }

    // --- cleanup and results ---

    fn cleanup(&self) -> Vec<String> {
        if self.cleaned.swap(true, Ordering::SeqCst) {
            return Vec::new();
        }
        let mut problems = Vec::new();
        for server in self.servers.lock().expect("servers").iter() {
            server.kill9();
        }
        // Reap until the provider's own inventory shows nothing of ours.
        let empty = self
            .wait_for_reaped(Duration::from_mins(3))
            .unwrap_or_else(|left| {
                let text = left
                    .iter()
                    .map(Machine::describe)
                    .collect::<Vec<_>>()
                    .join("; ");
                problems.push(format!("MACHINES LEFT AT THE PROVIDER: {text}"));
                false
            });
        if empty {
            self.say("inventory verified: nothing of this scenario remains at the provider");
        }
        for volume in self.volumes.lock().expect("volumes").iter() {
            if let Err(error) = providers::delete_network_volume(volume) {
                problems.push(format!("network volume {volume} not deleted: {error}"));
            }
        }
        if self.providers.contains(&Provider::Vast)
            && let Err(error) = providers::vast_remove_scratch_keys(&self.keys_root)
        {
            problems.push(format!(
                "scratch ssh keys left on the vast account: {error}"
            ));
        }
        problems
    }

    /// Destroy and re-list until empty. `Err` carries what would not go.
    fn wait_for_reaped(&self, timeout: Duration) -> Result<bool, Vec<Machine>> {
        let end = Instant::now() + timeout;
        loop {
            let left = self.machines();
            if left.is_empty() {
                return Ok(true);
            }
            for machine in &left {
                self.say(&format!("reaping {}", machine.describe()));
                if let Err(error) = providers::destroy(machine) {
                    self.say(&format!("destroy failed: {error}"));
                }
            }
            if Instant::now() >= end {
                return Err(left);
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    }

    fn finish(&self, outcome: std::thread::Result<()>) {
        let mut problems = self.cleanup();

        self.meter.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.meter_thread.lock().expect("meter thread").take() {
            let _ = handle.join();
        }
        let usd = *self.meter.spent.lock().expect("meter");
        let _ = std::fs::remove_file(self.inflight_path());
        let per_machine: Vec<Value> = self
            .meter
            .per_machine
            .lock()
            .expect("meter")
            .iter()
            .map(|(id, (provider, rate, cost))| {
                json!({"id": id, "provider": provider, "rate": rate, "usd": cost})
            })
            .collect();
        append_jsonl(
            "ledger.jsonl",
            &json!({"run": run_id(), "scenario": self.name, "usd": usd,
                    "machines": per_machine, "at": now_epoch()}),
        );

        let mut failures = self.failures.lock().expect("failures").clone();
        failures.append(&mut problems);
        let mut inconclusive = None;
        let mut resume = None;
        if let Err(payload) = outcome {
            if let Some(reason) = payload.downcast_ref::<Inconclusive>() {
                inconclusive = Some(reason.0.clone());
            } else {
                let message = panic_messages()
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&std::thread::current().id()).cloned())
                    .unwrap_or_else(|| "panicked".to_string());
                failures.push(scrub(&message));
                resume = Some(payload);
            }
        }
        let result = if !failures.is_empty() {
            "FAIL"
        } else if inconclusive.is_some() {
            "INCONCLUSIVE"
        } else {
            "PASS"
        };
        let secs = self.t0.elapsed().as_secs_f64();
        let timings: Vec<Value> = self
            .timings
            .lock()
            .expect("timings")
            .iter()
            .map(|(step, secs)| json!({"step": step, "secs": secs}))
            .collect();
        append_jsonl(
            "results.jsonl",
            &json!({
                "run": run_id(), "scenario": self.name, "result": result, "secs": secs,
                "usd": usd, "timings": timings, "failures": failures,
                "inconclusive": inconclusive, "notes": *self.notes.lock().expect("notes"),
                "binary": binary_under_test().display().to_string(),
                "machines": per_machine, "at": now_epoch(),
            }),
        );
        self.say(&format!(
            "{result} in {secs:.0}s, ~${usd:.4}; ledger total ${:.3} of ${:.2}; logs {}",
            total_spend(),
            spend_limit(),
            self.log_dir.display()
        ));
        if let Some(payload) = resume {
            std::panic::resume_unwind(payload);
        }
        assert!(
            failures.is_empty(),
            "{}: {} check(s) failed:\n  - {}",
            self.name,
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        // finish() normally ran. This is the guard for when it could not.
        let problems = self.cleanup();
        for problem in problems {
            eprintln!("[{}] {problem}", self.name);
        }
        self.meter.stop.store(true, Ordering::SeqCst);
    }
}

fn accrue(meter: &Meter, machines: &[Machine], dt: f64) {
    let mut spent = meter.spent.lock().expect("meter");
    let mut per_machine = meter.per_machine.lock().expect("meter");
    for machine in machines {
        let rate = if machine.is_stopped() {
            machine.stopped_rate
        } else {
            machine.running_rate
        };
        let cost = rate * dt / 3600.0;
        *spent += cost;
        let entry = per_machine.entry(machine.id.clone()).or_insert_with(|| {
            (
                machine.provider.name().to_string(),
                machine.running_rate,
                0.0,
            )
        });
        entry.2 += cost;
    }
}

/// `remote-kernels.toml` for a scenario.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Lines at the top level.
    pub top: Vec<String>,
    /// Lines in the provider's section.
    pub section: Vec<String>,
    pub vast: bool,
}

impl Config {
    /// The cheapest secure-cloud pod with a small persistent volume.
    pub fn runpod() -> Self {
        let gpus = RUNPOD_GPUS
            .iter()
            .map(|gpu| format!("{gpu:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            top: vec!["default-runtime = \"runpod\"".to_string()],
            section: vec![
                "cloud-type = \"SECURE\"".to_string(),
                format!("gpu-type-ids = [{gpus}]"),
                "container-disk-gb = 30".to_string(),
                // RunPod's floor for a persistent volume.
                "volume-gb = 10".to_string(),
            ],
            vast: false,
        }
    }

    /// The cheapest verified vast offers.
    pub fn vast() -> Self {
        Self {
            top: vec!["default-runtime = \"vast\"".to_string()],
            section: vec![
                "gpu-name = []".to_string(),
                "max-dph = 0.25".to_string(),
                "disk-gb = 25.0".to_string(),
                "provision-timeout-mins = 12".to_string(),
                // A host with no direct port leaves the proxy as the only way
                // in; keep the spend on machines the test can actually reach.
                "[vast.query]".to_string(),
                "direct_port_count = { gte = 1 }".to_string(),
            ],
            vast: true,
        }
    }

    pub fn budget(mut self, dollars: f64) -> Self {
        self.top.push(format!("budget-cap = {dollars}"));
        self
    }

    pub fn startup(mut self, commands: &[&str]) -> Self {
        let list = commands
            .iter()
            .map(|command| format!("{command:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.top.push(format!("startup-commands = [{list}]"));
        self
    }

    /// A line in the provider's section.
    pub fn with(mut self, line: &str) -> Self {
        // Section keys must come before the [vast.query] subtable.
        let at = self
            .section
            .iter()
            .position(|existing| existing.starts_with('['))
            .unwrap_or(self.section.len());
        self.section.insert(at, line.to_string());
        self
    }

    fn render(&self, name: &str) -> String {
        let mut out = format!("name = {name:?}\n");
        for line in &self.top {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(if self.vast {
            "\n[vast]\n"
        } else {
            "\n[runpod]\n"
        });
        for line in &self.section {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

// --- reading replies without leaning on their wording ---

/// The first ULID in a reply: machine ids.
pub fn find_ulid(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|token| {
            token.len() == 26
                && token.chars().any(|c| c.is_ascii_digit())
                && ulid::Ulid::from_string(token).is_ok()
        })
        .map(String::from)
}

/// The first UUID in a reply: kernel ids.
pub fn find_uuid(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|token| token.len() == 36 && uuid::Uuid::parse_str(token).is_ok())
        .map(String::from)
}

/// The dollar amount following the first of `labels` found in a reply.
pub fn dollars_after(text: &str, labels: &[&str]) -> Option<f64> {
    let lower = text.to_lowercase();
    for label in labels {
        if let Some(at) = lower.find(&label.to_lowercase()) {
            let rest = &text[at..];
            let dollar = rest.find('$')?;
            let number: String = rest[dollar + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(value) = number.trim_end_matches('.').parse() {
                return Some(value);
            }
        }
    }
    None
}

/// What a session has been charged, according to the server.
pub fn session_cost(status: &Reply) -> Option<f64> {
    dollars_after(
        &status.text,
        &["Session cost", "Session spend", "Spent", "Budget:"],
    )
}

/// Whether a failed start means the provider had nothing to rent.
pub fn is_capacity_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "not enough free",
        "no longer any instances",
        "no instances",
        "there are no",
        "out of stock",
        "no offers",
        "no offer ",
        "not rentable",
        "does not have the resources",
        "no capacity",
        "no gpu",
        "could not find any",
        "capacity",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[test]
fn reply_parsers() {
    let text = "Machine 01M6NB98YMRQYQ9X764PCY1TMX (provider id: 6ul2c85hitphqv)";
    assert_eq!(
        find_ulid(text).as_deref(),
        Some("01M6NB98YMRQYQ9X764PCY1TMX")
    );
    assert_eq!(find_ulid("NVIDIA-RTX-ABCDEFGHIJKLMNOPQRSTUVWXYZ"), None);
    let kernel = "Kernel created: 12345678-1234-1234-1234-123456789abc (x.ipynb)";
    assert_eq!(
        find_uuid(kernel).as_deref(),
        Some("12345678-1234-1234-1234-123456789abc")
    );
    let footer = "\n\nSession cost: $0.03\nBudget: $0.03 / $2.00 ($1.97 remaining)";
    assert_eq!(dollars_after(footer, &["Session cost"]), Some(0.03));
    assert_eq!(dollars_after(footer, &["nope"]), None);
}
