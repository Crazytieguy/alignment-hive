//! The engines behind `execute`, `wait` and `get_output`. A timed-out or
//! background execution parks its result receiver in the instance's
//! `pending_executions`; a call that takes it out reinserts it if the
//! execution is still running, so a later call can still collect the result.

use std::fmt::Write;

use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam, RequestMetaObject};
use rmcp::{ErrorData as McpError, Peer, RoleServer};

use crate::config::Cleanup;
use crate::images::ImageCollector;
use crate::jupyter::messages::ExecutionOutput;

use super::params::{ExecuteParams, GetOutputParams, WaitParams};
use super::{RemoteKernelsServer, err_text};

struct HeldExecution {
    result_rx: tokio::sync::oneshot::Receiver<ExecutionOutput>,
    machine_id: String,
    kernel_id: String,
    cell_number: Option<u32>,
    cleanup: Cleanup,
}

/// Result of holding one pending execution open (see `wait_execution`).
enum WaitOutcome {
    Completed(ExecutionOutput),
    StillRunning,
    Fenced(String),
    ConnectionLost,
}

/// Render an output's reply text, adding its images to `images`. Image
/// decoding runs on the blocking pool.
async fn render_output(
    output: ExecutionOutput,
    mut images: ImageCollector,
) -> (String, ImageCollector) {
    tokio::task::spawn_blocking(move || {
        let text = output.render(&mut images);
        (text, images)
    })
    .await
    .unwrap_or_else(|_| {
        (
            "[output could not be rendered]".to_string(),
            ImageCollector::default(),
        )
    })
}

