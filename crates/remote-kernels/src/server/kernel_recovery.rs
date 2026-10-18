//! Output recording and kernel recovery across reattach. Each kernel gets a
//! machine-side recorder that appends its iopub messages to a JSONL log, so
//! output produced while no server was connected can be backfilled into the
//! local notebook when `attach()` reconnects the kernels.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Deserialize;

use crate::runtime::{AnyConnection, Connection};
use crate::state::{InstanceRecord, KernelRecord};

use super::RemoteKernelsServer;

/// How much of the recorder log a reattach replays. Sized for image outputs:
/// dozens of figures, or one high-dpi image, fit.
pub(super) const RECORDER_TAIL_BYTES: usize = 16 * 1024 * 1024;

/// Room to transfer the full tail over a slow link.
const RECORDER_TAIL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

#[derive(Debug, Deserialize)]
pub(super) struct RecordedOutputMessage {
    parent_msg_id: String,
    msg_type: String,
    content: serde_json::Value,
    #[allow(dead_code)]
    ts: String,
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
        let state_dir = crate::machine_scripts::state_dir(connection.workdir());
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
            ws_url = crate::machine_scripts::shell_quote(&connection.recorder_ws_url()),
            recorder_log = crate::machine_scripts::shell_quote(&recorder_log),
        ))
    }

    pub(super) async fn read_recorder_tail(
        connection: &AnyConnection,
        kernel_id: &str,
    ) -> anyhow::Result<RecorderTail> {
        let path = format!(
            "{}/kernel-output/{kernel_id}.jsonl",
            crate::machine_scripts::state_dir(connection.workdir())
        );
        let predecessor = format!("{path}.1");
        let command = format!(
            "if [ ! -f {path} ] && [ ! -f {predecessor} ]; then exit 1; fi; {{ [ ! -f {predecessor} ] || cat -- {predecessor}; [ ! -f {path} ] || cat -- {path}; }} | tail -c {RECORDER_TAIL_BYTES}",
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

    #[allow(clippy::too_many_lines)]
    pub(super) async fn recover_attached_kernels(
        &self,
        machine_id: &str,
        durable_record: &InstanceRecord,
    ) -> String {
        let (jupyter, connection, ws_base, token, notebook_dir, fenced, external_id) = {
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
                state.project_dir.join(&self.config.notebook_dir),
                instance.fenced,
                instance.external_id.clone(),
            )
        };
        if fenced.is_some() {
            return "Previous kernels were not reconnected (this session no longer controls the machine); create new kernels if needed.".to_string();
        }
        let _recovery_guard = match self.recovery_guard(machine_id, &external_id).await {
            Ok(guard) => guard,
            Err(error) => {
                return format!(
                    "Previous kernels were not reconnected (could not confirm this session controls the machine: {error}); create new kernels if needed."
                );
            }
        };
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
            let (recovered, notes) =
                Self::recover_dead_binding(&connection, binding, &notebook_dir).await;
            let mut line = format!(
                "Kernel {}: no longer exists on the machine — create a new kernel; {recovered} output(s) produced while disconnected were saved to its notebook ({})",
                binding.kernel_id, binding.notebook_path
            );
            if !notes.is_empty() {
                let _ = write!(line, "; {}", notes.join("; "));
            }
            report.push(line);
        }
        for kernel in live_kernels {
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
                    "recovery: no durable notebook mapping"
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

            let websocket =
                match crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel.id, &token)
                    .await
                {
                    Ok(connection) => Some(connection),
                    Err(error) => {
                        notes.push(format!("live output connection failed ({error})"));
                        None
                    }
                };

            let mut recovered = Vec::new();
            if let Some(notebook) = notebook.as_mut() {
                match Self::read_recorder_tail(&connection, &kernel.id).await {
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
                        for (parent_msg_id, (output, complete)) in
                            Self::fold_recorded_outputs(tail.messages)
                        {
                            // A partial tail stays a placeholder. Writing it
                            // would make a later attach unable to apply the
                            // complete ordered message group without either
                            // duplicating or replacing live-path output.
                            if !complete {
                                continue;
                            }
                            match notebook.backfill_output(&parent_msg_id, &output, true) {
                                Ok(Some(cell_number)) => {
                                    recovered.push((cell_number, output));
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    notes.push(format!(
                                        "saving a disconnected output to the notebook failed ({error})"
                                    ));
                                }
                            }
                        }
                    }
                    Err(error) => notes.push(format!("recorder log skipped ({error})")),
                }
            }
            let recovered_count = recovered.len();

            let notebook_path = notebook
                .as_ref()
                .map(|notebook| notebook.path().to_path_buf());
            let notebook_display = notebook_path
                .as_ref()
                .map(|path| path.display().to_string());
            {
                let mut state = self.state.lock().await;
                if let Some(instance) = state.instances.get_mut(machine_id) {
                    instance.kernel_ids.push(kernel.id.clone());
                    if let Some(websocket) = websocket {
                        instance
                            .kernel_connections
                            .insert(kernel.id.clone(), websocket);
                    }
                    if let Some(notebook) = notebook {
                        instance.notebooks.insert(kernel.id.clone(), notebook);
                    }
                    for (cell_number, output) in recovered {
                        instance
                            .recovered_executions
                            .insert((kernel.id.clone(), cell_number), output);
                    }
                }
            }
            if let Some(path) = notebook_path {
                recovered_records.push(KernelRecord {
                    kernel_id: kernel.id.clone(),
                    notebook_path: path.display().to_string(),
                    name: binding.and_then(|binding| binding.name),
                });
            }
            let execution_state = kernel.execution_state.as_deref().unwrap_or("unknown");
            let mut line = format!(
                "Kernel {}: alive and reconnected ({execution_state}); {recovered_count} output(s) produced while disconnected were added to its notebook{} — read it to catch up",
                kernel.id,
                notebook_display
                    .map(|path| format!(" ({path})"))
                    .unwrap_or_default()
            );
            if !notes.is_empty() {
                let _ = write!(line, "; {}", notes.join("; "));
            }
            report.push(line);
        }

        let save_error = {
            let mut state = self.state.lock().await;
            if let Some(instance) = state.instances.get_mut(machine_id) {
                instance.kernels = recovered_records;
                let record = instance.record();
                state.save_record(machine_id, &record).err()
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
