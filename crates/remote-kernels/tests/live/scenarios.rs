//! The RunPod scenarios of the lifecycle plan. Each reads as the story it
//! guards: tool calls for what an agent does, provider API for what really
//! happened. Wording in replies is only matched where the scenario is about
//! what the agent gets told, and then loosely.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{
    COLD_IMAGE, Config, Limits, MIN, RUNPOD_ONLY, Scenario, find_ulid, find_uuid,
    is_capacity_error, run_scenario, session_cost,
};
use crate::mcp::{Launch, Pending, Reply, Server};
use crate::providers::{self, Machine, Provider};
use crate::proxy::{ConnectProxy, Mode};

/// A scenario's server-side bound on any quick tool call: long enough that only
/// a real hang trips it.
const QUICK: Duration = Duration::from_secs(120);
/// Long enough for setup to run its course in the foreground.
const START_REPLY: Duration = Duration::from_secs(25 * 60);
/// The stale window the machine side uses by default.
const STALE: Duration = Duration::from_secs(300);

fn limits(minutes: u64) -> Limits {
    Limits {
        minutes,
        providers: RUNPOD_ONLY,
    }
}

pub struct Up {
    pub machine_id: String,
    pub kernel: String,
    pub provider: Machine,
}

/// The machine id from the provider-side name, which every binary builds as
/// `<name>-<machine id>`.
pub fn machine_id_of(machine: &Machine) -> String {
    machine
        .name
        .rsplit('-')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Give up cleanly when the provider has nothing to rent.
pub fn bail_if_no_capacity(sc: &Scenario, reply: &Reply) {
    if reply.is_error && is_capacity_error(&reply.text) {
        sc.inconclusive(&format!("no capacity: {}", first_line(&reply.text)));
    }
}

pub fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// Create a kernel as soon as the server accepts one.
pub fn wait_kernel(
    sc: &Scenario,
    srv: &Server,
    instance: &str,
    timeout: Duration,
) -> Option<String> {
    let began = Instant::now();
    let found = sc.wait_for(timeout, Duration::from_secs(10), || {
        let reply = srv.call(
            "create_kernel",
            &json!({"name": "live", "instance": instance}),
            QUICK,
        );
        if reply.ok() {
            return find_uuid(&reply.text);
        }
        let status = srv.call("status", &json!({}), QUICK);
        assert!(
            !status.mentions("failed to start"),
            "the machine failed to start: {}",
            status.text
        );
        None
    });
    sc.timing("until a kernel was accepted", began.elapsed().as_secs_f64());
    found
}

/// `start()` in the background, returning once the provider lists the machine.
/// A start refused for capacity tries again a few times, then gives up cleanly.
pub fn start_until_listed(
    sc: &Scenario,
    srv: &Server,
    variant: &str,
    args: &Value,
) -> (Pending, Machine) {
    for attempt in 1..=5 {
        let mut start = srv.call_async("start", args);
        let listed = sc.wait_for(5 * MIN, Duration::from_secs(2), || {
            if let Some(machine) = sc.machines_named(variant).into_iter().next() {
                return Some(Some(machine));
            }
            start.replied().then_some(None)
        });
        match listed {
            Some(Some(machine)) => return (start, machine),
            Some(None) => {
                let reply = start.wait(Duration::ZERO).expect("it replied");
                assert!(
                    reply.is_error && is_capacity_error(&reply.text),
                    "start() replied without a machine: {}",
                    reply.text
                );
                if attempt == 5 {
                    bail_if_no_capacity(sc, &reply);
                }
                sc.say(&format!(
                    "no capacity on start attempt {attempt}; retrying in 60 s"
                ));
                sc.sleep(MIN);
            }
            None => panic!("start() neither replied nor made a machine in 5 min"),
        }
    }
    unreachable!("the last attempt bails out")
}

/// `start()` in the foreground, then a kernel: the normal opening of a session.
pub fn start_and_kernel(sc: &Scenario, srv: &Arc<Server>, variant: &str, args: &Value) -> Up {
    let began = Instant::now();
    // Capacity comes and goes by the minute: a few tries before giving up.
    let mut reply = srv.call("start", args, START_REPLY);
    for attempt in 1..=4 {
        // An old binary's 15 s budget gate can kill a healthy start (#99).
        let gate_missed = reply.text.contains("cannot enforce it after a disconnect");
        if !(reply.is_error && (is_capacity_error(&reply.text) || gate_missed)) {
            break;
        }
        sc.say(&format!(
            "no usable machine on start attempt {attempt}; retrying in 60 s"
        ));
        sc.sleep(MIN);
        reply = srv.call("start", args, START_REPLY);
    }
    sc.timing("start() reply", reply.secs);
    bail_if_no_capacity(sc, &reply);
    assert!(reply.ok(), "start failed: {}", reply.text);
    let provider = sc
        .wait_machine(variant, MIN)
        .expect("the provider lists no machine after a successful start");
    let machine_id = find_ulid(&reply.text).unwrap_or_else(|| machine_id_of(&provider));
    let kernel = wait_kernel(sc, srv, &machine_id, 20 * MIN).expect("no kernel within 20 min");
    sc.timing("start() to first kernel", began.elapsed().as_secs_f64());
    sc.say(&format!("up: {} machine {machine_id}", provider.describe()));
    Up {
        machine_id,
        kernel,
        provider,
    }
}

pub fn py(srv: &Server, kernel: &str, code: &str, secs: u64) -> Reply {
    srv.call(
        "execute",
        &json!({"kernel_id": kernel, "code": code, "timeout": secs}),
        Duration::from_secs(secs + 90),
    )
}

/// Run code where a network blip must not read as a verdict: an error reply
/// gets more tries over 2 min, as an agent told to "retry shortly" would.
pub fn py_settling(sc: &Scenario, srv: &Server, kernel: &str, code: &str) -> Reply {
    let mut last = py(srv, kernel, code, 60);
    let began = Instant::now();
    while last.is_error && began.elapsed() < 2 * MIN {
        sc.say(&format!("retrying after: {}", first_line(&last.text)));
        sc.sleep(Duration::from_secs(10));
        last = py(srv, kernel, code, 60);
    }
    last
}

/// A kernel-side script that ticks for a while and leaves a file behind.
pub fn ticking_cell(ticks: u32, every_secs: u32, done_file: &str, marker: &str) -> String {
    format!(
        "import time\nfor i in range({ticks}):\n    print('tick', i, flush=True)\n    time.sleep({every_secs})\nopen({done_file:?}, 'w').write(str(time.time()))\nprint({marker:?}, flush=True)\n"
    )
}

pub fn background(srv: &Server, kernel: &str, code: &str) -> Reply {
    srv.call(
        "execute",
        &json!({"kernel_id": kernel, "code": code, "background": true}),
        QUICK,
    )
}

pub fn terminate_and_verify(sc: &Scenario, srv: &Server, up: &Machine, args: &Value) {
    let reply = srv.call("terminate", args, Duration::from_secs(300));
    sc.timing("terminate()", reply.secs);
    sc.check(
        reply.ok(),
        &format!("terminate() succeeds: {}", first_line(&reply.text)),
    );
    sc.check(
        sc.wait_gone(up, 2 * MIN),
        "the provider reports the machine gone within 2 min of terminate()",
    );
}

/// Every notebook under the project, concatenated.
fn notebooks_text(project: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![project.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "ipynb") {
                text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    text
}

fn cold_image(which: usize) -> String {
    let fallback = match which {
        0 => COLD_IMAGE,
        1 => "runpod/pytorch:1.6.0-rc.146-cu1290-torch2121-ubuntu2404",
        _ => "runpod/pytorch:1.6.0-rc.146-cu1290-torch280-ubuntu2404",
    };
    std::env::var("RK_LIVE_COLD_IMAGE").unwrap_or_else(|_| fallback.to_string())
}

/// R1: the session the product exists for.
pub fn r1() {
    run_scenario("R1", limits(30), |sc| {
        let project = sc.project("", &Config::runpod().budget(1.0));
        std::fs::write(
            project.join("analysis.py"),
            "def answer():\n    return 42\n",
        )
        .unwrap();
        let srv = sc.server(&project, "a", &sc.session("a"));
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "r1"}));

        let synced = srv.call("sync", &json!({}), Duration::from_secs(300));
        sc.timing("sync()", synced.secs);
        sc.check(
            synced.ok(),
            &format!("sync() succeeds: {}", first_line(&synced.text)),
        );

        let gpu = py(
            &srv,
            &up.kernel,
            "import subprocess; print(subprocess.run(['nvidia-smi', '-L'], capture_output=True, text=True).stdout)",
            60,
        );
        sc.check(
            gpu.mentions("GPU 0"),
            &format!("nvidia-smi sees a GPU: {}", gpu.text),
        );

        let imported = py(
            &srv,
            &up.kernel,
            "import analysis; print('answer', analysis.answer())",
            60,
        );
        sc.check(
            imported.mentions("answer 42"),
            &format!("synced code runs: {}", imported.text),
        );

        let plot = py(
            &srv,
            &up.kernel,
            "import subprocess, sys, os\nsubprocess.run([sys.executable, '-m', 'pip', 'install', '-q', 'matplotlib'], capture_output=True)\nimport matplotlib\nmatplotlib.use('Agg')\nimport matplotlib.pyplot as plt\nos.makedirs('results', exist_ok=True)\nplt.plot([1, 2, 3])\nplt.savefig('results/plot.png')\nprint('plotted')\n",
            240,
        );
        sc.check(
            plot.mentions("plotted"),
            &format!("the plot is made: {}", plot.text),
        );

        let finish = srv.call(
            "finish",
            &json!({"download": ["results"], "then": "terminate"}),
            QUICK,
        );
        sc.timing("finish() reply", finish.secs);
        sc.check(
            finish.ok(),
            &format!("finish() is accepted: {}", finish.text),
        );
        let began = Instant::now();
        sc.check(
            sc.wait_gone(&up.provider, 5 * MIN),
            "finish(then=terminate) leaves the provider with no machine within 5 min",
        );
        sc.timing("finish() to machine gone", began.elapsed().as_secs_f64());

        let png = std::fs::read(project.join("results/plot.png")).unwrap_or_default();
        sc.check(
            png.starts_with(b"\x89PNG"),
            "the plot was downloaded before the machine went",
        );
        sc.check(
            notebooks_text(&project).contains("answer 42"),
            "the local notebook holds what ran",
        );
        let status = srv.call("status", &json!({}), QUICK);
        sc.check(
            !status.text.contains(&up.machine_id),
            &format!("status() no longer lists the machine: {}", status.text),
        );
    });
}

