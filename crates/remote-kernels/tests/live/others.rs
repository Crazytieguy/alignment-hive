//! The vast scenarios, and the one that goes through a real `claude`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{Config, Limits, MIN, RUNPOD_ONLY, Scenario, VAST_ONLY, run_scenario};
use crate::mcp::{Launch, binary_under_test};
use crate::providers::{self, Machine, Provider, runpod_key, scrub, vast_key};
use crate::proxy::{ConnectProxy, Mode};
use crate::scenarios::{
    bail_if_no_capacity, first_line, machine_id_of, py, start_and_kernel, terminate_and_verify,
    ticking_cell, wait_kernel,
};

const QUICK: Duration = Duration::from_secs(120);
const STALE: Duration = Duration::from_secs(300);

/// V1: a vast container's whole life, the machine's own cleanup, and recovery
/// by label when a create's answer is lost.
pub fn v1() {
    let limits = Limits {
        minutes: 45,
        providers: VAST_ONLY,
    };
    run_scenario("V1", limits, |sc| {
        v1_lifecycle(sc);
        v1_label_recovery(sc);
    });
}

fn v1_lifecycle(sc: &Scenario) {
    let project = sc.project("life", &Config::vast());
    std::fs::write(project.join("mod.py"), "VALUE = 7\n").unwrap();
    let srv = sc.server(&project, "life", &sc.session("life"));
    let up = start_and_kernel(sc, &srv, "life", &json!({"label": "v1"}));
    let synced = srv.call("sync", &json!({}), Duration::from_secs(300));
    sc.check(
        synced.ok(),
        &format!("sync() succeeds: {}", first_line(&synced.text)),
    );
    let ran = py(
        &srv,
        &up.kernel,
        "import subprocess, mod\nprint('value', mod.VALUE)\nprint(subprocess.run(['nvidia-smi', '-L'], capture_output=True, text=True).stdout)\nopen('out.txt', 'w').write('from vast')\n",
        120,
    );
    sc.check(
        ran.mentions("value 7"),
        &format!("code runs and sees synced files: {}", ran.text),
    );
    sc.check(
        ran.mentions("GPU 0"),
        &format!("the GPU is visible: {}", ran.text),
    );
    let download = srv.call(
        "download",
        &json!({"remote_path": "out.txt", "local_path": "out.txt"}),
        Duration::from_secs(300),
    );
    sc.check(
        download.ok(),
        &format!("download() succeeds: {}", first_line(&download.text)),
    );
    sc.check(
        std::fs::read_to_string(project.join("out.txt")).is_ok_and(|text| text == "from vast"),
        "the downloaded file arrived",
    );

    // The client goes away; the machine must clean up after itself.
    srv.disconnect();
    let began = Instant::now();
    sc.sleep(STALE - Duration::from_secs(60));
    sc.check_silently_running(
        &up.provider,
        "the machine survives the first 4 min after a disconnect",
    );
    let settled = sc.wait_not_billing(&up.provider, 10 * MIN);
    sc.timing("disconnect to GPU released", began.elapsed().as_secs_f64());
    sc.check(
        settled.is_some(),
        "the machine releases its GPU by itself within 14 min of a disconnect",
    );
    if let Some((_, end_state)) = settled {
        sc.note(&format!(
            "after self-cleanup with cleanup=terminate the instance is: {:?}",
            end_state.as_ref().map(|m| m.status.clone())
        ));
        sc.check(
            end_state.is_none(),
            "self-cleanup with cleanup=terminate destroys the instance, so storage stops billing",
        );
    } else if let Some(state) = sc.refresh(&up.provider) {
        sc.note(&format!(
            "still billing after self-cleanup should have acted: {}",
            state.describe()
        ));
    }
    srv.kill9();
}

