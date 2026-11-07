//! The engines behind `execute`, `wait` and `get_output`. A timed-out or
//! background execution parks its result receiver in the instance's
//! `pending_executions`; a call that takes it out reinserts it if the
//! execution is still running, so a later call can still collect the result.
//!
//! A call the client cancels is treated like a timeout: rmcp keeps running the
//! handler but drops its reply, so the handler re-parks what it holds, and
//! outputs that completed for that reply go to `recovered_executions` for
//! `get_output()` instead of being consumed. Cancelling never interrupts the
//! kernel.
//!
//! A socket dying during a cell is neither a result nor a failure: the cell
//! carries on, the waiter is told, and the cell comes back to
//! `pending_executions` when the socket does (see `kernel_recovery`).

use std::fmt::Write;

use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam, RequestMetaObject};
use rmcp::{ErrorData as McpError, Peer, RoleServer};
use tokio_util::sync::CancellationToken;

use crate::config::Cleanup;
use crate::images::ImageCollector;
use crate::jupyter::messages::ExecutionOutput;

use super::kernel_recovery::CONNECTION_DOWN;
use super::params::{ExecuteParams, GetOutputParams, WaitParams};
use super::{RemoteKernelsServer, err_text};

/// What the engines need from the MCP request: its progress channel (absent
/// for direct calls) and the client's cancellation.
pub(super) struct CallContext {
    pub(super) progress: Option<(RequestMetaObject, Peer<RoleServer>)>,
    pub(super) cancel: CancellationToken,
}

impl CallContext {
    pub(super) fn mcp(
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            progress: Some((meta, peer)),
            cancel,
        }
    }

    /// A direct (non-MCP) call: no progress, never cancelled.
    pub(super) fn detached() -> Self {
        Self {
            progress: None,
            cancel: CancellationToken::new(),
        }
    }
}

struct HeldExecution {
    result_rx: tokio::sync::oneshot::Receiver<ExecutionOutput>,
    machine_id: String,
    kernel_id: String,
    cell_number: Option<u32>,
    cleanup: Cleanup,
}

impl HeldExecution {
    fn completed(self, output: ExecutionOutput) -> CompletedOutput {
        CompletedOutput {
            machine_id: self.machine_id,
            kernel_id: self.kernel_id,
            cell_number: self.cell_number,
            output,
        }
    }
}

/// A finished execution's output, still owned by the reply being built so a
/// cancelled reply can park it (see `park_completed`).
struct CompletedOutput {
    machine_id: String,
    kernel_id: String,
    cell_number: Option<u32>,
    output: ExecutionOutput,
}

/// Result of holding one pending execution open (see `wait_execution`).
enum WaitOutcome {
    Completed(CompletedOutput),
    StillRunning,
    Cancelled,
    Detached(String),
    ConnectionLost,
}

/// The reply of a cancelled call. rmcp drops it; it exists only to end the
/// handler.
fn cancelled_reply() -> Result<CallToolResult, McpError> {
    err_text("Cancelled by the client; pending results were kept for wait()/get_output().")
}

