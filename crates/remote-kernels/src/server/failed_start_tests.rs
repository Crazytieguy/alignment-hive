//! Failed-start cleanup dispositions, driven by the per-machine cleanup
//! policy. Uses the fake runtime, whose `terminate` is idempotent (Ok for
//! unknown ids) while `stop` errors on unknown ids — exercising both the
//! confirmed and unconfirmed provider outcomes without spawning processes.

use super::provision::FailedStartCleanup;
use super::*;
use crate::state::InstanceRecord;

async fn server_with_instance(cleanup: Cleanup) -> (RemoteKernelsServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(r#"default-runtime = "fake""#).unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    {
        let mut state = server.state.lock().await;
        let inst = InstanceState::provisioning(
            "main".to_string(),
            None,
            "fake".to_string(),
            "fake-test-id".to_string(),
            "GPU".to_string(),
            1.0,
            cleanup,
            "token".to_string(),
            dir.path().join("id_ed25519"),
            false,
        );
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state.save_record("main", &record).unwrap();
    }
    (server, dir)
}

async fn record_of(server: &RemoteKernelsServer) -> Option<InstanceRecord> {
    let state = server.state.lock().await;
    crate::state::load_instance_record(&state.project_dir, "main")
}

#[tokio::test]
async fn failed_start_terminate_policy_clears_record() {
    let (server, _dir) = server_with_instance(Cleanup::Terminate).await;
    let outcome = server
        .cleanup_failed_start("main", "fake-test-id", "fake", false)
        .await;
    assert!(matches!(outcome, FailedStartCleanup::Terminated));
    assert!(record_of(&server).await.is_none(), "record must be cleared");
    assert!(server.state.lock().await.instances.is_empty());
}

/// Stop policy: the machine must NOT be terminated. Here the provider
/// stop is unconfirmed (fake stop errors on unknown ids), so the record
/// must survive for retry — nothing is forgotten on provider failure.
#[tokio::test]
async fn failed_start_stop_policy_never_terminates_and_keeps_record() {
    let (server, _dir) = server_with_instance(Cleanup::Stop).await;
    let outcome = server
        .cleanup_failed_start("main", "fake-test-id", "fake", false)
        .await;
    assert!(matches!(outcome, FailedStartCleanup::Unconfirmed));
    let record = record_of(&server).await.expect("record must be kept");
    assert_eq!(record.external_id, "fake-test-id");
}

#[tokio::test]
async fn failed_start_disabled_policy_leaves_machine_and_record() {
    let (server, _dir) = server_with_instance(Cleanup::Disabled).await;
    let outcome = server
        .cleanup_failed_start("main", "fake-test-id", "fake", false)
        .await;
    assert!(matches!(outcome, FailedStartCleanup::LeftRunning));
    let record = record_of(&server).await.expect("record must be kept");
    assert_eq!(record.phase, Phase::Running);
}

/// Provision-timeout overshoot forces termination even under a stop or
/// disabled policy — the timeout is the money backstop for stuck hosts.
#[tokio::test]
async fn failed_start_force_terminate_overrides_policy() {
    for cleanup in [Cleanup::Stop, Cleanup::Disabled] {
        let (server, _dir) = server_with_instance(cleanup).await;
        let outcome = server
            .cleanup_failed_start("main", "fake-test-id", "fake", true)
            .await;
        assert!(matches!(outcome, FailedStartCleanup::Terminated));
        assert!(record_of(&server).await.is_none(), "record must be cleared");
    }
}