fn v1_label_recovery(sc: &Scenario) {
    let project = sc.project("lost", &Config::vast());
    let prefix = format!("{}lost", sc.prefix);
    let exists =
        move || providers::list(Provider::Vast, &prefix).is_ok_and(|found| !found.is_empty());
    let proxy = ConnectProxy::start(Mode::Drop, exists).expect("proxy");
    let launch = Launch {
        env: vec![("HTTPS_PROXY".to_string(), proxy.url.clone())],
        ..Launch::default()
    };
    let srv = sc.server_with(&project, "lost", &sc.session("lost"), &launch);
    let reply = srv.call(
        "start",
        &json!({"label": "v1-lost"}),
        Duration::from_secs(1200),
    );
    sc.timing("lost: start() reply", reply.secs);
    sc.say(&format!("lost: start replied: {}", reply.text));
    sc.check(
        proxy.triggered(),
        "lost: the proxy dropped a create response",
    );
    let Some(machine) = sc.wait_machine("lost", 2 * MIN) else {
        bail_if_no_capacity(sc, &reply);
        sc.inconclusive("lost: the dropped create left no instance (offer gone?)");
    };
    sc.sleep(Duration::from_secs(30));
    let all = sc.machines_named("lost");
    sc.check(
        all.len() == 1,
        &format!(
            "lost: exactly one instance after a lost create, not one per offer: {:?}",
            all.iter().map(Machine::describe).collect::<Vec<_>>()
        ),
    );
    let machine_id = machine_id_of(&machine);
    let status = srv.call("status", &json!({}), QUICK);
    sc.check(
        status.text.contains(&machine_id) || status.mentions(&machine.id),
        &format!("lost: the instance is recovered by label: {}", status.text),
    );
    let kernel = wait_kernel(sc, &srv, &machine_id, 10 * MIN);
    sc.check(kernel.is_some(), "lost: the recovered instance is usable");
    terminate_and_verify(sc, &srv, &machine, &json!({"instance": machine_id}));
}

/// V3: with `cleanup = stop`, a vast container stops itself after a disconnect
/// through the provider's API, keeping its disk, and can be picked up again.
pub fn v3() {
    let limits = Limits {
        minutes: 35,
        providers: VAST_ONLY,
    };
    run_scenario("V3", limits, |sc| {
        let project = sc.project("", &Config::vast().with("cleanup = \"stop\""));
        let session = sc.session("a");
        let srv = sc.server(&project, "a", &session);
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "v3"}));
        let wrote = py(
            &srv,
            &up.kernel,
            "open('/root/kept', 'w').write('kept'); print('wrote')",
            60,
        );
        sc.check(wrote.mentions("wrote"), "the kernel writes to the disk");

        srv.disconnect();
        let began = Instant::now();
        sc.sleep(STALE - Duration::from_secs(60));
        sc.check_silently_running(
            &up.provider,
            "the machine survives the first 4 min after a disconnect",
        );
        let settled = sc.wait_not_billing(&up.provider, 10 * MIN);
        sc.timing("disconnect to GPU released", began.elapsed().as_secs_f64());
        let Some((_, end_state)) = settled else {
            sc.check(
                false,
                "the machine stops itself within 14 min of a disconnect",
            );
            return;
        };
        sc.check(
            end_state.as_ref().is_some_and(Machine::is_stopped),
            &format!(
                "self-cleanup with cleanup=stop keeps the instance, stopped: {:?}",
                end_state.as_ref().map(|machine| machine.status.clone())
            ),
        );

        let back = sc.server(&project, "b", &session);
        let status = back.call("status", &json!({}), QUICK);
        sc.check(
            status.mentions("stopped") && status.text.contains(&up.machine_id),
            &format!("a new server reports it stopped: {}", status.text),
        );
        sc.check(
            status.mentions("still billing") || status.mentions("storage"),
            &format!("status says storage still bills: {}", status.text),
        );

        let attach = back.call(
            "attach",
            &json!({"machine_id": up.machine_id}),
            Duration::from_secs(900),
        );
        if attach.ok() {
            let kernel = wait_kernel(sc, &back, &up.machine_id, 10 * MIN);
            sc.check(kernel.is_some(), "a kernel starts after the resume");
            if let Some(kernel) = &kernel {
                let read = py(
                    &back,
                    kernel,
                    "print('read', open('/root/kept').read())",
                    60,
                );
                sc.check(
                    read.mentions("read kept"),
                    &format!("the disk survived the stop: {}", read.text),
                );
            }
        } else {
            sc.note(&format!(
                "vast could not resume it (the GPU may be re-rented), which it warns about: {}",
                attach.text
            ));
        }
        terminate_and_verify(sc, &back, &up.provider, &json!({"instance": up.machine_id}));
    });
}

