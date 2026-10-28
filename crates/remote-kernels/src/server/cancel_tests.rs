//! A cancelled `execute`/`wait`/`get_output` keeps every result collectable: held
//! receivers go back to `pending_executions`, finished outputs to
//! `recovered_executions`. Driven with plain oneshot channels, no kernel.

use rmcp::model::CallToolResult;
use tokio::sync::oneshot;

use super::RemoteKernelsServer;
use super::execution::CallContext;
use super::params::{GetOutputParams, WaitParams};
use super::tests::test_instance;
use crate::jupyter::messages::{ExecutionOutput, Output};
use crate::state::AppState;

const MACHINE: &str = "machine-a";
const KERNEL: &str = "kernel-1";

fn server_with_kernel(dir: &std::path::Path) -> RemoteKernelsServer {
    let server = RemoteKernelsServer::new(
        toml::from_str("").unwrap(),
        AppState::new(dir.to_path_buf()),
        None,
    );
    let mut instance = test_instance(MACHINE);
    instance.kernel_ids.push(KERNEL.to_string());
    server
        .state
        .try_lock()
        .unwrap()
        .instances
        .insert(MACHINE.to_string(), instance);
    server
}

fn stdout(text: &str) -> ExecutionOutput {
    let mut output = ExecutionOutput::default();
    output.outputs.push(Output::Stream {
        stderr: false,
        text: text.to_string(),
    });
    output
}

fn cancelled() -> CallContext {
    let ctx = CallContext::detached();
    ctx.cancel.cancel();
    ctx
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn recovered_text(server: &RemoteKernelsServer, cell_number: u32) -> Option<String> {
    let state = server.state.try_lock().unwrap();
    state.instances[MACHINE]
        .recovered_executions
        .get(&(KERNEL.to_string(), cell_number))
        .map(|output| match &output.outputs[..] {
            [Output::Stream { text, .. }] => text.clone(),
            other => panic!("unexpected outputs {other:?}"),
        })
}

fn pending_cells(server: &RemoteKernelsServer) -> Vec<u32> {
    let state = server.state.try_lock().unwrap();
    let mut cells: Vec<u32> = state.instances[MACHINE]
        .pending_executions
        .keys()
        .map(|(_, cell)| *cell)
        .collect();
    cells.sort_unstable();
    cells
}

fn park_pending(
    server: &RemoteKernelsServer,
    cell_number: u32,
) -> oneshot::Sender<ExecutionOutput> {
    let (tx, rx) = oneshot::channel();
    server
        .state
        .try_lock()
        .unwrap()
        .instances
        .get_mut(MACHINE)
        .unwrap()
        .pending_executions
        .insert((KERNEL.to_string(), cell_number), rx);
    tx
}

#[tokio::test]
async fn recovered_output_is_consumed_only_by_a_live_call() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_kernel(dir.path());
    server
        .state
        .try_lock()
        .unwrap()
        .instances
        .get_mut(MACHINE)
        .unwrap()
        .recovered_executions
        .insert((KERNEL.to_string(), 3), stdout("kept"));
    let params = || GetOutputParams {
        kernel_id: KERNEL.to_string(),
        cell_number: 3,
        wait: None,
        timeout: None,
    };

    // A cancelled get_output parks it again instead of consuming it.
    server
        .get_output_inner(params(), cancelled())
        .await
        .unwrap();
    assert_eq!(recovered_text(&server, 3).as_deref(), Some("kept"));

    let result = server
        .get_output_inner(params(), CallContext::detached())
        .await
        .unwrap();
    assert!(text_of(&result).contains("kept"), "{}", text_of(&result));
    assert_eq!(recovered_text(&server, 3), None);
}

#[tokio::test]
async fn cancelled_wait_reparks_the_running_execution() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_kernel(dir.path());
    let tx = park_pending(&server, 4);

    let result = server
        .wait_inner(
            WaitParams {
                kernel_id: Some(KERNEL.to_string()),
                timeout: None,
            },
            cancelled(),
        )
        .await
        .unwrap();
    assert!(
        text_of(&result).contains("Cancelled"),
        "{}",
        text_of(&result)
    );
    assert_eq!(pending_cells(&server), vec![4]);

    // The re-parked receiver still delivers to a later call.
    tx.send(stdout("finished later")).unwrap();
    let result = server
        .get_output_inner(
            GetOutputParams {
                kernel_id: KERNEL.to_string(),
                cell_number: 4,
                wait: Some(false),
                timeout: None,
            },
            CallContext::detached(),
        )
        .await
        .unwrap();
    assert!(
        text_of(&result).contains("finished later"),
        "{}",
        text_of(&result)
    );
}

#[tokio::test]
async fn cancelled_get_output_wait_reparks_the_running_execution() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_kernel(dir.path());
    let _tx = park_pending(&server, 5);
    server
        .get_output_inner(
            GetOutputParams {
                kernel_id: KERNEL.to_string(),
                cell_number: 5,
                wait: Some(true),
                timeout: Some(0),
            },
            cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(pending_cells(&server), vec![5]);
}

#[tokio::test]
async fn cancelled_wait_all_keeps_finished_and_running_executions() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_kernel(dir.path());
    let finished = park_pending(&server, 6);
    let also_finished = park_pending(&server, 7);
    let _running = park_pending(&server, 8);
    finished.send(stdout("done before the cancel")).unwrap();
    also_finished.send(stdout("also done")).unwrap();

    server
        .wait_inner(
            WaitParams {
                kernel_id: None,
                timeout: None,
            },
            cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(
        recovered_text(&server, 6).as_deref(),
        Some("done before the cancel")
    );
    assert_eq!(recovered_text(&server, 7).as_deref(), Some("also done"));
    assert_eq!(pending_cells(&server), vec![8]);
}
