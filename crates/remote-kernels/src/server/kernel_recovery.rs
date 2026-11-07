//! Output recording and kernel recovery. Each kernel gets a machine-side
//! recorder that appends its iopub messages to a JSONL log, so output produced
//! while no socket was connected can be backfilled into the local notebook:
//! when `attach()` reconnects the kernels, and when a socket that died comes
//! back ([`RemoteKernelsServer::reconnect_closed_kernels`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::jupyter::messages::ExecutionStatus;
use crate::jupyter::rest::KernelInfo;
use crate::jupyter::ws::KernelConnection;
use crate::runtime::{AnyConnection, Connection, InstanceStatus};
use crate::state::{InstanceRecord, KernelRecord, Phase, load_instance_record};

use super::RemoteKernelsServer;

/// How much of the recorder log a reattach replays. Sized for image outputs:
/// dozens of figures, or one high-dpi image, fit.
pub(super) const RECORDER_TAIL_BYTES: usize = 16 * 1024 * 1024;

/// Room to transfer the full tail over a slow link.
const RECORDER_TAIL_TIMEOUT: Duration = Duration::from_mins(1);

const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a tool call waits for a dropped connection to come back.
const INLINE_RECONNECT_WAIT: Duration = Duration::from_secs(15);

/// The recorder writes a cell's end within moments of the kernel going idle.
/// Well past that, no end is coming: the kernel died in the cell, say.
const RECORDER_LAG: Duration = Duration::from_secs(2);

/// What a kernel's connection being down means to the agent.
pub(super) const CONNECTION_DOWN: &str =
    "The connection to the machine dropped. It reconnects automatically; retry shortly.";

#[derive(Debug, Deserialize)]
pub(super) struct RecordedOutputMessage {
    parent_msg_id: String,
    msg_type: String,
    content: serde_json::Value,
    #[allow(dead_code)]
    ts: String,
}

/// What one pass over a kernel's recorder log found.
#[derive(Default)]
struct Collected {
    recovered: usize,
    notes: Vec<String>,
}

pub(super) struct RecorderTail {
    pub(super) messages: Vec<RecordedOutputMessage>,
    pub(super) skipped_lines: usize,
    pub(super) window_truncated: bool,
}