/// What a waiter hears when the socket dies during its cell.
fn dropped_during_cell(kernel_id: &str, cell_number: Option<u32>) -> String {
    let collect = cell_number.map_or_else(String::new, |cell_number| {
        format!(" Collect its output with get_output(kernel_id=\"{kernel_id}\", cell_number={cell_number}).")
    });
    format!(
        "The connection to the machine dropped before this cell finished. It may still be running. The connection is retried automatically, and status() reports if the machine or kernel is gone.{collect}"
    )
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
        ctx: CallContext,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        self.reconnect_kernel_now(&params.kernel_id).await;

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

        let (mut result_rx, cell_number, kernel_id, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            if crate::state::load_instance_record(&self.project_dir, &machine_id)
                .is_some_and(|record| record.finish_intent.is_some())
            {
                return err_text(
                    "A finish() plan is queued for this machine, so it takes no new code. status() \
                     shows its progress. To keep working instead, call finish(then=\"keep\"), which \
                     replaces the plan; the machine takes new code again once running work finishes.",
                );
            }
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::detached_message(inst) {
                return err_text(message);
            }
            if inst.ending {
                return err_text(format!(
                    "Machine {machine_id} is being stopped or terminated, so it takes no new executions."
                ));
            }
            let cleanup = inst.cleanup;

            let Some(conn) = inst
                .kernel_connections
                .get(&params.kernel_id)
                .filter(|conn| !conn.is_closed())
            else {
                return err_text(CONNECTION_DOWN);
            };

            // Check if kernel is busy, with a cell the socket knows or one it doesn't.
            let running_out_of_sight = inst
                .running_cells
                .keys()
                .any(|(id, _)| *id == params.kernel_id);
            if (conn.has_pending_work() || running_out_of_sight) && !queue {
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
                        "\nWARNING: its notebook cell could not be created, so get_output() can't collect the result. The kernel still runs it; its output may reach the notebook after a reconnect, so re-run the cell if you need the result.",
                    );
                }
                return Ok(CallToolResult::success(vec![ContentBlock::text(msg)]));
            }

            (rx, cell_number, kernel_id, machine_id, cleanup)
        };
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
                    ctx,
                )
                .await;
        }

        // Wait for result with timeout. Using select! so we can store the receiver on timeout.
        // `Err(cancelled)` when the call stops waiting.
        let timeout = std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs));
        let waited = tokio::select! {
            result = &mut result_rx => Ok(result.ok()),
            () = tokio::time::sleep(timeout) => Err(false),
            () = ctx.cancel.cancelled() => Err(true),
        };

        if let Err(cancelled) = waited {
            // Store receiver for get_output().
            if let Some(cell_num) = cell_number {
                let mut state = self.state.lock().await;
                if let Some(inst) = state.instances.get_mut(&machine_id) {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), result_rx);
                }
            }
            if cancelled {
                return cancelled_reply();
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

        let Ok(Some(output)) = waited else {
            return Ok(self
                .lost_socket_reply(&machine_id, &kernel_id, cell_number)
                .await);
        };

        // Update notebook with final output.
        if let Some(cell_num) = cell_number {
            self.update_notebook_cell(&kernel_id, cell_num, &output)
                .await;
        }

        self.single_output_reply(
            &ctx,
            CompletedOutput {
                machine_id,
                kernel_id,
                cell_number,
                output,
            },
            Some(cleanup),
        )
        .await
    }

    pub(super) async fn wait_inner(
        &self,
        params: WaitParams,
        ctx: CallContext,
    ) -> Result<CallToolResult, McpError> {
        // 0 means "no cap", same as omitting the timeout (and as execute()).
        let timeout = params.timeout.filter(|&secs| secs != 0);
        let Some(kernel_id) = params.kernel_id else {
            return self.wait_all(timeout, ctx).await;
        };
        self.reconnect_kernel_now(&kernel_id).await;
        let held = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::detached_message(inst) {
                return err_text(format!("{message}; the execution continues on the machine"));
            }
            let Some(cell_number) = inst
                .pending_executions
                .keys()
                .filter(|(pending_kernel, _)| pending_kernel == &kernel_id)
                .map(|(_, cell_number)| *cell_number)
                .min()
            else {
                if inst.closed_kernels().contains(&kernel_id) {
                    return err_text(CONNECTION_DOWN);
                }
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
        self.wait_held(held, timeout, ctx).await
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
        ctx: CallContext,
    ) -> Result<CallToolResult, McpError> {
        /// One part of the batch reply. Outputs stay raw until the reply's
        /// one cancellation check, so a cancelled reply parks every one of
        /// them.
        enum Section {
            Text(String),
            Output {
                label: String,
                done: CompletedOutput,
            },
        }

        self.reconnect_all_now().await;
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        let mut sections: Vec<Section> = Vec::new();
        let mut any_error = false;
        // Executions on already-detached machines stay in place, exactly like
        // the single-kernel wait, which refuses before consuming a receiver.
        let mut held: Vec<HeldExecution> = {
            let mut state = self.state.lock().await;
            for (_, inst) in state
                .instances
                .iter()
                .filter(|(_, inst)| inst.detached && !inst.pending_executions.is_empty())
            {
                let message = Self::detached_message(inst).expect("filtered on detached");
                any_error = true;
                sections.push(Section::Text(format!(
                    "{message}; its pending executions were left in place"
                )));
            }
            let mut keys: Vec<(String, String, u32)> = state
                .instances
                .iter()
                .filter(|(_, inst)| !inst.detached)
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
            let down = self
                .state
                .lock()
                .await
                .instances
                .values()
                .any(|instance| !instance.detached && !instance.closed_kernels().is_empty());
            if down {
                return err_text(CONNECTION_DOWN);
            }
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
                        match self.park_if_detached(&done, output).await {
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
                                sections.push(Section::Output {
                                    label,
                                    done: done.completed(output),
                                });
                            }
                            Err(message) => {
                                any_error = true;
                                sections.push(Section::Text(format!(
                                    "[{label}] {message}; the execution continues on the machine"
                                )));
                            }
                        }
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => index += 1,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        let done = held.swap_remove(index);
                        // A takeover tears down connections; prefer its
                        // guidance over a bare connection-lost line.
                        let detached = {
                            let state = self.state.lock().await;
                            state
                                .instances
                                .get(&done.machine_id)
                                .and_then(Self::detached_message)
                        };
                        sections.push(Section::Text(match detached {
                            Some(message) => {
                                any_error = true;
                                format!(
                                    "[{}] {message}; the execution continues on the machine",
                                    batch_label(&done)
                                )
                            }
                            None => format!(
                                "[{}] {}",
                                batch_label(&done),
                                self.lost_socket_text(
                                    &done.machine_id,
                                    &done.kernel_id,
                                    done.cell_number
                                )
                                .await
                            ),
                        }));
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
                        .and_then(Self::detached_message);
                    if let Some(message) = message {
                        let dropped = held.swap_remove(index);
                        any_error = true;
                        sections.push(Section::Text(format!(
                            "[{}] {message}; the execution continues on the machine",
                            batch_label(&dropped)
                        )));
                    } else {
                        index += 1;
                    }
                }
            }
            if held.is_empty() {
                break;
            }

            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                let (reinserted, orphaned) = self.repark_all(std::mem::take(&mut held)).await;
                if reinserted > 0 {
                    sections.push(Section::Text(format!(
                        "Timed out after {}s with {reinserted} execution(s) still pending. Use wait() again to keep waiting.",
                        timeout_secs.expect("deadline requires a timeout")
                    )));
                }
                if orphaned > 0 {
                    sections.push(Section::Text(format!(
                        "{orphaned} execution(s) belonged to machines that are no longer active; their results are not collectable here."
                    )));
                }
                break;
            }

            if let Some((meta, peer)) = &ctx.progress
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
            tokio::select! {
                _ = tick.tick() => {}
                () = ctx.cancel.cancelled() => {
                    self.repark_all(std::mem::take(&mut held)).await;
                    break;
                }
            }
        }

        if ctx.cancel.is_cancelled() {
            self.park_completed(sections.into_iter().filter_map(|section| match section {
                Section::Output { done, .. } => Some(done),
                Section::Text(_) => None,
            }))
            .await;
            return cancelled_reply();
        }
        // One image cap for the whole batch: it is one reply.
        let mut images = ImageCollector::default();
        let mut texts = Vec::with_capacity(sections.len());
        for section in sections {
            match section {
                Section::Text(text) => texts.push(text),
                Section::Output { label, done } => {
                    let (text, collected) = render_output(done.output, images).await;
                    images = collected;
                    texts.push(format!("[{label}]\n{text}"));
                }
            }
        }
        self.finish_execution_reply(texts.join("\n\n"), images, disabled_cleanup, any_error)
            .await
    }

    /// Put still-running executions back for a later call, as a timeout or
    /// a cancelled call leaves them. Returns (reinserted, orphaned): an
    /// execution whose machine is gone, or that has no cell number, can't be
    /// put back.
    async fn repark_all(&self, held: Vec<HeldExecution>) -> (usize, usize) {
        let mut reinserted = 0usize;
        let mut orphaned = 0usize;
        let mut state = self.state.lock().await;
        for entry in held {
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
        (reinserted, orphaned)
    }

    /// Keep a cancelled reply's outputs for a later `get_output()`: rmcp drops
    /// the reply, so consuming them would lose them. Callers check the token
    /// once per reply, just before rendering; a cancel landing after that
    /// check can't be caught from here.
    async fn park_completed(&self, completed: impl IntoIterator<Item = CompletedOutput>) {
        let mut state = self.state.lock().await;
        for done in completed {
            if let Some(inst) = state.instances.get_mut(&done.machine_id)
                && let Some(cell_number) = done.cell_number
            {
                inst.recovered_executions
                    .insert((done.kernel_id, cell_number), done.output);
            }
        }
    }

    /// Reply with one completed output, or park it if the call was
    /// cancelled. `cleanup` adds the shared spend/cleanup footer; `None` (an
    /// output already recovered) replies without it.
    async fn single_output_reply(
        &self,
        ctx: &CallContext,
        done: CompletedOutput,
        cleanup: Option<Cleanup>,
    ) -> Result<CallToolResult, McpError> {
        if ctx.cancel.is_cancelled() {
            self.park_completed([done]).await;
            return cancelled_reply();
        }
        let is_error = done.output.error.is_some();
        let (body, images) = render_output(done.output, ImageCollector::default()).await;
        match cleanup {
            Some(cleanup) => {
                self.finish_execution_reply(body, images, cleanup == Cleanup::Disabled, is_error)
                    .await
            }
            None => Ok(execution_reply(body, images, is_error)),
        }
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
        if let Some(spend_line) = self.budget_line(&spend).map(|line| format!("\n[{line}]")) {
            body.push_str(&spend_line);
        }
        if cleanup_disabled {
            body.push_str(
                "\nNote: automatic cleanup is disabled. Remember to stop/terminate the machine when done.",
            );
        }
        Ok(execution_reply(body, images, is_error))
    }

    /// Post-completion detach gate shared by the wait paths: a detached machine
    /// keeps the output in `recovered_executions` (for `get_output()` after a
    /// takeover is resolved) and reports the detach instead of the result.
    async fn park_if_detached(
        &self,
        done: &HeldExecution,
        output: ExecutionOutput,
    ) -> Result<ExecutionOutput, String> {
        let mut state = self.state.lock().await;
        if let Some(inst) = state.instances.get_mut(&done.machine_id)
            && let Some(message) = Self::detached_message(inst)
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
        ctx: CallContext,
    ) -> Result<CallToolResult, McpError> {
        let cleanup = held.cleanup;
        let machine_id = held.machine_id.clone();
        let kernel_id = held.kernel_id.clone();
        let cell_number = held.cell_number;
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        match self.wait_execution(held, deadline, &ctx).await {
            WaitOutcome::Completed(done) => {
                self.single_output_reply(&ctx, done, Some(cleanup)).await
            }
            WaitOutcome::StillRunning => {
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Execution still running after {}s. Use wait() again or get_output() to collect it.",
                    timeout_secs.expect("StillRunning requires a timeout")
                ))]))
            }
            WaitOutcome::Cancelled => cancelled_reply(),
            WaitOutcome::Detached(message) => {
                err_text(format!("{message}; the execution continues on the machine"))
            }
            WaitOutcome::ConnectionLost => Ok(self
                .lost_socket_reply(&machine_id, &kernel_id, cell_number)
                .await),
        }
    }

    /// Why a waiter's socket went away: the socket alone, or the whole machine
    /// (which takes its sockets with it, and its cell).
    async fn lost_socket_text(
        &self,
        machine_id: &str,
        kernel_id: &str,
        cell_number: Option<u32>,
    ) -> String {
        if self.state.lock().await.instances.contains_key(machine_id) {
            dropped_during_cell(kernel_id, cell_number)
        } else {
            format!(
                "Machine {machine_id} stopped or was deleted while this cell was running. Call status()."
            )
        }
    }

    async fn lost_socket_reply(
        &self,
        machine_id: &str,
        kernel_id: &str,
        cell_number: Option<u32>,
    ) -> CallToolResult {
        CallToolResult::success(vec![ContentBlock::text(
            self.lost_socket_text(machine_id, kernel_id, cell_number)
                .await,
        )])
    }

    /// Put one still-running execution back for a later call (a timeout or
    /// a cancelled call). On a detached machine the receiver is dropped instead
    /// and the detach message returned; attach recovery collects the output.
    async fn repark_one(&self, held: HeldExecution) -> Option<String> {
        let mut state = self.state.lock().await;
        let inst = state.instances.get_mut(&held.machine_id)?;
        if let Some(message) = Self::detached_message(inst) {
            return Some(message);
        }
        if let Some(cell_number) = held.cell_number {
            inst.pending_executions
                .insert((held.kernel_id, cell_number), held.result_rx);
        }
        None
    }

    /// Hold one pending execution to completion (the engine behind
    /// `wait_held`; `wait_all` polls its batch directly). On `StillRunning`
    /// and `Cancelled` the receiver has been reinserted into
    /// `pending_executions`; on `Detached` after completion the output is
    /// parked in `recovered_executions` via `park_if_detached`.
    #[allow(clippy::cast_precision_loss, clippy::collapsible_if)]
    async fn wait_execution(
        &self,
        mut held: HeldExecution,
        deadline: Option<tokio::time::Instant>,
        ctx: &CallContext,
    ) -> WaitOutcome {
        let progress = ctx.progress.as_ref();
        let started = std::time::Instant::now();
        let mut detach_check = tokio::time::interval(std::time::Duration::from_millis(200));
        detach_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut progress_tick = tokio::time::interval(std::time::Duration::from_mins(1));
        progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        progress_tick.tick().await;
        let mut timeout = deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline)));

        let output = loop {
            tokio::select! {
                result = &mut held.result_rx => break result.ok(),
                _ = detach_check.tick() => {
                    let state = self.state.lock().await;
                    if let Some(inst) = state.instances.get(&held.machine_id) {
                        if let Some(message) = Self::detached_message(inst) {
                            return WaitOutcome::Detached(message);
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
                    return match self.repark_one(held).await {
                        Some(message) => WaitOutcome::Detached(message),
                        None => WaitOutcome::StillRunning,
                    };
                }
                () = ctx.cancel.cancelled() => {
                    return match self.repark_one(held).await {
                        Some(message) => WaitOutcome::Detached(message),
                        None => WaitOutcome::Cancelled,
                    };
                }
            }
        };

        let Some(output) = output else {
            let state = self.state.lock().await;
            if let Some(inst) = state.instances.get(&held.machine_id) {
                if let Some(message) = Self::detached_message(inst) {
                    return WaitOutcome::Detached(message);
                }
            }
            return WaitOutcome::ConnectionLost;
        };

        match self.park_if_detached(&held, output).await {
            Err(message) => WaitOutcome::Detached(message),
            Ok(output) => {
                if let Some(cell_number) = held.cell_number {
                    self.update_notebook_cell(&held.kernel_id, cell_number, &output)
                        .await;
                }
                WaitOutcome::Completed(held.completed(output))
            }
        }
    }

    pub(super) async fn get_output_inner(
        &self,
        params: GetOutputParams,
        ctx: CallContext,
    ) -> Result<CallToolResult, McpError> {
        let wait = params.wait.unwrap_or(true);
        let timeout_secs = params.timeout.unwrap_or(30);
        self.reconnect_kernel_now(&params.kernel_id).await;

        let (mut result_rx, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::detached_message(inst) {
                return err_text(message);
            }

            let key = (params.kernel_id.clone(), params.cell_number);
            if let Some(output) = inst.recovered_executions.remove(&key) {
                drop(state);
                let done = CompletedOutput {
                    machine_id,
                    kernel_id: params.kernel_id,
                    cell_number: Some(params.cell_number),
                    output,
                };
                return self.single_output_reply(&ctx, done, None).await;
            }
            let Some(rx) = inst.pending_executions.remove(&key) else {
                if inst.closed_kernels().contains(&params.kernel_id) {
                    return err_text(CONNECTION_DOWN);
                }
                return err_text(format!(
                    "No available pending execution found for kernel {} cell {}. It may have completed, or another wait/get_output call may currently hold it.",
                    params.kernel_id, params.cell_number
                ));
            };
            (rx, machine_id, inst.cleanup)
        };

        if wait {
            // Same engine as wait(): detach ticks, progress keep-alive, and
            // receiver preservation on timeout. timeout 0 = no cap.
            let held = HeldExecution {
                result_rx,
                machine_id: machine_id.clone(),
                kernel_id: params.kernel_id.clone(),
                cell_number: Some(params.cell_number),
                cleanup,
            };
            let deadline = (timeout_secs != 0).then(|| {
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs))
            });
            match self.wait_execution(held, deadline, &ctx).await {
                // Same footer as execute()/wait(): spend line and
                // cleanup-disabled reminder apply to results however they
                // are collected.
                WaitOutcome::Completed(done) => {
                    self.single_output_reply(&ctx, done, Some(cleanup)).await
                }
                WaitOutcome::StillRunning => {
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Execution still running after {timeout_secs}s. Use get_output() again to check."
                    ))]))
                }
                WaitOutcome::Cancelled => cancelled_reply(),
                WaitOutcome::Detached(message) => {
                    err_text(format!("{message}; the execution continues on the machine"))
                }
                WaitOutcome::ConnectionLost => Ok(self
                    .lost_socket_reply(&machine_id, &params.kernel_id, Some(params.cell_number))
                    .await),
            }
        } else {
            // Non-blocking check.
            match result_rx.try_recv() {
                Ok(output) => {
                    self.update_notebook_cell(&params.kernel_id, params.cell_number, &output)
                        .await;
                    let done = CompletedOutput {
                        machine_id,
                        kernel_id: params.kernel_id,
                        cell_number: Some(params.cell_number),
                        output,
                    };
                    self.single_output_reply(&ctx, done, Some(cleanup)).await
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
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Ok(self
                    .lost_socket_reply(&machine_id, &params.kernel_id, Some(params.cell_number))
                    .await),
            }
        }
    }

    /// Finished cells nobody collected go into the notebook now: the machine is
    /// about to end, and with it any later way of asking for them. Cells still
    /// running stay pending.
    pub(super) async fn save_uncollected_outputs(&self, machine_id: &str) {
        let pending = {
            let mut state = self.state.lock().await;
            let Some(instance) = state.instances.get_mut(machine_id) else {
                return;
            };
            std::mem::take(&mut instance.pending_executions)
        };
        for ((kernel_id, cell_number), mut result_rx) in pending {
            if let Ok(output) = result_rx.try_recv() {
                self.update_notebook_cell(&kernel_id, cell_number, &output)
                    .await;
                continue;
            }
            let mut state = self.state.lock().await;
            if let Some(instance) = state.instances.get_mut(machine_id) {
                instance
                    .pending_executions
                    .insert((kernel_id, cell_number), result_rx);
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
            .filter(|instance| !instance.detached)
            && let Some(nb) = inst.notebooks.get_mut(kernel_id)
            && let Err(e) = nb.update_cell_output(cell_number, output)
        {
            tracing::warn!(target: super::LOG_TARGET, "Failed to update notebook cell: {e}");
        }
    }
}