/// V2 (b561bd89): a vast VM runs Docker and Inspect, and cleans up after itself.
pub fn v2() {
    let limits = Limits {
        minutes: 60,
        providers: VAST_ONLY,
    };
    run_scenario("V2", limits, |sc| {
        let mut config = Config::vast()
            .with("vm = true")
            .with("image = \"vastai/kvm:ubuntu_terminal\"")
            .with("workdir = \"/root/workspace\"")
            .with("jupyter-command = \"/root/.local/bin/uv run --with jupyter-server --with ipykernel jupyter server\"")
            .with(
                "onstart = [\"exec > /var/tmp/rk-onstart-user.log 2>&1; date; ps -p 1 -o comm=\", \
                 \"docker info >/dev/null 2>&1 || (curl -fsSL https://get.docker.com | sh)\", \
                 \"for _ in $(seq 60); do docker info >/dev/null 2>&1 && break; sleep 2; done\", \
                 \"command -v /root/.local/bin/uv >/dev/null || (curl -LsSf https://astral.sh/uv/install.sh | sh)\"]",
            );
        config.section.retain(|line| {
            !line.starts_with("disk-gb") && !line.starts_with("provision-timeout-mins")
        });
        let config = config
            .with("disk-gb = 45.0")
            .with("provision-timeout-mins = 30");
        let project = sc.project("", &config);
        std::fs::write(
            project.join("hello_task.py"),
            "from inspect_ai import Task, task\nfrom inspect_ai.dataset import Sample\nfrom inspect_ai.scorer import includes\nfrom inspect_ai.solver import generate\n\n@task\ndef hello():\n    return Task(dataset=[Sample(input='Say hello', target='hello')], solver=generate(), scorer=includes())\n",
        )
        .unwrap();
        let srv = sc.server(&project, "a", &sc.session("a"));
        let up = start_and_kernel(sc, &srv, "", &json!({"label": "v2"}));
        let docker = py(
            &srv,
            &up.kernel,
            "import subprocess; r = subprocess.run(['docker', 'run', '--rm', 'hello-world'], capture_output=True, text=True, timeout=240); print(r.stdout or r.stderr)",
            300,
        );
        sc.check(
            docker.mentions("Hello from Docker"),
            &format!("Docker works inside the VM: {}", docker.text),
        );
        let synced = srv.call("sync", &json!({}), Duration::from_secs(300));
        sc.check(
            synced.ok(),
            &format!("sync() succeeds: {}", first_line(&synced.text)),
        );
        let inspect = py(
            &srv,
            &up.kernel,
            "import subprocess; r = subprocess.run(['/root/.local/bin/uv', 'run', '--with', 'inspect-ai', 'inspect', 'eval', 'hello_task.py', '--model', 'mockllm/model', '--log-dir', 'logs'], capture_output=True, text=True, timeout=600, cwd='/root/workspace'); print(r.stdout[-1500:] + r.stderr[-1500:]); print('inspect-exit', r.returncode)",
            700,
        );
        sc.check(
            inspect.mentions("inspect-exit 0"),
            &format!("an Inspect eval runs: {}", inspect.text),
        );

        srv.disconnect();
        let began = Instant::now();
        sc.sleep(STALE - Duration::from_secs(60));
        sc.check_silently_running(
            &up.provider,
            "the VM survives the first 4 min after a disconnect",
        );
        let settled = sc.wait_not_billing(&up.provider, 12 * MIN);
        sc.timing("disconnect to GPU released", began.elapsed().as_secs_f64());
        sc.check(
            settled.is_some(),
            "the VM releases its GPU by itself within 16 min",
        );
        if let Some((_, end_state)) = settled {
            sc.note(&format!(
                "after self-cleanup with cleanup=terminate the VM is: {:?}",
                end_state.as_ref().map(|m| m.status.clone())
            ));
            sc.check(
                end_state.is_none(),
                "self-cleanup with cleanup=terminate destroys the VM, so storage stops billing",
            );
        } else if let Some(state) = sc.refresh(&up.provider) {
            sc.note(&format!(
                "still billing after self-cleanup should have acted: {}",
                state.describe()
            ));
        }
        srv.kill9();
    });
}

