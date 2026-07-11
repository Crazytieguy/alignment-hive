use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::messages::{ExecutionOutput, ExecutionStatus, JupyterMessage};

/// A handle to a WebSocket connection to a single kernel.
pub struct KernelConnection {
    /// Send execute requests.
    request_tx: mpsc::Sender<ExecuteCommand>,
    /// Whether the kernel is currently executing code.
    is_busy: Arc<AtomicBool>,
    /// Background reader task handle.
    reader_handle: tokio::task::JoinHandle<()>,
}

struct ExecuteCommand {
    msg: JupyterMessage,
    result_tx: oneshot::Sender<ExecutionOutput>,
}

pub struct StartedExecution {
    pub parent_msg_id: String,
    pub result_rx: oneshot::Receiver<ExecutionOutput>,
}

impl KernelConnection {
    /// Connect to a kernel's WebSocket channels endpoint. `ws_base` is the
    /// machine's Jupyter WebSocket endpoint from the runtime's
    /// [`crate::runtime::JupyterEndpoint`].
    pub async fn connect(ws_base: &str, kernel_id: &str, token: &str) -> anyhow::Result<Self> {
        let url = format!(
            "{}/api/kernels/{kernel_id}/channels?token={token}",
            ws_base.trim_end_matches('/')
        );

        tracing::debug!(%kernel_id, "Connecting to kernel WebSocket");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await?;
        let (ws_sink, ws_stream_rx) = ws_stream.split();

        let (request_tx, request_rx) = mpsc::channel::<ExecuteCommand>(16);
        let is_busy = Arc::new(AtomicBool::new(false));

        let reader_handle = tokio::spawn(Self::run_ws_loop(
            ws_sink,
            ws_stream_rx,
            request_rx,
            Arc::clone(&is_busy),
        ));

        tracing::info!(%kernel_id, "Connected to kernel WebSocket");

        Ok(Self {
            request_tx,
            is_busy,
            reader_handle,
        })
    }

    /// Check if the kernel is currently executing code.
    pub fn is_busy(&self) -> bool {
        self.is_busy.load(Ordering::Relaxed)
    }