/// An execution result: the text, then its images in marker order.
fn execution_reply(body: String, images: ImageCollector, is_error: bool) -> CallToolResult {
    let mut content = vec![ContentBlock::text(body)];
    content.extend(
        images
            .images
            .into_iter()
            .map(|image| ContentBlock::image(image.base64, image.mime)),
    );
    if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Clamp a tool-supplied timeout to one year: `Instant + huge Duration`
/// (and oversized tokio sleeps) panic, and anything past this is
/// indistinguishable from "no cap" anyway.
fn clamp_timeout_secs(secs: u64) -> u64 {
    secs.min(31_536_000)
}

impl RemoteKernelsServer {
    #[allow(clippy::collapsible_if, clippy::too_many_lines)]
    pub(super) async fn execute_inner(
        &self,
        params: ExecuteParams,
        progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;

        let background = params.background.unwrap_or(false);
        if background && params.timeout.is_some() {
            return err_text(
                "`background` and `timeout` are mutually exclusive. background=true returns \
                 immediately (collect the result with get_output()); use `timeout` to wait — \
                 0 waits without a cap.",
            );
        }
        let wait_uncapped = params.timeout == Some(0);
        let timeout_secs = params.timeout.unwrap_or(30);
        let queue = params.queue.unwrap_or(false);

        let (guard_instance, guard_external_id) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let instance = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(instance) {
                return err_text(message);
            }
            (machine_id, instance.external_id.clone())
        };
        let mutation_guard = match self
            .mutation_guard(&guard_instance, &guard_external_id)
            .await
        {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        let (mut result_rx, cell_number, kernel_id, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            if crate::state::load_lifecycle_record(&state.project_dir, &machine_id)
                .finish_intent
                .is_some()
            {
                return err_text(
                    "A finish() plan is queued for this machine. Wait for it to complete before \
                     executing more code, or supersede it with a new finish() plan.",
                );
            }
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let cleanup = inst.cleanup;

            let Some(conn) = inst.kernel_connections.get(&params.kernel_id) else {
                return err_text(format!(
                    "Kernel {} exists but its connection was lost. Use restart_kernel().",
                    params.kernel_id
                ));
            };

            // Check if kernel is busy.
            if conn.has_pending_work() && !queue {
                return err_text(
                    "Kernel is busy. Use queue=true to wait, or interrupt() to cancel the current execution.",
                );
            }

            let jupyter_session_id = inst.jupyter_session_id.clone();
            let kernel_id = params.kernel_id.clone();
            let conn = inst.kernel_connections.get(&kernel_id).expect("checked");

            let started = conn
                .start_execution(&jupyter_session_id, &params.code)
                .await
                .map_err(|e| McpError::internal_error(format!("Execution failed: {e}"), None))?;
            // Persist the execute-request id before returning so recorder
            // catch-up can target exactly this placeholder after a restart.
            let cell_number = if let Some(nb) = inst.notebooks.get_mut(&params.kernel_id) {
                match nb.append_cell_placeholder(&params.code, &started.parent_msg_id) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        tracing::warn!(target: super::LOG_TARGET, "Failed to create notebook cell: {e}");
                        None
                    }
                }
            } else {
                None
            };
            let rx = started.result_rx;

            // Fire-and-forget: store receiver and return immediately.
            if background {
                if let Some(cell_num) = cell_number {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), rx);
                }

                let mut msg = String::from("Execution started in the background.");
                if let Some(cell_num) = cell_number {
                    let _ = write!(
                        msg,
                        "\nCell number: {cell_num}\nUse get_output(kernel_id=\"{kernel_id}\", cell_number={cell_num}) to check on it."
                    );
                } else {
                    // Without a cell number there is no key to park the
                    // receiver under — say so instead of silently dropping
                    // the only handle to the result.
                    msg.push_str(
                        "\nWARNING: its notebook cell could not be created, so the result cannot be collected with get_output(); the kernel still runs it, and the machine-side recorder (if active) captures the output.",
                    );
                }
                return Ok(CallToolResult::success(vec![ContentBlock::text(msg)]));
            }

            (rx, cell_number, kernel_id, machine_id, cleanup)
        };
        drop(mutation_guard);
        // State lock dropped here — we can await freely.

        if wait_uncapped {
            return self
                .wait_held(
                    HeldExecution {
                        result_rx,
                        machine_id,
                        kernel_id,
                        cell_number,
                        cleanup,
                    },
                    None,
                    progress,
                )
                .await;
        }

        // Wait for result with timeout. Using select! so we can store the receiver on timeout.
        let timed_out;
        let mut completed_output = None;

        let timeout = std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs));
        tokio::select! {
            result = &mut result_rx => {
                timed_out = false;
                completed_output = result.ok();
            }
            () = tokio::time::sleep(timeout) => {
                timed_out = true;
            }
        }

        if timed_out {
            // Store receiver for get_output().
            if let Some(cell_num) = cell_number {
                let mut state = self.state.lock().await;
                if let Some(inst) = state.instances.get_mut(&machine_id) {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), result_rx);
                }
            }

            let mut msg =
                format!("Execution timed out after {timeout_secs}s. The code is still running.");
            if let Some(cell_num) = cell_number {
                let _ = write!(
                    msg,
                    "\nCell number: {cell_num}\nUse get_output(kernel_id=\"{kernel_id}\", cell_number={cell_num}) to check on it."
                );
            }
            return Ok(CallToolResult::success(vec![ContentBlock::text(msg)]));
        }

        let Some(output) = completed_output else {
            return Err(McpError::internal_error(
                "Kernel connection dropped before execution completed",
                None,
            ));
        };

        // Update notebook with final output.
        if let Some(cell_num) = cell_number {
            self.update_notebook_cell(&kernel_id, cell_num, &output)
                .await;
        }

        let is_error = output.error.is_some();
        let (body, images) = render_output(output, ImageCollector::default()).await;
        self.finish_execution_reply(body, images, cleanup == Cleanup::Disabled, is_error)
            .await
    }

    pub(super) async fn wait_inner(
        &self,
        params: WaitParams,
        progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        // 0 means "no cap", same as omitting the timeout (and as execute()).
        let timeout = params.timeout.filter(|&secs| secs != 0);
        let Some(kernel_id) = params.kernel_id else {
            return self.wait_all(timeout, progress).await;
        };
        let held = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(format!("{message}; the execution continues on the machine"));
            }
            let Some(cell_number) = inst
                .pending_executions
                .keys()
                .filter(|(pending_kernel, _)| pending_kernel == &kernel_id)
                .map(|(_, cell_number)| *cell_number)
                .min()
            else {
                return err_text(format!(
                    "No available pending execution found for kernel {kernel_id}."
                ));
            };
            let result_rx = inst
                .pending_executions
                .remove(&(kernel_id.clone(), cell_number))
                .expect("selected pending execution");
            HeldExecution {
                result_rx,
                machine_id,
                kernel_id,
                cell_number: Some(cell_number),
                cleanup: inst.cleanup,
            }
        };
        self.wait_held(held, timeout, progress).await
    }

    /// Wait for every execution that was pending when the call was made,
    /// concurrently — one slow cell doesn't delay collecting the others, and
    /// outputs are reported in completion order. Executions queued after the
    /// call starts need another `wait()`. The deadline (when given) caps the
    /// whole batch, not each execution.
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    async fn wait_all(
        &self,
        timeout_secs: Option<u64>,
        progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        let mut sections: Vec<String> = Vec::new();
        // One image cap for the whole batch: it is one reply.
        let mut images = ImageCollector::default();
        let mut any_error = false;
        // Executions on already-fenced machines stay in place, exactly like
        // the single-kernel wait, which refuses before consuming a receiver.
        let mut held: Vec<HeldExecution> =
            {
                let mut state = self.state.lock().await;
                for (_, inst) in state.instances.iter().filter(|(_, inst)| {
                    inst.fenced.is_some() && !inst.pending_executions.is_empty()
                }) {
                    let message = Self::fenced_message(inst).expect("filtered on fenced");
                    any_error = true;
                    sections.push(format!(
                        "{message}; its pending executions were left in place"
                    ));
                }
                let mut keys: Vec<(String, String, u32)> = state
                    .instances
                    .iter()
                    .filter(|(_, inst)| inst.fenced.is_none())
                    .flat_map(|(machine_id, inst)| {
                        inst.pending_executions
                            .keys()
                            .map(|(kernel_id, cell)| (machine_id.clone(), kernel_id.clone(), *cell))
                    })
                    .collect();
                keys.sort();
                let mut held = Vec::new();
                for (machine_id, kernel_id, cell_number) in keys {
                    let inst = state.instances.get_mut(&machine_id).expect("listed");
                    let result_rx = inst
                        .pending_executions
                        .remove(&(kernel_id.clone(), cell_number))
                        .expect("listed pending execution");
                    held.push(HeldExecution {
                        result_rx,
                        machine_id,
                        kernel_id,
                        cell_number: Some(cell_number),
                        cleanup: inst.cleanup,
                    });
                }
                held
            };
        if held.is_empty() && sections.is_empty() {
            return err_text("No pending executions found on any kernel.");
        }

        let total = held.len();
        let disabled_cleanup = held.iter().any(|entry| entry.cleanup == Cleanup::Disabled);
        let started = std::time::Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut next_progress = std::time::Duration::from_mins(1);

        let batch_label = |entry: &HeldExecution| {
            format!(
                "kernel {} cell {}",
                entry.kernel_id,
                entry.cell_number.expect("held from pending map")
            )
        };
        while !held.is_empty() {
            // Collect everything that has finished since the last tick.
            let mut index = 0;
            while index < held.len() {
                match held[index].result_rx.try_recv() {
                    Ok(output) => {
                        let done = held.swap_remove(index);
                        let label = batch_label(&done);
                        match self.park_if_fenced(&done, output).await {
                            Ok(output) => {
                                if let Some(cell_number) = done.cell_number {
                                    self.update_notebook_cell(
                                        &done.kernel_id,
                                        cell_number,
                                        &output,
                                    )
                                    .await;
                                }
                                any_error |= output.error.is_some();
                                let (text, collected) = render_output(output, images).await;
                                images = collected;
                                sections.push(format!("[{label}]\n{text}"));
                            }
                            Err(message) => {
                                any_error = true;
                                sections.push(format!(
                                    "[{label}] {message}; the execution continues on the machine"
                                ));
                            }
                        }
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => index += 1,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        let done = held.swap_remove(index);
                        any_error = true;
                        // A takeover tears down connections; prefer its
                        // guidance over a bare connection-lost line.
                        let fence = {
                            let state = self.state.lock().await;
                            state
                                .instances
                                .get(&done.machine_id)
                                .and_then(Self::fenced_message)
                        };
                        sections.push(match fence {
                            Some(message) => format!(
                                "[{}] {message}; the execution continues on the machine",
                                batch_label(&done)
                            ),
                            None => {
                                format!("[{}] Kernel connection was lost.", batch_label(&done))
                            }
                        });
                    }
                }
            }

            // Stop waiting on machines this session no longer controls; their
            // executions continue remotely (like the single-kernel wait, the
            // receiver is dropped — attach recovery collects the output).
            {
                let state = self.state.lock().await;
                let mut index = 0;
                while index < held.len() {
                    let message = state
                        .instances
                        .get(&held[index].machine_id)
                        .and_then(Self::fenced_message);
                    if let Some(message) = message {
                        let dropped = held.swap_remove(index);
                        any_error = true;
                        sections.push(format!(
                            "[{}] {message}; the execution continues on the machine",
                            batch_label(&dropped)
                        ));
                    } else {
                        index += 1;
                    }
                }
            }
            if held.is_empty() {
                break;
            }

            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                let mut reinserted = 0usize;
                let mut orphaned = 0usize;
                {
                    let mut state = self.state.lock().await;
                    for entry in held.drain(..) {
                        if let Some(inst) = state.instances.get_mut(&entry.machine_id)
                            && let Some(cell_number) = entry.cell_number
                        {
                            inst.pending_executions
                                .insert((entry.kernel_id, cell_number), entry.result_rx);
                            reinserted += 1;
                        } else {
                            orphaned += 1;
                        }
                    }
                }
                if reinserted > 0 {
                    sections.push(format!(
                        "Timed out after {}s with {reinserted} execution(s) still pending. Use wait() again to keep waiting.",
                        timeout_secs.expect("deadline requires a timeout")
                    ));
                }
                if orphaned > 0 {
                    sections.push(format!(
                        "{orphaned} execution(s) belonged to machines that are no longer active; their results are not collectable here."
                    ));
                }
                break;
            }

            if let Some((meta, peer)) = &progress
                && started.elapsed() >= next_progress
            {
                next_progress += std::time::Duration::from_mins(1);
                if let Some(progress_token) = meta.get_progress_token() {
                    let elapsed = started.elapsed().as_secs();
                    let _ = peer
                        .notify_progress(
                            ProgressNotificationParam::new(progress_token, elapsed as f64)
                                .with_message(format!(
                                    "wait: elapsed={elapsed}s collected={}/{total}",
                                    total - held.len()
                                )),
                        )
                        .await;
                }
            }
            tick.tick().await;
        }

        self.finish_execution_reply(sections.join("\n\n"), images, disabled_cleanup, any_error)
            .await
    }

    /// Shared footer for execution results: spend/budget line, cleanup-disabled nudge, and error-vs-success
    /// wrapping. The images follow the text, in marker order.
    async fn finish_execution_reply(
        &self,
        mut body: String,
        images: ImageCollector,
        cleanup_disabled: bool,
        is_error: bool,
    ) -> Result<CallToolResult, McpError> {
        let spend = self.state.lock().await.session_spend();
        if let Some(spend_line) = self.format_spend_line(spend) {
            body.push_str(&spend_line);
        }
        if cleanup_disabled {
            body.push_str(
                "\nNote: automatic cleanup is disabled. Remember to stop/terminate the machine when done.",
            );
        }
        Ok(execution_reply(body, images, is_error))
    }

    /// Post-completion fence gate shared by the wait paths: a fenced machine
    /// keeps the output in `recovered_executions` (for `get_output()` after a
    /// takeover is resolved) and reports the fence instead of the result.
    async fn park_if_fenced(
        &self,
        done: &HeldExecution,
        output: ExecutionOutput,
    ) -> Result<ExecutionOutput, String> {
        let mut state = self.state.lock().await;
        if let Some(inst) = state.instances.get_mut(&done.machine_id)
            && let Some(message) = Self::fenced_message(inst)
        {
            if let Some(cell_number) = done.cell_number {
                inst.recovered_executions
                    .insert((done.kernel_id.clone(), cell_number), output);
            }
            return Err(message);
        }
        Ok(output)
    }

    async fn wait_held(
        &self,
        held: HeldExecution,
        timeout_secs: Option<u64>,
        progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let cleanup = held.cleanup;
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        match self.wait_execution(held, deadline, progress.as_ref()).await {
            WaitOutcome::Completed(output) => {
                let is_error = output.error.is_some();
                let (body, images) = render_output(output, ImageCollector::default()).await;
                self.finish_execution_reply(body, images, cleanup == Cleanup::Disabled, is_error)
                    .await
            }
            WaitOutcome::StillRunning => {
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Execution still running after {}s. Use wait() again or get_output() to collect it.",
                    timeout_secs.expect("StillRunning requires a timeout")
                ))]))
            }
            WaitOutcome::Fenced(message) => {
                err_text(format!("{message}; the execution continues on the machine"))
            }
            WaitOutcome::ConnectionLost => err_text("Kernel connection was lost."),
        }
    }

    /// Hold one pending execution to completion (the engine behind
    /// `wait_held`; `wait_all` polls its batch directly). On `StillRunning`
    /// the receiver has been reinserted into `pending_executions`; on
    /// `Fenced` after completion the output is parked in
    /// `recovered_executions` via `park_if_fenced`.
    #[allow(clippy::cast_precision_loss, clippy::collapsible_if)]
    async fn wait_execution(
        &self,
        mut held: HeldExecution,
        deadline: Option<tokio::time::Instant>,
        progress: Option<&(RequestMetaObject, Peer<RoleServer>)>,
    ) -> WaitOutcome {
        let started = std::time::Instant::now();
        let mut fence_check = tokio::time::interval(std::time::Duration::from_millis(200));
        fence_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut progress_tick = tokio::time::interval(std::time::Duration::from_mins(1));
        progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        progress_tick.tick().await;
        let mut timeout = deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline)));

        let output = loop {
            tokio::select! {
                result = &mut held.result_rx => break result.ok(),
                _ = fence_check.tick() => {
                    let state = self.state.lock().await;
                    if let Some(inst) = state.instances.get(&held.machine_id) {
                        if let Some(message) = Self::fenced_message(inst) {
                            return WaitOutcome::Fenced(message);
                        }
                    }
                }
                _ = progress_tick.tick(), if progress.is_some() => {
                    let Some((meta, peer)) = progress else { unreachable!() };
                    if let Some(progress_token) = meta.get_progress_token() {
                        let execution_state = {
                            let state = self.state.lock().await;
                            state.instances.get(&held.machine_id)
                                .and_then(|inst| inst.kernel_connections.get(&held.kernel_id))
                                .map_or("connection unavailable", |conn| if conn.is_busy() { "running" } else { "queued" })
                        };
                        let elapsed = started.elapsed().as_secs();
                        let _ = peer.notify_progress(
                            ProgressNotificationParam::new(progress_token, elapsed as f64)
                                .with_message(format!("wait: elapsed={elapsed}s state={execution_state}")),
                        ).await;
                    }
                }
                () = async { timeout.as_mut().expect("guarded timeout").as_mut().await }, if timeout.is_some() => {
                    let mut state = self.state.lock().await;
                    if let Some(inst) = state.instances.get_mut(&held.machine_id) {
                        if let Some(message) = Self::fenced_message(inst) {
                            return WaitOutcome::Fenced(message);
                        }
                        if let Some(cell_number) = held.cell_number {
                            inst.pending_executions.insert(
                                (held.kernel_id.clone(), cell_number),
                                held.result_rx,
                            );
                        }
                    }
                    return WaitOutcome::StillRunning;
                }
            }
        };

        let Some(output) = output else {
            let state = self.state.lock().await;
            if let Some(inst) = state.instances.get(&held.machine_id) {
                if let Some(message) = Self::fenced_message(inst) {
                    return WaitOutcome::Fenced(message);
                }
            }
            return WaitOutcome::ConnectionLost;
        };

        match self.park_if_fenced(&held, output).await {
            Err(message) => WaitOutcome::Fenced(message),
            Ok(output) => {
                if let Some(cell_number) = held.cell_number {
                    self.update_notebook_cell(&held.kernel_id, cell_number, &output)
                        .await;
                }
                WaitOutcome::Completed(output)
            }
        }
    }

    pub(super) async fn get_output_inner(
        &self,
        params: GetOutputParams,
        progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let wait = params.wait.unwrap_or(true);
        let timeout_secs = params.timeout.unwrap_or(30);

        let (mut result_rx, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }

            let key = (params.kernel_id.clone(), params.cell_number);
            if let Some(output) = inst.recovered_executions.remove(&key) {
                drop(state);
                let is_error = output.error.is_some();
                let (body, images) = render_output(output, ImageCollector::default()).await;
                return Ok(execution_reply(body, images, is_error));
            }
            let Some(rx) = inst.pending_executions.remove(&key) else {
                return err_text(format!(
                    "No available pending execution found for kernel {} cell {}. It may have completed, or another wait/get_output call may currently hold it.",
                    params.kernel_id, params.cell_number
                ));
            };
            (rx, machine_id, inst.cleanup)
        };

        if wait {
            // Same engine as wait(): fence ticks, progress keep-alive, and
            // receiver preservation on timeout. timeout 0 = no cap.
            let held = HeldExecution {
                result_rx,
                machine_id,
                kernel_id: params.kernel_id,
                cell_number: Some(params.cell_number),
                cleanup,
            };
            let deadline = (timeout_secs != 0).then(|| {
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs))
            });
            match self.wait_execution(held, deadline, progress.as_ref()).await {
                WaitOutcome::Completed(output) => {
                    let is_error = output.error.is_some();
                    // Same footer as execute()/wait(): spend line and
                    // cleanup-disabled reminder apply to results however
                    // they are collected.
                    let (body, images) = render_output(output, ImageCollector::default()).await;
                    self.finish_execution_reply(
                        body,
                        images,
                        cleanup == Cleanup::Disabled,
                        is_error,
                    )
                    .await
                }
                WaitOutcome::StillRunning => {
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Execution still running after {timeout_secs}s. Use get_output() again to check."
                    ))]))
                }
                WaitOutcome::Fenced(message) => {
                    err_text(format!("{message}; the execution continues on the machine"))
                }
                WaitOutcome::ConnectionLost => err_text("Kernel connection was lost."),
            }
        } else {
            // Non-blocking check.
            match result_rx.try_recv() {
                Ok(output) => {
                    self.update_notebook_cell(&params.kernel_id, params.cell_number, &output)
                        .await;
                    let is_error = output.error.is_some();
                    let (body, images) = render_output(output, ImageCollector::default()).await;
                    self.finish_execution_reply(
                        body,
                        images,
                        cleanup == Cleanup::Disabled,
                        is_error,
                    )
                    .await
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Put it back.
                    let mut state = self.state.lock().await;
                    if let Some(inst) = state.instances.get_mut(&machine_id) {
                        inst.pending_executions
                            .insert((params.kernel_id, params.cell_number), result_rx);
                    }
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        "Execution is still running.",
                    )]))
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    err_text("Kernel connection was lost.")
                }
            }
        }
    }

    /// Update a notebook cell with the final execution output.
    pub(super) async fn update_notebook_cell(
        &self,
        kernel_id: &str,
        cell_number: u32,
        output: &crate::jupyter::messages::ExecutionOutput,
    ) {
        let mut state = self.state.lock().await;
        let Some(machine_id) = state.instance_for_kernel(kernel_id).map(String::from) else {
            return;
        };
        if let Some(inst) = state
            .instances
            .get_mut(&machine_id)
            .filter(|instance| instance.fenced.is_none())
            && let Some(nb) = inst.notebooks.get_mut(kernel_id)
            && let Err(e) = nb.update_cell_output(cell_number, output)
        {
            tracing::warn!(target: super::LOG_TARGET, "Failed to update notebook cell: {e}");
        }
    }
}