/// One run of `claude -p`, with its stream-json output collected.
struct ClaudeRun {
    child: std::process::Child,
    lines: Arc<Mutex<Vec<Value>>>,
}

impl ClaudeRun {
    fn spawn(
        sc: &Scenario,
        config_dir: &std::path::Path,
        mcp_config: &std::path::Path,
        project: &std::path::Path,
        resume: Option<&str>,
        prompt: &str,
        log: &str,
    ) -> Self {
        let mut command = Command::new("claude");
        command
            .current_dir(project)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            // A scratch config, never the user's; empty means "the default
            // keychain entry", which is what keeps the login.
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", "")
            .env("RUNPOD_API_KEY", runpod_key())
            .env("VAST_API_KEY", vast_key())
            .env("REMOTE_KERNELS_KEYS_ROOT", &sc.keys_root)
            .env("RUST_LOG", "remote_kernels=info,rmcp=warn")
            .args(["-p", prompt])
            .args(["--output-format", "stream-json", "--verbose"])
            .args(["--model", "haiku"])
            .arg("--mcp-config")
            .arg(mcp_config)
            .arg("--strict-mcp-config")
            .args(["--allowedTools", "mcp__rk"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(session) = resume {
            command.args(["--resume", session]);
        }
        let mut child = command.spawn().expect("spawning claude");
        let stdout = child.stdout.take().expect("piped");
        let lines: Arc<Mutex<Vec<Value>>> = Arc::default();
        {
            let lines = Arc::clone(&lines);
            let log_path = sc.log_dir.join(log);
            std::thread::spawn(move || {
                let mut out = std::fs::File::create(log_path).ok();
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(out) = &mut out {
                        use std::io::Write as _;
                        let _ = writeln!(out, "{}", scrub(&line));
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        lines.lock().expect("lines").push(value);
                    }
                }
            });
        }
        Self { child, lines }
    }

    fn session_id(&self) -> Option<String> {
        self.lines
            .lock()
            .expect("lines")
            .iter()
            .find_map(|line| line.get("session_id")?.as_str().map(String::from))
    }

    fn result_text(&self) -> String {
        self.lines
            .lock()
            .expect("lines")
            .iter()
            .filter_map(|line| line.get("result")?.as_str().map(String::from))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn signal(&self, signal: &str) {
        let _ = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .status();
    }

    /// Wait for exit, ending the process if it runs past `timeout`.
    fn finish(&mut self, sc: &Scenario, timeout: Duration) -> bool {
        let done = sc
            .wait_for(timeout, Duration::from_secs(1), || {
                self.child.try_wait().ok().flatten()
            })
            .is_some();
        if !done {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        done
    }
}

impl Drop for ClaudeRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// C1 (cb617169, ce606857): a real Claude Code client. A long wait() is
/// cancelled, the MCP server comes back with the resumed session, and the
/// cell's result is still collectable.
pub fn c1() {
    let limits = Limits {
        minutes: 40,
        providers: RUNPOD_ONLY,
    };
    run_scenario("C1", limits, |sc| {
        let project = sc.project("", &Config::runpod());
        let config_dir = sc.dir.join("claude-config");
        std::fs::create_dir_all(&config_dir).unwrap();
        // The keys reach the server through the environment `claude` passes
        // down; nothing secret goes in this file.
        let mcp_config = sc.dir.join("mcp.json");
        std::fs::write(
            &mcp_config,
            json!({"mcpServers": {"rk": {
                "command": binary_under_test(),
                "args": ["--project-dir", project],
                "timeout": 3_600_000,
            }}})
            .to_string(),
        )
        .unwrap();

        let cell = ticking_cell(48, 10, "/workspace/cell-done", "C1-CELL-FINISHED");
        let first_prompt = format!(
            "This is an automated infrastructure test. Use only the mcp__rk tools and do exactly this, with no commentary:\n\
             1. Call start with label \"c1\". If it fails for lack of GPU capacity, wait a minute and call it again, up to 8 times. If it says the machine is still provisioning, call status every so often until it is running.\n\n\
             2. Call create_kernel.\n\
             3. Call execute with that kernel_id, background=true and exactly this code:\n{cell}\n\
             4. Call wait with that kernel_id and no timeout. Do nothing else until it returns."
        );
        let began = Instant::now();
        let mut first = ClaudeRun::spawn(
            sc,
            &config_dir,
            &mcp_config,
            &project,
            None,
            &first_prompt,
            "claude-1.jsonl",
        );
        // Claude's stream can arrive only when it exits, so "it reached wait()"
        // is timed from the pod being up, well inside the 8 min cell.
        let pod = sc.wait_for(15 * MIN, Duration::from_secs(3), || {
            if first.child.try_wait().ok().flatten().is_some() {
                panic!(
                    "claude exited before the cell ended: {}",
                    first.result_text()
                );
            }
            sc.wait_machine("", Duration::from_secs(1))
        });
        let pod = pod.expect("no pod within 15 min of starting claude");
        sc.timing("pod up", began.elapsed().as_secs_f64());
        sc.sleep(3 * MIN);
        assert!(
            first.child.try_wait().ok().flatten().is_none(),
            "claude exited before the cell ended: {}",
            first.result_text()
        );
        let session = first.session_id().unwrap_or_else(|| {
            panic!("no session id from claude: its stream is not arriving live");
        });
        sc.say(&format!(
            "claude session {session} is working on {}",
            pod.describe()
        ));

        first.signal("INT");
        let exited = first.finish(sc, MIN);
        sc.note(&format!("claude exited on SIGINT within 60 s: {exited}"));
        sc.check_silently_running(&pod, "cancelling the wait leaves the machine running");

        // The MCP server respawns with the resumed session.
        let second_prompt = "Automated infrastructure test, continued. Use only the mcp__rk tools, with no commentary. \
             The wait() you started was cancelled. Call status. If this session's machine is not attached, call attach \
             with its id (force=true if plain attach is refused). Then collect the cell's result with wait or get_output, \
             until the code has finished. Then call terminate. Your last line must be exactly C1RESULT= followed by the \
             last line the cell printed.";
        let resumed = Instant::now();
        let mut second = ClaudeRun::spawn(
            sc,
            &config_dir,
            &mcp_config,
            &project,
            Some(&session),
            second_prompt,
            "claude-2.jsonl",
        );
        let done = second.finish(sc, 15 * MIN);
        sc.timing("resumed session finished", resumed.elapsed().as_secs_f64());
        sc.check(done, "the resumed session finishes within 15 min");
        let result = second.result_text();
        sc.note(&format!(
            "resumed session's result: {}",
            first_line(&result)
        ));
        sc.check(
            result.contains("C1RESULT=C1-CELL-FINISHED"),
            &format!("the cell's result survives the cancel and the respawn: {result}"),
        );
        sc.check(
            sc.wait_gone(&pod, 3 * MIN),
            "terminate from the resumed session removes the machine",
        );
    });
}
