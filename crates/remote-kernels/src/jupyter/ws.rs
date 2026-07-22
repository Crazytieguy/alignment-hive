use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::messages::{ExecutionOutput, ExecutionStatus, JupyterMessage};

/// A handle to a WebSocket connection to a single kernel.
pub struct KernelConnection {
    /// Send execute requests.
    request_tx: mpsc::Sender<ExecuteCommand>,
    /// Whether the kernel is currently executing code.
    is_busy: Arc<AtomicBool>,
    /// Executions accepted by this connection but not yet resolved.
    in_flight: Arc<AtomicUsize>,
    /// Background reader task handle.
    reader_handle: tokio::task::JoinHandle<()>,
}

struct ExecuteCommand {
    msg: JupyterMessage,
    result_tx: oneshot::Sender<ExecutionOutput>,
    permit: InFlightPermit,
}

struct InFlightPermit {
    in_flight: Arc<AtomicUsize>,
}

impl InFlightPermit {
    fn new(in_flight: Arc<AtomicUsize>) -> Self {
        in_flight.fetch_add(1, Ordering::Relaxed);
        Self { in_flight }
    }
}

impl Drop for InFlightPermit {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

struct PendingExecution {
    msg_id: String,
    output: ExecutionOutput,
    result_tx: oneshot::Sender<ExecutionOutput>,
    shell_replied: bool,
    idle: bool,
    _permit: InFlightPermit,
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
        let in_flight = Arc::new(AtomicUsize::new(0));

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
            in_flight,
            reader_handle,
        })
    }

    /// Check if the kernel is currently executing code.
    pub fn is_busy(&self) -> bool {
        self.is_busy.load(Ordering::Relaxed)
    }

    /// Check if an execution is queued or running on this connection.
    pub fn has_pending_work(&self) -> bool {
        self.in_flight.load(Ordering::Relaxed) > 0
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
        let permit = InFlightPermit::new(Arc::clone(&self.in_flight));

        // try_send, not send: callers hold the server state lock, and the ws
        // loop only drains this queue between executions — a blocking send
        // on a full queue would freeze every other tool call (including
        // interrupt/stop) until the running cell finishes.
        self.request_tx
            .try_send(ExecuteCommand {
                msg,
                result_tx,
                permit,
            })
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
    async fn run_ws_loop<S, R>(
        mut ws_sink: S,
        mut ws_stream_rx: R,
        mut request_rx: mpsc::Receiver<ExecuteCommand>,
        is_busy: Arc<AtomicBool>,
    ) where
        S: Sink<Message> + Unpin,
        S::Error: std::fmt::Display,
        R: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let mut pending: Option<PendingExecution> = None;
        // Grace deadline armed when execute_reply has arrived but the iopub
        // idle has not: idle normally lands milliseconds later, but it can be
        // lost server-side (iopub HWM overflow under output floods, or a
        // kernel crash in the reply→idle gap) — without a bound the kernel
        // wedges busy forever and the queue jams.
        let mut idle_grace: Option<tokio::time::Instant> = None;

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
                    pending = Some(PendingExecution {
                        msg_id,
                        output: ExecutionOutput::default(),
                        result_tx: cmd.result_tx,
                        shell_replied: false,
                        idle: false,
                        _permit: cmd.permit,
                    });
                    idle_grace = None;
                }

                () = async {
                    match idle_grace {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if idle_grace.is_some() => {
                    tracing::warn!(
                        "iopub idle never arrived after execute_reply; completing with the output received so far"
                    );
                    idle_grace = None;
                    is_busy.store(false, Ordering::Relaxed);
                    if let Some(pending) = pending.take() {
                        let _ = pending.result_tx.send(pending.output);
                    }
                }

                msg_result = ws_stream_rx.next() => {
                    let Some(msg_result) = msg_result else {
                        tracing::info!("WebSocket stream ended");
                        break;
                    };
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
                    if let Some(ref mut pending) = pending {
                        if parent_msg_id != pending.msg_id {
                            continue;
                        }
                        match msg.channel.as_str() {
                            "iopub" => {
                                pending.output.process_iopub(&msg);
                                if msg.header.msg_type == "status"
                                    && msg.content["execution_state"].as_str() == Some("idle")
                                {
                                    pending.idle = true;
                                }
                            }
                            "shell" if msg.header.msg_type == "execute_reply" => {
                                let status = msg.content["status"].as_str().unwrap_or("ok");
                                if status == "error"
                                    && pending.output.status != ExecutionStatus::Errored
                                {
                                    pending.output.status = ExecutionStatus::Errored;
                                } else if pending.output.status == ExecutionStatus::Running {
                                    pending.output.status = ExecutionStatus::Complete;
                                }

                                pending.shell_replied = true;
                            }
                            _ => {}
                        }
                        complete = pending.shell_replied && pending.idle;
                        idle_grace = if pending.shell_replied && !pending.idle {
                            // Trailing iopub output is still expected — give
                            // it a bounded window rather than forever.
                            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
                        } else {
                            None
                        };
                    }
                    if complete {
                        is_busy.store(false, Ordering::Relaxed);
                        if let Some(pending) = pending.take() {
                            let _ = pending.result_tx.send(pending.output);
                        }
                    }
                }

                else => break,
            }
        }

        request_rx.close();

        // If we exit with a pending execution, complete it with what we have.
        is_busy.store(false, Ordering::Relaxed);
        if let Some(mut pending) = pending.take() {
            if pending.output.status == ExecutionStatus::Running {
                pending.output.status = ExecutionStatus::Errored;
                pending
                    .output
                    .stderr
                    .push_str("\nWebSocket connection closed unexpectedly.");
            }
            let _ = pending.result_tx.send(pending.output);
        }
        while let Ok(cmd) = request_rx.try_recv() {
            let _ = cmd.result_tx.send(ExecutionOutput::error(
                "WebSocket connection closed before execution started.".to_string(),
            ));
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::channel::mpsc as futures_mpsc;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    use super::super::messages::{ExecutionStatus, Header, JupyterMessage};
    use super::{ExecuteCommand, KernelConnection};

    fn connection_with_channels() -> (
        KernelConnection,
        futures_mpsc::Receiver<Message>,
        futures_mpsc::Sender<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) {
        let (request_tx, request_rx) = tokio::sync::mpsc::channel::<ExecuteCommand>(16);
        let is_busy = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let (ws_sink, sent_rx) = futures_mpsc::channel(16);
        let (incoming_tx, ws_stream_rx) = futures_mpsc::channel(16);
        let reader_handle = tokio::spawn(KernelConnection::run_ws_loop(
            ws_sink,
            ws_stream_rx,
            request_rx,
            Arc::clone(&is_busy),
        ));
        (
            KernelConnection {
                request_tx,
                is_busy,
                in_flight,
                reader_handle,
            },
            sent_rx,
            incoming_tx,
        )
    }

    fn response(
        parent_msg_id: &str,
        channel: &str,
        msg_type: &str,
        content: serde_json::Value,
    ) -> Message {
        Message::Text(
            serde_json::to_string(&JupyterMessage {
                channel: channel.to_string(),
                header: Header {
                    msg_id: uuid::Uuid::new_v4().to_string(),
                    msg_type: msg_type.to_string(),
                    username: "test".to_string(),
                    session: "test-session".to_string(),
                    date: String::new(),
                    version: "5.3".to_string(),
                },
                parent_header: serde_json::json!({"msg_id": parent_msg_id}),
                metadata: serde_json::json!({}),
                content,
                buffers: vec![],
            })
            .expect("serialize test response")
            .into(),
        )
    }

    async fn wait_for_reader_exit(connection: &KernelConnection) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !connection.reader_handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("websocket reader should exit");
    }

    #[tokio::test]
    async fn permit_returns_to_zero_on_completion() {
        let (connection, mut sent_rx, mut incoming_tx) = connection_with_channels();
        let started = connection
            .start_execution("test-session", "1 + 1")
            .await
            .unwrap();
        assert!(connection.has_pending_work());

        sent_rx.next().await.expect("execute request sent");
        incoming_tx
            .send(Ok(response(
                &started.parent_msg_id,
                "shell",
                "execute_reply",
                serde_json::json!({"status": "ok"}),
            )))
            .await
            .unwrap();
        incoming_tx
            .send(Ok(response(
                &started.parent_msg_id,
                "iopub",
                "status",
                serde_json::json!({"execution_state": "idle"}),
            )))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while connection.has_pending_work() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed execution should release its permit");
        assert!(!connection.has_pending_work());

        let output = started.result_rx.await.unwrap();
        assert_eq!(output.status, ExecutionStatus::Complete);
    }

    #[tokio::test]
    async fn permit_returns_to_zero_on_send_error() {
        let (connection, sent_rx, _incoming_tx) = connection_with_channels();
        drop(sent_rx);
        let started = connection
            .start_execution("test-session", "1 + 1")
            .await
            .unwrap();
        assert!(connection.has_pending_work());

        let output = tokio::time::timeout(Duration::from_secs(1), started.result_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(output.stderr.contains("WebSocket send error"));
        tokio::task::yield_now().await;
        assert!(!connection.has_pending_work());
    }

    #[tokio::test]
    async fn loop_exit_resolves_pending_and_queued_commands() {
        let (connection, mut sent_rx, incoming_tx) = connection_with_channels();
        let first = connection
            .start_execution("test-session", "first")
            .await
            .unwrap();
        let second = connection
            .start_execution("test-session", "second")
            .await
            .unwrap();
        let third = connection
            .start_execution("test-session", "third")
            .await
            .unwrap();
        assert!(connection.has_pending_work());
        sent_rx.next().await.expect("first execute request sent");

        drop(incoming_tx);
        wait_for_reader_exit(&connection).await;

        let first_output = first.result_rx.await.unwrap();
        assert!(
            first_output
                .stderr
                .contains("connection closed unexpectedly")
        );
        for queued in [second, third] {
            let output = queued.result_rx.await.unwrap();
            assert!(
                output
                    .stderr
                    .contains("connection closed before execution started")
            );
        }
        assert!(!connection.has_pending_work());
    }

    #[tokio::test]
    async fn clean_eof_closes_request_receiver() {
        let (connection, _sent_rx, incoming_tx) = connection_with_channels();
        drop(incoming_tx);
        wait_for_reader_exit(&connection).await;

        let Err(error) = connection.start_execution("test-session", "1 + 1").await else {
            panic!("closed connection must reject execution");
        };
        assert!(error.to_string().contains("Kernel connection closed"));
        assert!(!connection.has_pending_work());
    }

    #[tokio::test]
    async fn queued_command_is_pending_before_reader_picks_it_up() {
        let (request_tx, request_rx) = tokio::sync::mpsc::channel::<ExecuteCommand>(1);
        let reader_handle = tokio::spawn(std::future::pending::<()>());
        let in_flight = Arc::new(AtomicUsize::new(0));
        let connection = KernelConnection {
            request_tx,
            is_busy: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::clone(&in_flight),
            reader_handle,
        };

        let _started = connection
            .start_execution("test-session", "1 + 1")
            .await
            .unwrap();
        assert!(connection.has_pending_work());
        assert!(!connection.is_busy());

        drop(request_rx);
        assert_eq!(in_flight.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn dropping_fenced_connection_aborts_reader_task() {
        let (request_tx, _request_rx) = tokio::sync::mpsc::channel::<ExecuteCommand>(1);
        let reader_handle = tokio::spawn(std::future::pending::<()>());
        let abort_handle = reader_handle.abort_handle();
        let connection = KernelConnection {
            request_tx,
            is_busy: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            reader_handle,
        };
        drop(connection);
        tokio::task::yield_now().await;
        assert!(abort_handle.is_finished());
    }
}