/// L1 (#98, #99): a budgeted start of a big image with slow startup commands.
pub fn l1() {
    run_scenario("L1", limits(35), |sc| {
        let config = Config::runpod()
            .budget(1.0)
            .startup(&["sleep 60; touch /tmp/rk-marker"])
            .with("container-disk-gb = 60");
        let mut config = config;
        config
            .section
            .retain(|line| line != "container-disk-gb = 30");
        let project = sc.project("", &config);
        let srv = sc.server(&project, "a", &sc.session("a"));
        let image = cold_image(0);
        sc.note(&format!("image {image}"));

        let began = Instant::now();
        let (mut start, pod) =
            start_until_listed(sc, &srv, "", &json!({"label": "l1", "image": image}));
        sc.timing("start() to pod listed", began.elapsed().as_secs_f64());
        let reply = sc.wait_for(START_REPLY, Duration::from_secs(5), || {
            let state = sc.refresh(&pod);
            assert!(state.is_some(), "the pod was destroyed during its pull");
            start.wait(Duration::ZERO)
        });
        let reply = reply.expect("start() never replied");
        sc.timing("start() reply", reply.secs);
        bail_if_no_capacity(sc, &reply);
        sc.check(
            reply.ok(),
            &format!("start() does not fail a slow pull: {}", reply.text),
        );

        let machine_id = machine_id_of(&pod);
        let kernel = wait_kernel(sc, &srv, &machine_id, 25 * MIN).expect("no kernel in 25 min");
        sc.timing("start() to first kernel", began.elapsed().as_secs_f64());
        let marker = py(
            &srv,
            &kernel,
            "import os; print('marker', os.path.exists('/tmp/rk-marker'))",
            60,
        );
        sc.check(
            marker.mentions("marker True"),
            &format!(
                "startup commands had finished when the machine was reported usable: {}",
                marker.text
            ),
        );
        let state = sc.refresh(&pod);
        sc.check(
            state.as_ref().is_some_and(|m| !m.is_stopped()),
            "the provider still shows the same pod running",
        );
        let status = srv.call("status", &json!({}), QUICK);
        sc.check(
            !status.mentions("unenforced") && !status.mentions("unsupervis"),
            &format!("the budget is enforced on this machine: {}", status.text),
        );
        sc.check(sc.machines().len() == 1, "exactly one pod exists");
        terminate_and_verify(sc, &srv, &pod, &json!({"instance": machine_id}));
    });
}

/// L2 (#88): stop() and terminate() while the image pulls must act at once.
pub fn l2() {
    run_scenario("L2", limits(25), |sc| {
        for (which, (variant, tool)) in [("stop", "stop"), ("term", "terminate")].iter().enumerate()
        {
            let mut config = Config::runpod().with("container-disk-gb = 60");
            config
                .section
                .retain(|line| line != "container-disk-gb = 30");
            let project = sc.project(variant, &config);
            let srv = sc.server(&project, variant, &sc.session(variant));
            let image = cold_image(which);
            let (mut start, pod) = start_until_listed(
                sc,
                &srv,
                variant,
                &json!({"label": variant, "image": image}),
            );
            sc.sleep(Duration::from_secs(25));
            let state = sc.refresh(&pod).expect("the pod vanished before the call");
            if state.has_ssh() {
                sc.note(&format!("{variant}: SSH was already up, so the pull had finished; the image is cached on this host"));
            }
            let reply = srv.call(tool, &json!({}), Duration::from_secs(420));
            sc.timing(&format!("{tool}() during pull"), reply.secs);
            sc.check(
                reply.secs <= 60.0,
                &format!(
                    "{tool}() during a pull answers within 60 s (took {:.0}s)",
                    reply.secs
                ),
            );
            sc.check(
                reply.ok(),
                &format!("{tool}() succeeds: {}", first_line(&reply.text)),
            );
            let began = Instant::now();
            if *tool == "stop" {
                let stopped = sc.wait_not_billing(&pod, 3 * MIN);
                sc.check(
                    stopped.is_some(),
                    "stop() during a pull ends GPU billing within 3 min",
                );
            } else {
                sc.check(
                    sc.wait_gone(&pod, 3 * MIN),
                    "terminate() during a pull removes the pod within 3 min",
                );
            }
            sc.timing(
                &format!("{tool}() to provider confirms"),
                began.elapsed().as_secs_f64(),
            );
            match start.wait(5 * MIN) {
                Some(reply) => sc.timing("start() returned after", reply.secs),
                None => sc.check(false, "start() replies within 5 min of being ended"),
            }
            sc.check(
                sc.machines_named(variant).len() <= 1,
                "no second pod appeared",
            );
            let leftovers = sc.refresh(&pod);
            if leftovers.is_some() {
                terminate_and_verify(sc, &srv, &pod, &json!({"instance": machine_id_of(&pod)}));
            }
        }
    });
}