    /// Start an execution without waiting for the result.
    /// Returns a receiver that will yield the final `ExecutionOutput` when complete.
    #[allow(clippy::unused_async)] // callers treat submission as async; keep the signature stable
    pub async fn start_execution(
        &self,
        session_id: &str,
        code: &str,
    ) -> anyhow::Result<StartedExecution> {
        let msg = JupyterMessage::execute_request(session_id, code);
        let parent_msg_id = msg.header.msg_id.clone();
        let (result_tx, result_rx) = oneshot::channel();

        // try_send, not send: callers hold the server state lock, and the ws
        // loop only drains this queue between executions — a blocking send
        // on a full queue would freeze every other tool call (including
        // interrupt/stop) until the running cell finishes.
        self.request_tx
            .try_send(ExecuteCommand { msg, result_tx })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => anyhow::anyhow!(
                    "Kernel execution queue is full (16 pending executions) — wait() for \
                     results or interrupt() before queueing more"
                ),
                mpsc::error::TrySendError::Closed(_) => {
                    anyhow::anyhow!("Kernel connection closed")
                }
            })?;

        Ok(StartedExecution {
            parent_msg_id,
            result_rx,
        })
    }

    /// Execute code and wait for the result with a timeout.
    pub async fn execute(
        &self,
        session_id: &str,
        code: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<ExecutionOutput> {
        let result_rx = self.start_execution(session_id, code).await?.result_rx;

        match tokio::time::timeout(timeout, result_rx).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(_)) => anyhow::bail!("Kernel connection dropped before execution completed"),
            Err(_) => Ok(ExecutionOutput {
                stderr: "Execution timed out. The code may still be running on the kernel."
                    .to_string(),
                ..Default::default()
            }),
        }
    }

    /// Background task: handles sending requests and reading responses.
    /// Only accepts new requests when no execution is pending (select guard).
    /// Messages queue in the mpsc channel until the current execution completes.
    #[allow(clippy::too_many_lines)]
    async fn run_ws_loop(
        mut ws_sink: futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        mut ws_stream_rx: futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        mut request_rx: mpsc::Receiver<ExecuteCommand>,
        is_busy: Arc<AtomicBool>,
    ) {
        let mut pending: Option<(
            String,
            ExecutionOutput,
            oneshot::Sender<ExecutionOutput>,
            bool,
            bool,
        )> = None;

        loop {
            tokio::select! {
                // Only accept new requests when no execution is pending.
                // This prevents clobbering a pending result — messages queue in the channel.
                Some(cmd) = request_rx.recv(), if pending.is_none() => {
                    let msg_id = cmd.msg.header.msg_id.clone();
                    let json = match serde_json::to_string(&cmd.msg) {
                        Ok(j) => j,
                        Err(e) => {
                            tracing::error!("Failed to serialize execute_request: {e}");
                            let _ = cmd.result_tx.send(ExecutionOutput::error(format!("Internal error: {e}")));
                            continue;
                        }
                    };

                    if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                        tracing::error!("Failed to send WebSocket message: {e}");
                        let _ = cmd.result_tx.send(ExecutionOutput::error(format!("WebSocket send error: {e}")));
                        continue;
                    }

                    is_busy.store(true, Ordering::Relaxed);
                    pending = Some((
                        msg_id,
                        ExecutionOutput::default(),
                        cmd.result_tx,
                        false,
                        false,
                    ));
                }

                Some(msg_result) = ws_stream_rx.next() => {
                    let msg = match msg_result {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<JupyterMessage>(&text) {
                                Ok(m) => m,
                                Err(e) => {
                                    tracing::debug!("Ignoring unparseable WS message: {e}");
                                    continue;
                                }
                            }
                        }
                        Ok(Message::Close(_)) => {
                            tracing::info!("WebSocket closed");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("WebSocket error: {e}");
                            break;
                        }
                        _ => continue,
                    };

                    let parent_msg_id = msg.parent_header
                        .get("msg_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let mut complete = false;
                    if let Some((ref expected_id, ref mut output, _, ref mut shell_replied, ref mut idle)) = pending {
                        if parent_msg_id != expected_id {
                            continue;
                        }
                        match msg.channel.as_str() {
                            "iopub" => {
                                output.process_iopub(&msg);
                                if msg.header.msg_type == "status"
                                    && msg.content["execution_state"].as_str() == Some("idle")
                                {
                                    *idle = true;
                                }
                            }
                            "shell" if msg.header.msg_type == "execute_reply" => {
                                let status = msg.content["status"].as_str().unwrap_or("ok");
                                if status == "error" && output.status != ExecutionStatus::Errored {
                                    output.status = ExecutionStatus::Errored;
                                } else if output.status == ExecutionStatus::Running {
                                    output.status = ExecutionStatus::Complete;
                                }

                                *shell_replied = true;
                            }
                            _ => {}
                        }
                        complete = *shell_replied && *idle;
                    }
                    if complete {
                        is_busy.store(false, Ordering::Relaxed);
                        if let Some((_, output, tx, _, _)) = pending.take() {
                            let _ = tx.send(output);
                        }
                    }
                }

                else => break,
            }
        }

        // If we exit with a pending execution, complete it with what we have.
        is_busy.store(false, Ordering::Relaxed);
        if let Some((_, mut output, tx, _, _)) = pending.take() {
            if output.status == ExecutionStatus::Running {
                output.status = ExecutionStatus::Errored;
                output
                    .stderr
                    .push_str("\nWebSocket connection closed unexpectedly.");
            }
            let _ = tx.send(output);
        }
    }
}

impl Drop for KernelConnection {
    fn drop(&mut self) {
        self.reader_handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::{ExecuteCommand, KernelConnection};

    #[tokio::test]
    async fn dropping_fenced_connection_aborts_reader_task() {
        let (request_tx, _request_rx) = tokio::sync::mpsc::channel::<ExecuteCommand>(1);
        let reader_handle = tokio::spawn(std::future::pending::<()>());
        let abort_handle = reader_handle.abort_handle();
        let connection = KernelConnection {
            request_tx,
            is_busy: Arc::new(AtomicBool::new(false)),
            reader_handle,
        };
        drop(connection);
        tokio::task::yield_now().await;
        assert!(abort_handle.is_finished());
    }
}