impl RemoteKernelsServer {
    /// Start the machine-side recorder for one kernel (see the module docs).
    pub(super) async fn install_output_recorder(
        connection: &AnyConnection,
        kernel_id: &str,
        token: &str,
    ) -> anyhow::Result<()> {
        let command = Self::output_recorder_command(connection, kernel_id, token)?;
        connection
            .exec(&command, std::time::Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    pub(super) fn output_recorder_command(
        connection: &AnyConnection,
        kernel_id: &str,
        token: &str,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !kernel_id.is_empty()
                && kernel_id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character)),
            "unsafe kernel id"
        );
        let state_dir = connection.state_dir();
        let bin_dir = format!("{state_dir}/bin");
        let output_dir = format!("{state_dir}/kernel-output");
        let script_path = format!("{bin_dir}/rk-output-recorder.py");
        let recorder_log = format!("{output_dir}/{kernel_id}.recorder.log");
        let source_hex = hex::encode(crate::machine_scripts::OUTPUT_RECORDER.as_bytes());
        Ok(format!(
            "mkdir -p {bin_dir} {output_dir} && python3 -c 'import sys; open(sys.argv[1], \"wb\").write(bytes.fromhex(sys.argv[2]))' {script_path} {source_hex} && chmod 700 {script_path} && (export REMOTE_KERNELS_JUPYTER_TOKEN={token}; nohup python3 {script_path} --kernel-id {kernel_id} --state-dir {state_dir} --ws-url {ws_url} --diagnostic-log {recorder_log} </dev/null >/dev/null 2>&1 &)",
            bin_dir = crate::machine_scripts::shell_quote(&bin_dir),
            output_dir = crate::machine_scripts::shell_quote(&output_dir),
            script_path = crate::machine_scripts::shell_quote(&script_path),
            source_hex = crate::machine_scripts::shell_quote(&source_hex),
            kernel_id = crate::machine_scripts::shell_quote(kernel_id),
            token = crate::machine_scripts::shell_quote(token),
            state_dir = crate::machine_scripts::shell_quote(&state_dir),
            ws_url = crate::machine_scripts::shell_quote(&format!(
                "ws://127.0.0.1:{}",
                connection.machine_jupyter_port()
            )),
            recorder_log = crate::machine_scripts::shell_quote(&recorder_log),
        ))
    }

    pub(super) async fn read_recorder_tail(
        connection: &AnyConnection,
        kernel_id: &str,
    ) -> anyhow::Result<RecorderTail> {
        let path = format!("{}/kernel-output/{kernel_id}.jsonl", connection.state_dir());
        let predecessor = format!("{path}.1");
        let command = format!(
            "if [ ! -f {path} ] && [ ! -f {predecessor} ]; then exit 0; fi; {{ [ ! -f {predecessor} ] || cat -- {predecessor}; [ ! -f {path} ] || cat -- {path}; }} | tail -c {RECORDER_TAIL_BYTES}",
            path = crate::machine_scripts::shell_quote(&path),
            predecessor = crate::machine_scripts::shell_quote(&predecessor),
        );
        let raw = connection.exec(&command, RECORDER_TAIL_TIMEOUT).await?;
        Ok(Self::parse_recorder_tail(&raw))
    }

    pub(super) fn parse_recorder_tail(raw: &str) -> RecorderTail {
        let lines: Vec<&str> = raw.lines().collect();
        let mut messages = Vec::new();
        let mut skipped_lines = 0;
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(message) => messages.push(message),
                Err(_) => skipped_lines += 1,
            }
        }
        RecorderTail {
            messages,
            skipped_lines,
            window_truncated: raw.len() == RECORDER_TAIL_BYTES,
        }
    }

    pub(super) fn fold_recorded_outputs(
        messages: Vec<RecordedOutputMessage>,
    ) -> BTreeMap<String, (crate::jupyter::messages::ExecutionOutput, bool)> {
        use crate::jupyter::messages::{ExecutionStatus, Header, JupyterMessage};

        let mut outputs = BTreeMap::new();
        let mut saw_busy = BTreeMap::<String, bool>::new();
        for message in messages {
            let entry = outputs
                .entry(message.parent_msg_id.clone())
                .or_insert_with(|| (crate::jupyter::messages::ExecutionOutput::default(), false));
            let jupyter_message = JupyterMessage {
                channel: "iopub".to_string(),
                header: Header {
                    msg_id: String::new(),
                    msg_type: message.msg_type.clone(),
                    username: String::new(),
                    session: String::new(),
                    date: String::new(),
                    version: String::new(),
                },
                parent_header: serde_json::json!({"msg_id": message.parent_msg_id}),
                metadata: serde_json::json!({}),
                content: message.content,
                buffers: Vec::new(),
            };
            entry.0.process_iopub(&jupyter_message);
            if message.msg_type == "status" {
                if jupyter_message.content["execution_state"] == "busy" {
                    saw_busy.insert(message.parent_msg_id.clone(), true);
                }
                if jupyter_message.content["execution_state"] == "idle"
                    && saw_busy.get(&message.parent_msg_id) == Some(&true)
                {
                    entry.1 = true;
                    if entry.0.status == ExecutionStatus::Running {
                        entry.0.status = ExecutionStatus::Complete;
                    }
                }
            }
        }
        outputs
    }

    pub(super) async fn recover_dead_binding(
        connection: &AnyConnection,
        binding: &KernelRecord,
        notebook_dir: &std::path::Path,
    ) -> (usize, Vec<String>) {
        let mut notes = Vec::new();
        let path = std::path::PathBuf::from(&binding.notebook_path);
        if !path.is_absolute() || !path.starts_with(notebook_dir) {
            return (0, vec!["its saved notebook path was invalid".to_string()]);
        }
        let mut notebook = match crate::notebook::Notebook::load(&path) {
            Ok(mut notebook) => match notebook.bind_for_recovery(&binding.kernel_id) {
                Ok(()) => notebook,
                Err(error) => {
                    return (
                        0,
                        vec![format!("its notebook could not be reopened ({error})")],
                    );
                }
            },
            Err(error) => return (0, vec![format!("its notebook could not be read ({error})")]),
        };
        let tail = match Self::read_recorder_tail(connection, &binding.kernel_id).await {
            Ok(tail) => tail,
            Err(error) => {
                return (
                    0,
                    vec![format!(
                        "the machine's output log could not be read ({error})"
                    )],
                );
            }
        };
        if tail.skipped_lines > 0 {
            notes.push(format!(
                "{} recorded output line(s) were unreadable and skipped",
                tail.skipped_lines
            ));
        }
        if tail.window_truncated {
            notes.push(
                "only the most recent outputs could be recovered (log truncated)".to_string(),
            );
        }
        let mut recovered = 0;
        for (parent_msg_id, (output, complete)) in Self::fold_recorded_outputs(tail.messages) {
            if !complete {
                continue;
            }
            match notebook.backfill_output(&parent_msg_id, &output, true) {
                Ok(Some(_)) => recovered += 1,
                Ok(None) => {}
                Err(error) => notes.push(format!(
                    "saving a disconnected output to the notebook failed ({error})"
                )),
            }
        }
        (recovered, notes)
    }

    /// A kernel that isn't on the machine any more: what the recorder has goes
    /// to its notebook, and this line says so.
    async fn dead_kernel_line(
        connection: &AnyConnection,
        binding: &KernelRecord,
        notebook_dir: &std::path::Path,
    ) -> String {
        let (recovered, notes) =
            Self::recover_dead_binding(connection, binding, notebook_dir).await;
        let mut line = format!(
            "Kernel {}: no longer exists on the machine — create a new kernel; {recovered} output(s) produced while disconnected were saved to its notebook ({})",
            binding.kernel_id, binding.notebook_path
        );
        if !notes.is_empty() {
            let _ = write!(line, "; {}", notes.join("; "));
        }
        line
    }

    /// A closed kernel that isn't on the machine any more. The report comes
    /// first: a call cut off before it just does this again next time.
    async fn forget_gone_kernel(
        &self,
        machine_id: &str,
        kernel_id: &str,
        connection: &AnyConnection,
        notebook_dir: &std::path::Path,
    ) {
        let binding = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return;
            };
            instance
                .kernels
                .iter()
                .find(|binding| binding.kernel_id == kernel_id)
                .cloned()
        };
        let report = match &binding {
            Some(binding) => Some(Self::dead_kernel_line(connection, binding, notebook_dir).await),
            None => None,
        };
        let mut state = self.state.lock().await;
        let Some(instance) = state.instances.get_mut(machine_id) else {
            return;
        };
        instance.kernel_ids.retain(|id| id != kernel_id);
        instance.kernel_connections.remove(kernel_id);
        instance.notebooks.remove(kernel_id);
        instance.forget_executions(kernel_id);
        instance
            .kernels
            .retain(|binding| binding.kernel_id != kernel_id);
        if let Err(error) = state.save_live(machine_id) {
            tracing::warn!(instance = %machine_id, "could not save its kernels: {error:#}");
        }
        drop(state);
        self.queue_alerts(report.into_iter().collect()).await;
    }

    /// What the recorder has for a kernel, or `None` if the machine can't be
    /// asked just now. One with no way to run commands has no log, and never will.
    async fn recorded_outputs(
        connection: &AnyConnection,
        kernel_id: &str,
        notes: &mut Vec<String>,
    ) -> Option<BTreeMap<String, (crate::jupyter::messages::ExecutionOutput, bool)>> {
        notes.clear();
        match Self::read_recorder_tail(connection, kernel_id).await {
            Ok(tail) => {
                if tail.skipped_lines > 0 {
                    notes.push(format!(
                        "{} recorded output line(s) were unreadable and skipped",
                        tail.skipped_lines
                    ));
                }
                if tail.window_truncated {
                    notes.push(
                        "only the most recent outputs could be recovered (log truncated)"
                            .to_string(),
                    );
                }
                Some(Self::fold_recorded_outputs(tail.messages))
            }
            Err(error) if error.to_string().contains("no SSH endpoint") => Some(BTreeMap::new()),
            Err(error) => {
                notes.push(format!("recorder log skipped ({error})"));
                None
            }
        }
    }

    /// Settle every running cell the recorder has an end for, and any on an idle
    /// kernel: that one's end isn't coming. A log that can't be read changes
    /// nothing, and the next pass reads it again.
    async fn collect_running_cells(
        &self,
        machine_id: &str,
        connection: &AnyConnection,
        live_kernels: &[KernelInfo],
    ) -> HashMap<String, Collected> {
        let watched: BTreeSet<String> = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return HashMap::new();
            };
            instance
                .running_cells
                .keys()
                .map(|(kernel_id, _)| kernel_id.clone())
                .collect()
        };
        let mut collected = HashMap::<String, Collected>::new();
        for kernel_id in watched {
            let found = collected.entry(kernel_id.clone()).or_default();
            let Some(mut folded) =
                Self::recorded_outputs(connection, &kernel_id, &mut found.notes).await
            else {
                continue;
            };
            // Read before the log, so "idle" is older than what the log says.
            let kernel_idle = !live_kernels.iter().any(|kernel| {
                kernel.id == kernel_id && kernel.execution_state.as_deref() == Some("busy")
            });
            let no_end_yet = self
                .state
                .lock()
                .await
                .instances
                .get(machine_id)
                .is_some_and(|instance| {
                    instance.running_cells.iter().any(|((id, _), cell)| {
                        *id == kernel_id
                            && !folded
                                .get(&cell.parent_msg_id)
                                .is_some_and(|(_, complete)| *complete)
                    })
                });
            if kernel_idle && no_end_yet {
                tokio::time::sleep(RECORDER_LAG).await;
                let Some(again) =
                    Self::recorded_outputs(connection, &kernel_id, &mut found.notes).await
                else {
                    continue;
                };
                folded = again;
            }
            let mut state = self.state.lock().await;
            let Some(instance) = state.instances.get_mut(machine_id) else {
                break;
            };
            let cells: Vec<(String, u32)> = instance
                .running_cells
                .keys()
                .filter(|(kernel, _)| *kernel == kernel_id)
                .cloned()
                .collect();
            for key in cells {
                let parent_msg_id = &instance.running_cells[&key].parent_msg_id;
                let (mut output, complete) = folded.remove(parent_msg_id).unwrap_or_default();
                if !complete {
                    if !kernel_idle {
                        continue;
                    }
                    output.push_stream(
                        true,
                        "\n[The kernel is idle, but the machine recorded no end for this cell: this may be only part of its output.]",
                    );
                    if output.status == ExecutionStatus::Running {
                        output.status = ExecutionStatus::Complete;
                    }
                }
                let cell = instance.running_cells.remove(&key).expect("listed");
                if let Some(notebook) = instance.notebooks.get_mut(&kernel_id) {
                    match notebook.backfill_output(&cell.parent_msg_id, &output, true) {
                        Ok(Some(_)) => found.recovered += 1,
                        Ok(None) => {}
                        Err(error) => found.notes.push(format!(
                            "saving a disconnected output to the notebook failed ({error})"
                        )),
                    }
                }
                let _ = cell.result_tx.send(output);
            }
        }
        collected
    }

    /// Jupyter isn't answering. If the machine has stopped or gone, say so and
    /// let go of it, so tools stop waiting for a connection that won't come back.
    async fn check_unreachable_machine(&self, machine_id: &str) {
        let Some(record) = load_instance_record(&self.project_dir, machine_id) else {
            return;
        };
        match self.check_record(machine_id, &record).await {
            Ok((status, message)) => {
                if matches!(status, InstanceStatus::Stopped | InstanceStatus::Gone) {
                    self.state
                        .lock()
                        .await
                        .remove_live(machine_id, record.provider_id());
                }
                self.queue_alerts(message.into_iter().collect()).await;
            }
            Err(message) => tracing::debug!(instance = %machine_id, "{message}"),
        }
    }

    /// Bring this machine's kernels back after their sockets died: each live
    /// one gets a new socket, each gone one is reported, and running cells that
    /// ended are collected. Does nothing if nothing needs it, and leaves
    /// everything as it was if the machine still can't be reached: the next
    /// call retries.
    pub(super) async fn reconnect_closed_kernels(&self, machine_id: &str) {
        let flight = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return;
            };
            Arc::clone(&instance.reconnect_flight)
        };
        let _flight = flight.lock_owned().await;
        let (jupyter, connection, token, closed) = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return;
            };
            let closed = instance.closed_kernels();
            if instance.detached
                || instance.phase != Phase::Running
                || (closed.is_empty() && instance.running_cells.is_empty())
            {
                return;
            }
            let Some(connection) = instance.connection.clone() else {
                return;
            };
            (
                instance.jupyter.clone(),
                connection,
                instance.jupyter_token.clone(),
                closed,
            )
        };
        connection.keep_transport_alive().await;
        let live_kernels = match jupyter.list_kernels().await {
            Ok(kernels) => kernels,
            Err(error) => {
                tracing::debug!(instance = %machine_id, "kernels not reconnected yet: {error}");
                self.check_unreachable_machine(machine_id).await;
                return;
            }
        };
        let notebook_dir = self.project_dir.join(&self.config.notebook_dir);
        let ws_base = &connection.jupyter().ws_base;
        for kernel_id in closed {
            if !live_kernels.iter().any(|kernel| kernel.id == kernel_id) {
                self.forget_gone_kernel(machine_id, &kernel_id, &connection, &notebook_dir)
                    .await;
                continue;
            }
            let socket = tokio::time::timeout(
                SOCKET_TIMEOUT,
                KernelConnection::connect(ws_base, &kernel_id, &token),
            )
            .await;
            match socket {
                Ok(Ok(socket)) => {
                    let mut state = self.state.lock().await;
                    // A restart or a new attach may have got there first.
                    if let Some(instance) = state.instances.get_mut(machine_id)
                        && !instance.detached
                        && instance.closed_kernels().contains(&kernel_id)
                    {
                        instance.reconnected(&kernel_id, Some(socket));
                    }
                }
                Ok(Err(error)) => {
                    tracing::debug!(%kernel_id, "kernel socket still down: {error}");
                }
                Err(_) => tracing::debug!(%kernel_id, "kernel socket still down: timed out"),
            }
        }
        self.collect_running_cells(machine_id, &connection, &live_kernels)
            .await;
    }

    /// The beat loop's way in: off its task, so a slow machine can't delay a beat.
    pub(super) fn reconnect_callback(&self, machine_id: &str) -> crate::heartbeat::Reachable {
        let server = self.clone();
        let machine_id = machine_id.to_string();
        Arc::new(move || {
            let server = server.clone();
            let machine_id = machine_id.clone();
            tokio::spawn(async move { server.reconnect_closed_kernels(&machine_id).await });
        })
    }

    /// A tool call's way in: try once, for a bounded time. Being cut off is
    /// harmless: what's done is recorded as it happens, and the next pass goes on.
    pub(super) async fn reconnect_kernel_now(&self, kernel_id: &str) {
        let machine_id = {
            let state = self.state.lock().await;
            state.instance_for_kernel(kernel_id).map(String::from)
        };
        if let Some(machine_id) = machine_id {
            let _ = tokio::time::timeout(
                INLINE_RECONNECT_WAIT,
                self.reconnect_closed_kernels(&machine_id),
            )
            .await;
        }
    }

    /// The same, for a call about every machine.
    pub(super) async fn reconnect_all_now(&self) {
        let machines: Vec<String> = self.state.lock().await.instances.keys().cloned().collect();
        let reconnects = machines
            .iter()
            .map(|machine_id| self.reconnect_closed_kernels(machine_id));
        let _ = tokio::time::timeout(INLINE_RECONNECT_WAIT, futures::future::join_all(reconnects))
            .await;
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn recover_attached_kernels(
        &self,
        machine_id: &str,
        durable_record: &InstanceRecord,
    ) -> String {
        let (jupyter, connection, ws_base, token, notebook_dir, detached) = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return "Previous kernels were not reconnected (the machine is no longer attached); create new kernels if needed.".to_string();
            };
            (
                instance.jupyter.clone(),
                instance.connection.clone(),
                instance
                    .connection
                    .as_ref()
                    .map(|connection| connection.jupyter().ws_base.clone()),
                instance.jupyter_token.clone(),
                self.project_dir.join(&self.config.notebook_dir),
                instance.detached,
            )
        };
        if detached {
            return "Previous kernels were not reconnected (this session no longer controls the machine); create new kernels if needed.".to_string();
        }
        let Some(connection) = connection else {
            return "Previous kernels were not reconnected (no connection to the machine); create new kernels if needed.".to_string();
        };
        let Some(ws_base) = ws_base else {
            return "Previous kernels were not reconnected (no Jupyter connection endpoint); create new kernels if needed.".to_string();
        };
        let live_kernels = match jupyter.list_kernels().await {
            Ok(kernels) => kernels,
            Err(error) => {
                return format!(
                    "Previous kernels were not reconnected (the machine's kernel list could not be read: {error}); create new kernels if needed."
                );
            }
        };
        let mut report = Vec::new();
        let mut recovered_records = Vec::new();
        let stale_bindings = durable_record
            .kernels
            .iter()
            .filter(|binding| {
                !live_kernels
                    .iter()
                    .any(|kernel| kernel.id == binding.kernel_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for binding in &stale_bindings {
            report.push(Self::dead_kernel_line(&connection, binding, &notebook_dir).await);
        }
        let mut reconnected = Vec::new();
        for kernel in &live_kernels {
            let binding = durable_record
                .kernels
                .iter()
                .find(|binding| binding.kernel_id == kernel.id)
                .cloned();
            let mut notes = Vec::new();
            let mut notebook = binding.as_ref().and_then(|binding| {
                let path = std::path::PathBuf::from(&binding.notebook_path);
                if !path.is_absolute() || !path.starts_with(&notebook_dir) {
                    notes.push("its saved notebook path was invalid".to_string());
                    return None;
                }
                match crate::notebook::Notebook::load(&path) {
                    Ok(mut notebook) => match notebook.bind_for_recovery(&kernel.id) {
                        Ok(()) => Some(notebook),
                        Err(error) => {
                            notes.push(format!("its notebook could not be reopened ({error})"));
                            None
                        }
                    },
                    Err(error) => {
                        notes.push(format!("its notebook could not be read ({error})"));
                        None
                    }
                }
            });
            if notebook.is_none() {
                let reason = if binding.is_some() {
                    "recovery: prior notebook unbindable"
                } else {
                    "recovery: no saved notebook for this kernel"
                };
                match crate::notebook::Notebook::new_continuation(
                    &notebook_dir,
                    &kernel.id,
                    binding.as_ref().and_then(|binding| binding.name.as_deref()),
                    reason,
                ) {
                    Ok(continuation) => {
                        notes.push(
                            "a fresh notebook was created for it (the prior one couldn't be reused)"
                                .to_string(),
                        );
                        notebook = Some(continuation);
                    }
                    Err(error) => notes.push(format!("creating a fresh notebook failed ({error})")),
                }
            }

            let websocket = match KernelConnection::connect(&ws_base, &kernel.id, &token).await {
                Ok(connection) => Some(connection),
                Err(error) => {
                    notes.push(format!("live output connection failed ({error})"));
                    None
                }
            };

            let notebook_path = notebook
                .as_ref()
                .map(|notebook| notebook.path().to_path_buf());
            {
                let mut state = self.state.lock().await;
                if let Some(instance) = state.instances.get_mut(machine_id) {
                    instance.kernel_ids.push(kernel.id.clone());
                    if let Some(notebook) = notebook {
                        instance.notebooks.insert(kernel.id.clone(), notebook);
                    }
                    instance.reconnected(&kernel.id, websocket);
                }
            }
            if let Some(path) = &notebook_path {
                recovered_records.push(KernelRecord {
                    kernel_id: kernel.id.clone(),
                    notebook_path: path.display().to_string(),
                    name: binding.and_then(|binding| binding.name),
                });
            }
            reconnected.push((kernel, notebook_path, notes));
        }

        let mut collected = self
            .collect_running_cells(machine_id, &connection, &live_kernels)
            .await;
        for (kernel, notebook_path, mut notes) in reconnected {
            let found = collected.remove(&kernel.id).unwrap_or_default();
            notes.extend(found.notes);
            let execution_state = kernel.execution_state.as_deref().unwrap_or("unknown");
            let mut line = format!(
                "Kernel {}: alive and reconnected ({execution_state}); {} output(s) produced while disconnected were added to its notebook{} — read it to catch up",
                kernel.id,
                found.recovered,
                notebook_path
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default()
            );
            let mut running = {
                let state = self.state.lock().await;
                state
                    .instances
                    .get(machine_id)
                    .map_or_else(Vec::new, |instance| {
                        instance
                            .running_cells
                            .keys()
                            .filter(|(id, _)| *id == kernel.id)
                            .map(|(_, cell_number)| cell_number.to_string())
                            .collect::<Vec<_>>()
                    })
            };
            running.sort();
            if !running.is_empty() {
                let _ = write!(
                    line,
                    "; cell(s) {} still running: wait() collects them",
                    running.join(", ")
                );
            }
            if !notes.is_empty() {
                let _ = write!(line, "; {}", notes.join("; "));
            }
            report.push(line);
        }

        let save_error = {
            let mut state = self.state.lock().await;
            if let Some(instance) = state.instances.get_mut(machine_id) {
                instance.kernels = recovered_records;
                state.save_live(machine_id).err()
            } else {
                None
            }
        };
        if let Some(error) = save_error {
            report.push(format!(
                "Warning: could not save the recovered kernel records to disk ({error}); after a server restart they won't reconnect automatically (the kernels themselves are fine now)."
            ));
        }
        if report.is_empty() {
            report.push("No previous kernels to reconnect.".to_string());
        }
        format!("Recovery:\n{}", report.join("\n"))
    }
}