/// L3 (#100): a stale record must not slow the handshake or wake the machine.
pub fn l3() {
    run_scenario("L3", limits(20), |sc| {
        let project = sc.project("", &Config::runpod());
        let session = sc.session("a");
        let first = sc.server(&project, "a", &session);
        let up = start_and_kernel(sc, &first, "", &json!({"label": "l3"}));
        first.kill9();
        providers::runpod_stop(&up.provider.id).expect("provider stop");
        let stopped = sc
            .wait_for(3 * MIN, Duration::from_secs(5), || {
                sc.refresh(&up.provider).filter(Machine::is_stopped)
            })
            .expect("the pod never stopped at the provider");
        sc.say(&format!(
            "stopped behind the server's back: {}",
            stopped.describe()
        ));

        for (tag, who) in [
            ("same-session", session.clone()),
            ("new-session", sc.session("b")),
        ] {
            let srv = sc.server(&project, tag, &who);
            sc.timing(&format!("{tag} handshake"), srv.handshake_secs);
            sc.check(
                srv.handshake_secs < 2.0,
                &format!(
                    "{tag}: handshake answers in under 2 s (took {:.2}s)",
                    srv.handshake_secs
                ),
            );
            let status = srv.call("status", &json!({}), QUICK);
            sc.timing(&format!("{tag} status"), status.secs);
            sc.check(
                status.secs < 5.0,
                &format!(
                    "{tag}: status answers in under 5 s (took {:.1}s)",
                    status.secs
                ),
            );
            sc.check(
                status.text.contains(&up.machine_id),
                &format!("{tag}: status lists the stopped machine: {}", status.text),
            );
            // Time for anything that wants to resume the machine to try it.
            sc.sleep(Duration::from_secs(45));
            let again = srv.call("status", &json!({}), QUICK);
            sc.timing(&format!("{tag} second status"), again.secs);
            let state = sc.refresh(&up.provider);
            sc.check(
                state.as_ref().is_some_and(Machine::is_stopped),
                &format!(
                    "{tag}: the pod stays stopped; reading state must not resume it (now {:?})",
                    state.map(|m| m.status)
                ),
            );
            srv.kill9();
        }

        let srv = sc.server(&project, "cleanup", &session);
        terminate_and_verify(sc, &srv, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// L4 (#97): what a machine killed mid-cleanup leaves on a network volume must
/// not trouble the next machine on that volume.
pub fn l4() {
    run_scenario("L4", limits(30), |sc| {
        let mut chosen = None;
        for datacenter in ["EU-RO-1", "US-IL-1", "EU-CZ-1"] {
            let name = format!("{}-vol-{datacenter}", sc.prefix);
            let volume = match providers::create_network_volume(&name, datacenter, 10) {
                Ok(id) => id,
                Err(error) => {
                    sc.say(&format!("volume in {datacenter}: {error}"));
                    continue;
                }
            };
            sc.add_volume(&volume);
            sc.say(&format!("network volume {volume} in {datacenter}"));

            let config =
                network_volume_config(&volume).with("pre-terminate-command = \"sleep 600\"");
            let project = sc.project(&format!("a{datacenter}"), &config);
            let srv = sc.server(&project, "a", &sc.session("a"));
            let reply = srv.call("start", &json!({"label": "l4-a"}), START_REPLY);
            if reply.is_error && is_capacity_error(&reply.text) {
                sc.say(&format!(
                    "no capacity in {datacenter}: {}",
                    first_line(&reply.text)
                ));
                srv.kill9();
                continue;
            }
            sc.timing("pod A start() reply", reply.secs);
            assert!(reply.ok(), "pod A failed to start: {}", reply.text);
            chosen = Some((volume, datacenter, srv));
            break;
        }
        let Some((volume, datacenter, srv)) = chosen else {
            sc.inconclusive("no datacenter tried could host a pod on a new network volume");
        };
        let pod_a = sc.wait_machine("a", MIN).expect("pod A is not listed");
        let machine_a = machine_id_of(&pod_a);
        let kernel = wait_kernel(sc, &srv, &machine_a, 15 * MIN).expect("no kernel on pod A");
        let ran = py(
            &srv,
            &kernel,
            "open('/workspace/from-a', 'w').write('a'); print('wrote')",
            60,
        );
        sc.check(ran.mentions("wrote"), "pod A can write to the volume");

        // Cleanup is running (its save command sleeps) when the machine dies.
        let mut cleanup = srv.call_async("terminate", &json!({}));
        sc.sleep(Duration::from_secs(40));
        sc.check(
            !cleanup.replied(),
            "the slow save is still running when the pod is killed",
        );
        providers::destroy(&pod_a).expect("delete pod A");
        srv.kill9();
        sc.check(sc.wait_gone(&pod_a, 2 * MIN), "pod A is gone");

        let config_b = network_volume_config(&volume);
        let project_b = sc.project(&format!("b{datacenter}"), &config_b);
        let srv_b = sc.server(&project_b, "b", &sc.session("b"));
        let reply = srv_b.call("start", &json!({"label": "l4-b"}), START_REPLY);
        sc.timing("pod B start() reply", reply.secs);
        bail_if_no_capacity(sc, &reply);
        sc.check(
            reply.ok(),
            &format!("pod B starts on the used volume: {}", reply.text),
        );
        let pod_b = sc.wait_machine("b", MIN).expect("pod B is not listed");
        let machine_b = machine_id_of(&pod_b);
        let kernel_b = wait_kernel(sc, &srv_b, &machine_b, 15 * MIN).expect("no kernel on pod B");
        let read = py(
            &srv_b,
            &kernel_b,
            "print('read', open('/workspace/from-a').read())",
            60,
        );
        sc.check(
            read.mentions("read a"),
            &format!("pod B sees the volume's data: {}", read.text),
        );

        let stop = srv_b.call("stop", &json!({}), Duration::from_secs(300));
        sc.timing("pod B stop()", stop.secs);
        sc.check(
            stop.ok(),
            &format!("pod B stops normally: {}", first_line(&stop.text)),
        );
        sc.check(
            sc.wait_not_billing(&pod_b, 3 * MIN).is_some(),
            "pod B's GPU is released within 3 min of stop()",
        );
        terminate_and_verify(sc, &srv_b, &pod_b, &json!({"instance": machine_b}));
    });
}

fn network_volume_config(volume: &str) -> Config {
    let mut config = Config::runpod();
    config.section.retain(|line| !line.starts_with("volume-gb"));
    config.with(&format!("network-volume-id = {volume:?}"))
}

/// L5: close the laptop during a long cell. The core promise: the cell
/// finishes, the save runs, the action follows, and attach backfills the
/// notebook. Both save commands use paths relative to the workdir
/// (/workspace), which is where they must run: the machine's own save here,
/// and the explicit terminate()'s at the end.
pub fn l5() {
    run_scenario("L5", limits(35), |sc| {
        let config = Config::runpod()
            .with("cleanup = \"stop\"")
            .with(
                "pre-stop-command = \"test -e cell-done && echo cell-was-done > save-ran || echo cell-was-not-done > save-ran\"",
            )
            .with("pre-terminate-command = \"test -e save-ran\"");
        let project = sc.project("", &config);
        let session = sc.session("a");
        let srv = sc.server(&project, "a", &session);
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "l5"}));

        let cell = ticking_cell(48, 10, "/workspace/cell-done", "L5-CELL-FINISHED");
        let began = Instant::now();
        let reply = background(&srv, &up.kernel, &cell);
        sc.check(
            reply.ok(),
            &format!("the 8 min cell starts: {}", reply.text),
        );
        sc.sleep(Duration::from_secs(30));
        srv.sigstop();
        sc.say("laptop closed (SIGSTOP)");

        // The machine must wait out the cell before doing anything.
        let cell_end = Duration::from_secs(480);
        while began.elapsed() < cell_end {
            sc.check_silently_running(&up.provider, "the pod keeps running while the cell does");
            sc.sleep(Duration::from_secs(20));
        }
        let settled = sc.wait_not_billing(&up.provider, 6 * MIN);
        sc.check(
            settled.is_some(),
            "the machine stops itself within 6 min of the cell finishing",
        );
        sc.timing(
            "cell end to machine stopped",
            began.elapsed().as_secs_f64() - 480.0,
        );
        srv.kill9();
        let Some((_, Some(_))) = settled else {
            sc.say("machine not stopped (or deleted); nothing further to verify");
            return;
        };

        // Open the laptop elsewhere: attach resumes and backfills.
        let srv2 = sc.server(&project, "b", &session);
        let attach = attach_with_capacity_retry(sc, &srv2, &up.machine_id);
        sc.check(
            attach.ok(),
            &format!("attach resumes the machine: {}", attach.text),
        );
        let notebook_has_result = sc
            .wait_for(3 * MIN, Duration::from_secs(10), || {
                notebooks_text(&project)
                    .contains("L5-CELL-FINISHED")
                    .then_some(())
            })
            .is_some();
        sc.check(
            notebook_has_result,
            "attach backfills the notebook with the output produced while away",
        );
        let kernel =
            wait_kernel(sc, &srv2, &up.machine_id, 10 * MIN).expect("no kernel after attach");
        let save = py(
            &srv2,
            &kernel,
            "import os\nprint('done', os.path.exists('/workspace/cell-done'))\nprint('save', open('/workspace/save-ran').read() if os.path.exists('/workspace/save-ran') else 'missing')\n",
            60,
        );
        sc.check(
            save.mentions("done True"),
            &format!("the cell finished on the machine: {}", save.text),
        );
        sc.check(
            save.mentions("cell-was-done"),
            &format!(
                "the save command ran in the workdir, after the cell: {}",
                save.text
            ),
        );
        // A pre-terminate command run anywhere else fails, and the terminate
        // would become a stop.
        terminate_and_verify(sc, &srv2, &up.provider, &json!({"instance": up.machine_id}));
    });
}

pub fn attach_with_capacity_retry(sc: &Scenario, srv: &Server, machine_id: &str) -> Reply {
    let began = Instant::now();
    let mut attempts = 0;
    loop {
        attempts += 1;
        let reply = srv.call(
            "attach",
            &json!({"machine_id": machine_id}),
            Duration::from_secs(900),
        );
        if !(reply.is_error && is_capacity_error(&reply.text)) {
            sc.timing("attach()", began.elapsed().as_secs_f64());
            return reply;
        }
        if attempts >= 5 {
            sc.inconclusive(&format!(
                "the host never had capacity to resume after {attempts} tries: {}",
                first_line(&reply.text)
            ));
        }
        sc.say(&format!(
            "resume attempt {attempts} found no capacity; retrying in 60 s"
        ));
        sc.sleep(MIN);
    }
}

/// L6: close the laptop on an idle machine; it cleans itself up. Opening the
/// laptop inside the window cancels that.
pub fn l6() {
    run_scenario("L6", limits(35), |sc| {
        // Variant: the window passes.
        let project = sc.project("gone", &Config::runpod().with("cleanup = \"terminate\""));
        let srv = sc.server(&project, "gone", &sc.session("gone"));
        let up = start_and_kernel(sc, &srv, "gone", &json!({"label": "l6"}));
        srv.sigstop();
        let began = Instant::now();
        sc.say("laptop closed on an idle machine");
        // Nothing should happen for a while first.
        sc.sleep(STALE - Duration::from_secs(60));
        sc.check_silently_running(&up.provider, "the idle machine survives the first 4 min");
        let settled = sc.wait_not_billing(&up.provider, 10 * MIN);
        sc.timing(
            "laptop close to machine cleaned up",
            began.elapsed().as_secs_f64(),
        );
        sc.check(
            settled.as_ref().is_some_and(|(_, state)| state.is_none()),
            &format!(
                "the idle machine terminates itself within 14 min (end state: {:?})",
                settled
                    .as_ref()
                    .map(|(_, s)| s.as_ref().map(|m| m.status.clone()))
            ),
        );
        srv.kill9();

        // Variant: the laptop opens inside the window.
        let project = sc.project("back", &Config::runpod().with("cleanup = \"terminate\""));
        let srv = sc.server(&project, "back", &sc.session("back"));
        let up = start_and_kernel(sc, &srv, "back", &json!({"label": "l6b"}));
        srv.sigstop();
        let began = Instant::now();
        sc.sleep(Duration::from_secs(200));
        srv.sigcont();
        sc.say("laptop opened after 200 s");
        // Past where the window would have ended, plus margin.
        while began.elapsed() < STALE + Duration::from_secs(180) {
            sc.check_silently_running(&up.provider, "the machine survives when the laptop returns");
            sc.sleep(Duration::from_secs(15));
        }
        let ran = py_settling(sc, &srv, &up.kernel, "print('still', 40 + 2)");
        sc.check(
            ran.mentions("still 42"),
            &format!("the kernel still works: {}", ran.text),
        );
        let status = srv.call("status", &json!({}), QUICK);
        sc.check(
            !status.mentions("fenced") && !status.mentions("detached"),
            &format!("the session still drives the machine: {}", status.text),
        );
        terminate_and_verify(sc, &srv, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// L7 (f6587371): three minutes without ssh must cost nothing.
pub fn l7() {
    run_scenario("L7", limits(25), |sc| {
        let shim_dir = sc.dir.join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let sentinel = sc.dir.join("ssh-is-down");
        let script = format!(
            "#!/bin/sh\nif [ -e {} ]; then echo 'ssh: connect to host: Network is unreachable' >&2; exit 255; fi\nexec /usr/bin/ssh \"$@\"\n",
            sentinel.display()
        );
        let shim = shim_dir.join("ssh");
        std::fs::write(&shim, script).unwrap();
        std::process::Command::new("chmod")
            .args(["755"])
            .arg(&shim)
            .status()
            .unwrap();

        let project = sc.project("", &Config::runpod());
        let launch = Launch {
            path_prefix: Some(shim_dir),
            ..Launch::default()
        };
        let srv = sc.server_with(&project, "a", &sc.session("a"), &launch);
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "l7"}));

        let cell = ticking_cell(30, 10, "/workspace/cell-done", "L7-CELL-FINISHED");
        let reply = background(&srv, &up.kernel, &cell);
        sc.check(reply.ok(), &format!("the cell starts: {}", reply.text));
        sc.sleep(Duration::from_secs(20));

        std::fs::write(&sentinel, "down").unwrap();
        srv.kill_ssh_children();
        sc.say("ssh is down, established connections killed");
        let down_for = Duration::from_secs(180);
        let began = Instant::now();
        let mut statuses = Vec::new();
        while began.elapsed() < down_for {
            sc.sleep(Duration::from_secs(30));
            srv.kill_ssh_children();
            let status = srv.call("status", &json!({}), QUICK);
            sc.check_silently_running(&up.provider, "the pod runs through the outage");
            statuses.push((status.secs, status.text));
        }
        std::fs::remove_file(&sentinel).unwrap();
        sc.say("ssh is back");
        let slowest = statuses.iter().map(|(secs, _)| *secs).fold(0.0, f64::max);
        sc.timing("slowest status() during the outage", slowest);
        sc.check(
            !statuses.iter().any(|(_, text)| {
                let lower = text.to_lowercase();
                lower.contains("fenced") || lower.contains("detached")
            }),
            "the outage does not detach the session",
        );

        let result = srv.call(
            "wait",
            &json!({"kernel_id": up.kernel, "timeout": 420}),
            Duration::from_secs(600),
        );
        sc.timing("wait() after the outage", result.secs);
        // Do what the replies say, as the agent would: a wait held when the
        // socket died says to collect with get_output; one that lands before
        // the reconnect says to retry shortly.
        let mut result = result;
        for _ in 0..30 {
            if result.mentions("L7-CELL-FINISHED") {
                break;
            }
            if result.mentions("retry shortly") {
                sc.sleep(Duration::from_secs(15));
            }
            result = srv.call(
                "get_output",
                &json!({"kernel_id": up.kernel, "cell_number": 1, "timeout": 420}),
                Duration::from_secs(600),
            );
        }
        sc.check(
            result.mentions("L7-CELL-FINISHED"),
            &format!("the result is collected after the outage: {}", result.text),
        );
        let ran = py_settling(sc, &srv, &up.kernel, "print('after', 40 + 2)");
        sc.check(
            ran.mentions("after 42"),
            &format!("the kernel works after the outage: {}", ran.text),
        );
        terminate_and_verify(sc, &srv, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// L8 (1b07965f, 8eb4b48e): a crash at any point of provisioning, or a lost
/// create response, ends with exactly one machine, known to the next server.
pub fn l8() {
    run_scenario("L8", limits(50), |sc| {
        // The server dies just after the pod appears, then with setup under way.
        l8_kill_after_create(sc);
        l8_kill_during_setup(sc);
    });
}

/// L8, continued: the same promise where the create's answer goes missing.
pub fn l8_create() {
    run_scenario("L8-create", limits(30), |sc| {
        // The server dies while the create's answer is in flight.
        l8_kill_during_create(sc);
        // The create lands, the answer never arrives, the server lives.
        l8_lost_create_response(sc);
    });
}

fn l8_recover(sc: &Scenario, variant: &str, project: &Path, session: &str, pod: &Machine) {
    let srv = sc.server(project, &format!("{variant}-next"), session);
    sc.timing(
        &format!("{variant}: handshake with a fresh record"),
        srv.handshake_secs,
    );
    let machine_id = machine_id_of(pod);
    let status = srv.call("status", &json!({}), QUICK);
    sc.check(
        status.text.contains(&machine_id),
        &format!(
            "{variant}: the next server knows the machine: {}",
            status.text
        ),
    );
    let mut kernel = wait_kernel(sc, &srv, &machine_id, 3 * MIN);
    if kernel.is_none() {
        // What status() tells an agent to do when nothing happens by itself.
        sc.note(&format!(
            "{variant}: no automatic reattach within 3 min, so attach() as status() advises"
        ));
        let attach = srv.call(
            "attach",
            &json!({"machine_id": machine_id}),
            Duration::from_secs(600),
        );
        sc.timing(&format!("{variant}: attach()"), attach.secs);
        sc.check(
            attach.ok(),
            &format!("{variant}: attach() succeeds: {}", attach.text),
        );
        kernel = wait_kernel(sc, &srv, &machine_id, 8 * MIN);
    }
    sc.check(
        kernel.is_some(),
        &format!("{variant}: the next server reaches a usable machine within 11 min"),
    );
    sc.check(
        sc.machines_named(variant).len() == 1,
        &format!("{variant}: exactly one pod"),
    );
    terminate_and_verify(sc, &srv, pod, &json!({"instance": machine_id}));
}

fn l8_kill_after_create(sc: &Scenario) {
    let variant = "created";
    let project = sc.project(variant, &Config::runpod());
    let session = sc.session(variant);
    let srv = sc.server(&project, variant, &session);
    let (_start, pod) = start_until_listed(sc, &srv, variant, &json!({"label": variant}));
    // The record lands right after the create returns; give it a moment.
    sc.sleep(Duration::from_secs(2));
    srv.kill9();
    sc.say(&format!(
        "{variant}: killed just after create: {}",
        pod.describe()
    ));
    l8_recover(sc, variant, &project, &session, &pod);
}

fn l8_kill_during_setup(sc: &Scenario) {
    let variant = "setup";
    let project = sc.project(variant, &Config::runpod().startup(&["sleep 240"]));
    let session = sc.session(variant);
    let srv = sc.server(&project, variant, &session);
    let (_start, pod) = start_until_listed(sc, &srv, variant, &json!({"label": variant}));
    let reached_ssh = sc.wait_for(10 * MIN, Duration::from_secs(5), || {
        sc.refresh(&pod).filter(Machine::has_ssh)
    });
    sc.check(
        reached_ssh.is_some(),
        "setup: the pod reaches SSH within 10 min",
    );
    // Long enough to be into the startup commands.
    sc.sleep(Duration::from_secs(40));
    srv.kill9();
    sc.say("setup: killed during startup commands");
    l8_recover(sc, variant, &project, &session, &pod);
}

/// A start that got no machine: wait and let the caller try again, or give up.
fn bail_or_retry_capacity(sc: &Scenario, reply: &Reply, attempts: &mut u32) {
    assert!(
        is_capacity_error(&reply.text),
        "start() replied without the create landing: {}",
        reply.text
    );
    *attempts += 1;
    if *attempts > 6 {
        bail_if_no_capacity(sc, reply);
    }
    sc.say("no capacity; retrying in 60 s");
    sc.sleep(MIN);
}

/// For the proxy: does this variant's machine exist at the provider yet?
fn machine_exists(sc: &Scenario, variant: &str) -> impl Fn() -> bool + Send + Sync + 'static {
    let prefix = format!("{}{variant}", sc.prefix);
    move || providers::list(Provider::Runpod, &prefix).is_ok_and(|found| !found.is_empty())
}

fn l8_kill_during_create(sc: &Scenario) {
    let variant = "inflight";
    let project = sc.project(variant, &Config::runpod());
    let session = sc.session(variant);
    let proxy = ConnectProxy::start(Mode::Hold, machine_exists(sc, variant)).expect("proxy");
    let launch = Launch {
        env: vec![("HTTPS_PROXY".to_string(), proxy.url.clone())],
        ..Launch::default()
    };
    let srv = sc.server_with(&project, variant, &session, &launch);
    let mut start = srv.call_async("start", &json!({"label": variant}));
    let mut attempts = 1;
    let reached = sc.wait_for(8 * MIN, Duration::from_millis(500), || {
        if proxy.triggered() {
            return Some(());
        }
        if let Some(reply) = start.wait(Duration::ZERO) {
            bail_or_retry_capacity(sc, &reply, &mut attempts);
            start = srv.call_async("start", &json!({"label": variant}));
        }
        None
    });
    assert!(
        reached.is_some(),
        "the proxy never saw a create land in 8 min"
    );
    // The create is at the provider and its answer is being withheld.
    let Some(pod) = sc.wait_machine(variant, MIN) else {
        sc.inconclusive("the held create left no pod: the first GPU probably had no capacity");
    };
    srv.kill9();
    sc.say(&format!(
        "inflight: killed with the create's answer withheld: {}",
        pod.describe()
    ));

    let next = sc.server(&project, "inflight-next", &session);
    let machine_id = machine_id_of(&pod);
    let known = sc.wait_for(2 * MIN, Duration::from_secs(10), || {
        let status = next.call("status", &json!({}), QUICK);
        status.text.contains(&machine_id).then_some(status.text)
    });
    sc.check(
        known.is_some(),
        "inflight: a crash during the create leaves a local trace; the next server's status() names the machine",
    );
    if known.is_some() {
        let reply = next.call(
            "terminate",
            &json!({"instance": machine_id}),
            Duration::from_secs(300),
        );
        sc.check(
            reply.ok(),
            &format!(
                "inflight: terminate(instance=...) works: {}",
                first_line(&reply.text)
            ),
        );
        sc.check(
            sc.wait_gone(&pod, 2 * MIN),
            "inflight: the pod is gone after terminate",
        );
    }
    sc.check(
        sc.machines_named(variant).len() <= 1,
        "inflight: no second pod",
    );
}

fn l8_lost_create_response(sc: &Scenario) {
    let variant = "lost";
    let project = sc.project(variant, &Config::runpod());
    let session = sc.session(variant);
    let proxy = ConnectProxy::start(Mode::Drop, machine_exists(sc, variant)).expect("proxy");
    let launch = Launch {
        env: vec![("HTTPS_PROXY".to_string(), proxy.url.clone())],
        ..Launch::default()
    };
    let srv = sc.server_with(&project, variant, &session, &launch);
    let began = Instant::now();
    let mut reply = srv.call("start", &json!({"label": variant}), START_REPLY);
    for attempt in 1..=6 {
        if proxy.triggered() || !(reply.is_error && is_capacity_error(&reply.text)) {
            break;
        }
        sc.say(&format!(
            "lost: no capacity on attempt {attempt}; retrying in 60 s"
        ));
        sc.sleep(MIN);
        reply = srv.call("start", &json!({"label": variant}), START_REPLY);
    }
    sc.timing("lost: start() reply", reply.secs);
    if !proxy.triggered() {
        bail_if_no_capacity(sc, &reply);
    }
    sc.check(
        proxy.triggered(),
        "lost: the proxy dropped a create response",
    );
    sc.say(&format!("lost: start replied: {}", reply.text));
    let Some(pod) = sc.wait_machine(variant, 2 * MIN) else {
        bail_if_no_capacity(sc, &reply);
        sc.inconclusive("lost: the dropped create left no pod (no capacity on the first GPU?)");
    };
    let machine_id = machine_id_of(&pod);
    sc.sleep(Duration::from_secs(30));
    sc.check(
        sc.machines_named(variant).len() == 1,
        "lost: exactly one pod, never a second create",
    );
    let adopted = sc.wait_for(10 * MIN, Duration::from_secs(10), || {
        let status = srv.call("status", &json!({}), QUICK);
        if !status.text.contains(&machine_id) {
            return None;
        }
        let reply = srv.call("create_kernel", &json!({"instance": machine_id}), QUICK);
        reply.ok().then_some(())
    });
    sc.timing("lost: start() to usable", began.elapsed().as_secs_f64());
    sc.check(
        adopted.is_some(),
        "lost: the pod is adopted and usable within 10 min",
    );
    terminate_and_verify(sc, &srv, &pod, &json!({"instance": machine_id}));
    sc.check(
        sc.machines_named(variant).is_empty(),
        "lost: nothing remains",
    );
}

/// L9: a budget runs out under a busy kernel. Warning, grace, stop. Then the
/// same with the provider's API blocked on the machine: the server must back
/// it up.
pub fn l9() {
    run_scenario("L9", limits(40), |sc| {
        l9_variant(sc, "plain", false);
        l9_variant(sc, "blocked", true);
    });
}

fn l9_variant(sc: &Scenario, variant: &str, block_api: bool) {
    const BUDGET: f64 = 0.02;
    const GRACE: u64 = 90;
    let config = Config::runpod()
        .budget(BUDGET)
        .with("cleanup = \"stop\"")
        .with(&format!("budget-grace-secs = {GRACE}"));
    let project = sc.project(variant, &config);
    let srv = sc.server(&project, variant, &sc.session(variant));
    let began = Instant::now();
    let up = start_and_kernel(sc, &srv, variant, &json!({"label": variant}));
    let rate = up.provider.running_rate;
    let exhausted_after = BUDGET / rate * 3600.0;
    sc.say(&format!(
        "{variant}: ${rate:.3}/hr: budget gone about {exhausted_after:.0}s after the start"
    ));

    if block_api {
        let blocked = py(
            &srv,
            &up.kernel,
            "import subprocess\nopen('/etc/hosts', 'a').write('127.0.0.1 api.runpod.io\\n127.0.0.1 rest.runpod.io\\n')\nprint('curl-exit', subprocess.run('curl -sm 5 -o /dev/null https://api.runpod.io/v2/pods', shell=True).returncode)\n",
            60,
        );
        sc.check(
            blocked.ok() && !blocked.mentions("curl-exit 0"),
            &format!(
                "{variant}: the API is unreachable from the pod: {}",
                blocked.text
            ),
        );
    }
    let cell = ticking_cell(180, 10, "/workspace/cell-done", "NEVER-FINISHES");
    sc.check(
        background(&srv, &up.kernel, &cell).ok(),
        "the busy cell starts",
    );

    // Watch the machine and what the agent gets told.
    let mut footers = String::new();
    let deadline = Duration::from_secs_f64(exhausted_after) + Duration::from_secs(GRACE + 300);
    let stopped_at = sc.wait_for(deadline, Duration::from_secs(20), || {
        let status = srv.call("status", &json!({}), QUICK);
        footers.push_str(&status.text);
        footers.push('\n');
        let state = sc.refresh(&up.provider);
        state
            .as_ref()
            .is_none_or(Machine::is_stopped)
            .then(|| began.elapsed().as_secs_f64())
    });
    let lower = footers.to_lowercase();
    sc.check(
        ["warning", "nearly", "almost", "running out", "80%", "95%"]
            .iter()
            .any(|needle| lower.contains(needle)),
        &format!("{variant}: the agent is warned as the budget runs out"),
    );

    match stopped_at {
        Some(at) => {
            sc.timing(&format!("{variant}: start to GPU released"), at);
            sc.note(&format!(
                "{variant}: estimated exhaustion at {exhausted_after:.0}s, grace {GRACE}s, stopped at {at:.0}s"
            ));
            sc.check(
                at >= exhausted_after - 90.0,
                &format!(
                    "{variant}: it did not stop early ({at:.0}s, exhaustion ~{exhausted_after:.0}s)"
                ),
            );
        }
        None => {
            sc.check(
                false,
                &format!("{variant}: the machine is still billing {GRACE}+300 s past exhaustion: no backstop"),
            );
            let reply = srv.call("stop", &json!({}), Duration::from_secs(300));
            sc.timing(&format!("{variant}: explicit stop()"), reply.secs);
            sc.check(
                reply.ok(),
                &format!("{variant}: stop() works: {}", first_line(&reply.text)),
            );
            sc.check(
                sc.wait_not_billing(&up.provider, 3 * MIN).is_some(),
                &format!("{variant}: stop() releases the GPU within 3 min"),
            );
        }
    }

    // Spend, three ways: what the server says, rate times time, the provider's bill.
    let status = srv.call("status", &json!({}), QUICK);
    let reported = session_cost(&status);
    let estimate = rate * began.elapsed().as_secs_f64() / 3600.0;
    sc.note(&format!(
        "{variant}: server-reported spend {reported:?}, rate x time ${estimate:.4}"
    ));
    sc.check(
        reported.is_some_and(|spent| spent <= BUDGET + rate * (GRACE as f64 + 330.0) / 3600.0),
        &format!("{variant}: reported spend stays near the budget: {reported:?}"),
    );
    let billed = sc.wait_for(5 * MIN, Duration::from_secs(20), || {
        providers::runpod_billed(&up.provider.id)
            .ok()
            .filter(|amount| *amount > 0.0)
    });
    sc.note(&format!(
        "{variant}: /v2/billing/pods says {billed:?} (polled up to 5 min)"
    ));
    if let (Some(billed), Some(reported)) = (billed, reported) {
        sc.check(
            (billed - reported).abs() <= 0.015 + 0.5 * billed,
            &format!("{variant}: reported ${reported:.3} is close to billed ${billed:.3}"),
        );
    }
    terminate_and_verify(sc, &srv, &up.provider, &json!({"instance": up.machine_id}));
}

/// L10: deliberate takeover. Refused without force; with it, the old session
/// hears within 90 s, the spend splits, the budget deadline moves.
pub fn l10() {
    run_scenario("L10", limits(30), |sc| {
        const NEW_BUDGET: f64 = 0.04;
        let config = Config::runpod()
            .with("cleanup = \"stop\"")
            .with("budget-grace-secs = 60");
        let project = sc.project("", &config);
        let budget = |dollars: f64| Launch {
            env: vec![("REMOTE_KERNELS_BUDGET".to_string(), dollars.to_string())],
            ..Launch::default()
        };
        let a = sc.server_with(&project, "a", &sc.session("a"), &budget(1.0));
        let up = start_and_kernel(sc, &a, "", &json!({"label": "l10"}));
        let b = sc.server_with(&project, "b", &sc.session("b"), &budget(NEW_BUDGET));

        let refused = b.call(
            "attach",
            &json!({"machine_id": up.machine_id}),
            Duration::from_secs(300),
        );
        sc.timing("plain attach() answer", refused.secs);
        sc.check(
            refused.is_error || refused.mentions("force"),
            &format!(
                "plain attach is refused while the other session drives: {}",
                refused.text
            ),
        );
        let still = py(&a, &up.kernel, "print('a-ok', 1 + 1)", 60);
        sc.check(
            still.mentions("a-ok 2"),
            &format!("a refused attach costs the driver nothing: {}", still.text),
        );
        let before = a.call("status", &json!({}), QUICK);
        sc.note(&format!(
            "session A spend before takeover: {:?}",
            session_cost(&before)
        ));

        let forced = b.call(
            "attach",
            &json!({"machine_id": up.machine_id, "force": true}),
            Duration::from_secs(300),
        );
        let moved_at = Instant::now();
        sc.timing("attach(force) answer", forced.secs);
        let b0 = session_cost(&b.call("status", &json!({}), QUICK));
        sc.check(
            forced.ok(),
            &format!("attach(force) takes over: {}", first_line(&forced.text)),
        );

        let told = sc.wait_for(Duration::from_secs(180), Duration::from_secs(5), || {
            let probe = py(&a, &up.kernel, "print('a-probe')", 30);
            let status = a.call("status", &json!({}), QUICK);
            let lost = |reply: &Reply| {
                [
                    "fenced",
                    "detached",
                    "another session",
                    "took over",
                    "taken over",
                    "force",
                ]
                .iter()
                .any(|needle| reply.mentions(needle))
            };
            (probe.is_error || lost(&probe) || lost(&status))
                .then(|| moved_at.elapsed().as_secs_f64())
        });
        match told {
            Some(after) => {
                sc.timing("old session told after", after);
                sc.check(
                    after <= 90.0,
                    &format!("the old session is told within 90 s (took {after:.0}s)"),
                );
            }
            None => sc.check(
                false,
                "the old session is never told it lost the machine (3 min)",
            ),
        }

        let kernel_b = wait_kernel(sc, &b, &up.machine_id, 5 * MIN);
        sc.check(kernel_b.is_some(), "the new session can use the machine");
        if let Some(kernel) = &kernel_b {
            let ran = py(&b, kernel, "print('b-ok', 2 + 2)", 60);
            sc.check(
                ran.mentions("b-ok 4"),
                &format!("the new session's code runs: {}", ran.text),
            );
        }

        // Let the split show at two decimals, then compare.
        sc.sleep(Duration::from_secs(180));
        let a1 = session_cost(&a.call("status", &json!({}), QUICK));
        let b1 = session_cost(&b.call("status", &json!({}), QUICK));
        sc.sleep(Duration::from_secs(120));
        let a2 = session_cost(&a.call("status", &json!({}), QUICK));
        let b2 = session_cost(&b.call("status", &json!({}), QUICK));
        sc.note(&format!(
            "spend: A {a1:?} -> {a2:?}, B {b0:?} -> {b1:?} -> {b2:?}"
        ));
        sc.check(
            matches!((a1, a2), (Some(x), Some(y)) if (y - x).abs() < 0.005),
            &format!("the old session stops being charged: {a1:?} -> {a2:?}"),
        );
        sc.check(
            matches!((b0, b2), (Some(x), Some(y)) if y > x),
            &format!("the new session is charged from here on: {b0:?} -> {b2:?}"),
        );

        // The machine now lives on the new session's small budget, not the old
        // session's dollar: at 25c/hr that is minutes, not hours.
        let by =
            Duration::from_secs_f64(NEW_BUDGET / up.provider.running_rate * 3600.0 + 60.0 + 300.0);
        let waited = moved_at.elapsed();
        let stopped = sc.wait_not_billing(&up.provider, by.saturating_sub(waited));
        sc.timing("takeover to budget stop", moved_at.elapsed().as_secs_f64());
        sc.check(
            stopped.is_some(),
            "the budget deadline moved to the new session's: the machine stops on its budget",
        );
        terminate_and_verify(sc, &b, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// L11: the machine dies during a slow save; and a failing save stops rather
/// than deletes, both by explicit call and by the machine's own cleanup.
pub fn l11() {
    run_scenario("L11", limits(35), |sc| {
        // The pod is deleted out from under a slow save.
        let config = Config::runpod().with("pre-terminate-command = \"sleep 280\"");
        let project = sc.project("dies", &config);
        let srv = sc.server(&project, "dies", &sc.session("dies"));
        let up = start_and_kernel(sc, &srv, "dies", &json!({"label": "l11"}));
        let mut call = srv.call_async("terminate", &json!({}));
        sc.sleep(Duration::from_secs(30));
        sc.check(!call.replied(), "dies: the slow save is running");
        providers::destroy(&up.provider).expect("delete the pod");
        let deleted_at = Instant::now();
        let reply = call.wait(Duration::from_secs(420));
        sc.timing(
            "dies: terminate() answered after the delete",
            deleted_at.elapsed().as_secs_f64(),
        );
        sc.check(
            reply.is_some() && deleted_at.elapsed() < Duration::from_secs(120),
            &format!("dies: terminate() answers within 2 min of the machine dying: {reply:?}"),
        );
        let settled = sc.wait_for(2 * MIN, Duration::from_secs(10), || {
            let status = srv.call("status", &json!({}), QUICK);
            (!status.text.contains(&up.machine_id)).then_some(())
        });
        sc.check(
            settled.is_some(),
            "dies: status() lets go of the deleted machine within 2 min",
        );
        srv.kill9();

        // A failing save, explicit terminate.
        let config = Config::runpod().with("pre-terminate-command = \"exit 7\"");
        let project = sc.project("fails", &config);
        let srv = sc.server(&project, "fails", &sc.session("fails"));
        let up = start_and_kernel(sc, &srv, "fails", &json!({"label": "l11b"}));
        let reply = srv.call("terminate", &json!({}), Duration::from_secs(300));
        sc.timing("fails: terminate() answer", reply.secs);
        sc.say(&format!("fails: terminate replied: {}", reply.text));
        let settled = sc.wait_not_billing(&up.provider, 3 * MIN);
        sc.check(
            matches!(&settled, Some((_, Some(_)))),
            &format!(
                "fails: a failed save stops the machine rather than deleting it (end state: {:?})",
                settled
                    .as_ref()
                    .map(|(_, s)| s.as_ref().map(|m| m.status.clone()))
            ),
        );
        if sc.refresh(&up.provider).is_some() {
            terminate_and_verify(
                sc,
                &srv,
                &up.provider,
                &json!({"instance": up.machine_id, "skip_pre_terminate_command": true}),
            );
        }
        srv.kill9();

        // A failing save, the machine's own cleanup with the server gone.
        let config = Config::runpod()
            .with("cleanup = \"terminate\"")
            .with("pre-terminate-command = \"exit 7\"");
        let project = sc.project("selfclean", &config);
        let session = sc.session("selfclean");
        let srv = sc.server(&project, "selfclean", &session);
        let up = start_and_kernel(sc, &srv, "selfclean", &json!({"label": "l11c"}));
        srv.kill9();
        let began = Instant::now();
        sc.sleep(STALE - Duration::from_secs(30));
        let settled = sc.wait_not_billing(&up.provider, 8 * MIN);
        sc.timing(
            "selfclean: server gone to GPU released",
            began.elapsed().as_secs_f64(),
        );
        sc.check(
            matches!(&settled, Some((_, Some(_)))),
            &format!(
                "selfclean: a failed save stops rather than deletes (end state: {:?})",
                settled
                    .as_ref()
                    .map(|(_, s)| s.as_ref().map(|m| m.status.clone()))
            ),
        );
        if sc.refresh(&up.provider).is_some() {
            let next = sc.server(&project, "selfclean-next", &session);
            terminate_and_verify(
                sc,
                &next,
                &up.provider,
                &json!({"instance": up.machine_id, "skip_pre_terminate_command": true}),
            );
        }
    });
}

/// L12: a machine started by the released binary survives the upgrade.
pub fn l12() {
    let Some(old_binary) = std::env::var_os("RK_LIVE_OLD_BINARY") else {
        eprintln!("L12 SKIPPED: set RK_LIVE_OLD_BINARY to a released remote-kernels binary");
        return;
    };
    run_scenario("L12", limits(25), |sc| {
        let project = sc.project("", &Config::runpod().budget(1.0));
        let session = sc.session("a");
        let old = Launch {
            binary: Some(old_binary.clone().into()),
            ..Launch::default()
        };
        let srv = sc.server_with(&project, "old", &session, &old);
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "l12"}));
        let ran = py(&srv, &up.kernel, "x = 41; print('old-binary ran')", 60);
        sc.check(ran.mentions("old-binary ran"), "the old binary runs code");
        sc.sleep(Duration::from_secs(120));
        let spent_before = session_cost(&srv.call("status", &json!({}), QUICK));
        sc.note(&format!("spend under the old binary: {spent_before:?}"));

        // A running cell, because the old binary deletes an idle machine the
        // moment its server disconnects (seen live; the new one doesn't).
        let cell = ticking_cell(20, 10, "/workspace/cell-done", "L12-CELL-FINISHED");
        let reply = background(&srv, &up.kernel, &cell);
        sc.check(reply.ok(), &format!("the cell starts: {}", reply.text));

        // The upgrade: the client restarts the server with the new binary.
        srv.disconnect();
        let exited = sc.wait_for(MIN, Duration::from_secs(1), || srv.exited().then_some(()));
        sc.check(exited.is_some(), "the old server exits on disconnect");
        srv.kill9();

        let new = sc.server(&project, "new", &session);
        sc.timing("new binary handshake", new.handshake_secs);
        let status = new.call("status", &json!({}), QUICK);
        sc.check(
            status.text.contains(&up.machine_id),
            &format!("the new binary knows the machine: {}", status.text),
        );
        let kernel_state = sc.wait_for(5 * MIN, Duration::from_secs(10), || {
            let reply = py(&new, &up.kernel, "print('state', x + 1)", 30);
            reply.ok().then_some(reply)
        });
        sc.check(
            kernel_state
                .as_ref()
                .is_some_and(|reply| reply.mentions("state 42")),
            &format!("the old kernel and its state carry over: {kernel_state:?}"),
        );

        // Past the old watchdog's whole stale window: nothing may clean it up.
        let until = Instant::now() + STALE + MIN;
        while Instant::now() < until {
            sc.check_silently_running(&up.provider, "the upgrade causes no cleanup");
            sc.sleep(Duration::from_secs(20));
        }
        let watchdogs = py(
            &new,
            &up.kernel,
            "import subprocess\nprint('watchdogs', subprocess.run('pgrep -fc rk-watchdo[g]', shell=True, capture_output=True, text=True).stdout.strip())\n",
            60,
        );
        sc.check(
            watchdogs.mentions("watchdogs 0"),
            &format!("the old binary's watchdog is gone: {}", watchdogs.text),
        );
        let spent_after = session_cost(&new.call("status", &json!({}), QUICK));
        sc.note(&format!("spend after the upgrade: {spent_after:?}"));
        sc.check(
            matches!((spent_before, spent_after), (Some(x), Some(y)) if y >= x),
            &format!("spend carries over: {spent_before:?} -> {spent_after:?}"),
        );
        terminate_and_verify(sc, &new, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// L13 (91670932, 77f84195): stop and resume; the meter reopens.
pub fn l13() {
    run_scenario("L13", limits(25), |sc| {
        let project = sc.project("", &Config::runpod().with("cleanup = \"stop\""));
        let session = sc.session("a");
        let srv = sc.server(&project, "a", &session);
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "l13"}));
        let wrote = py(
            &srv,
            &up.kernel,
            "open('/workspace/keep', 'w').write('kept'); print('wrote')",
            60,
        );
        sc.check(wrote.mentions("wrote"), "the kernel writes to the volume");
        let prices = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(remote_kernels::runpod::pricing::StoragePrices::fetch_from_page());
        sc.check(
            prices.is_ok(),
            &format!("RunPod's pricing page still reads: {prices:?}"),
        );
        sc.sleep(Duration::from_secs(60));
        let running = srv.call("status", &json!({}), QUICK);
        sc.note(&format!(
            "while running: status says {:?}, RunPod bills {:?}/hr",
            running.text.lines().find(|line| line.starts_with("Cost:")),
            providers::runpod_spend_per_hr().ok(),
        ));

        let stop = srv.call("stop", &json!({}), Duration::from_secs(300));
        sc.timing("stop()", stop.secs);
        sc.check(
            stop.ok(),
            &format!("stop() succeeds: {}", first_line(&stop.text)),
        );
        sc.check(
            sc.wait_not_billing(&up.provider, 3 * MIN).is_some(),
            "the GPU is released within 3 min of stop()",
        );

        let stopped = srv.call("status", &json!({}), QUICK);
        sc.check(
            stopped.mentions("still billing"),
            &format!("status says storage still bills: {}", stopped.text),
        );
        let c1 = session_cost(&stopped);
        sc.sleep(Duration::from_secs(120));
        let c2 = session_cost(&srv.call("status", &json!({}), QUICK));
        sc.note(&format!("while stopped: spend {c1:?} -> {c2:?}"));
        sc.note(&format!(
            "while stopped: status says {:?}, RunPod bills {:?}/hr",
            stopped.text.lines().find(|line| line.starts_with("Cost:")),
            providers::runpod_spend_per_hr().ok(),
        ));
        sc.check(
            matches!((c1, c2), (Some(x), Some(y)) if (y - x).abs() < 0.005),
            &format!("the meter holds while stopped: {c1:?} -> {c2:?}"),
        );

        let attach = attach_with_capacity_retry(sc, &srv, &up.machine_id);
        sc.check(attach.ok(), &format!("attach resumes: {}", attach.text));
        let kernel = wait_kernel(sc, &srv, &up.machine_id, 10 * MIN);
        sc.check(kernel.is_some(), "a kernel starts after the resume");
        if let Some(kernel) = &kernel {
            let read = py(
                &srv,
                kernel,
                "print('read', open('/workspace/keep').read())",
                60,
            );
            sc.check(
                read.mentions("read kept"),
                &format!("the volume survived: {}", read.text),
            );
        }
        let state = sc.refresh(&up.provider);
        sc.check(
            state.as_ref().is_some_and(|m| !m.is_stopped()),
            "the provider shows the pod running again",
        );
        let c3 = session_cost(&srv.call("status", &json!({}), QUICK));
        sc.sleep(Duration::from_secs(240));
        let c4 = session_cost(&srv.call("status", &json!({}), QUICK));
        sc.note(&format!("after resume: spend {c3:?} -> {c4:?}"));
        sc.check(
            matches!((c3, c4), (Some(x), Some(y)) if y > x),
            &format!("the meter reopened after the resume: {c3:?} -> {c4:?}"),
        );
        terminate_and_verify(sc, &srv, &up.provider, &json!({"instance": up.machine_id}));
    });
}

impl Scenario {
    /// Record a failure if the machine stopped billing, once for each message.
    pub fn check_silently_running(&self, machine: &Machine, what: &str) {
        let state = self.refresh(machine);
        if state.as_ref().is_none_or(Machine::is_stopped) {
            self.check(
                false,
                &format!(
                    "{what} (at {:.0}s it is {:?})",
                    self.elapsed_secs(),
                    state.map(|m| m.status)
                ),
            );
        }
    }
}
