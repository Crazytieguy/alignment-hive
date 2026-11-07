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
        let mut record = InstanceRecord::for_test("fake", "fake-test-id", Phase::Provisioning);
        record.cleanup = cleanup;
        crate::state::create_record(&state.project_dir, "main", &record).unwrap();
        state.instances.insert(
            "main".to_string(),
            InstanceState::from_record("main", &record),
        );
    }
    (server, dir)
}

async fn record_of(server: &RemoteKernelsServer) -> Option<InstanceRecord> {
    let state = server.state.lock().await;
    crate::state::load_instance_record(&state.project_dir, "main")
}

/// `stop()` during setup leaves no instance and a Stopped record. The setup that
/// then fails must not apply its terminate policy to the stopped machine.
#[tokio::test]
async fn failed_start_cleanup_leaves_a_machine_stopped_during_setup_alone() {
    let (server, _dir) = server_with_instance(Cleanup::Terminate).await;
    {
        let mut state = server.state.lock().await;
        state.instances.remove("main").unwrap();
        crate::state::with_record(&state.project_dir, "main", |record| {
            record.phase = Phase::Stopped;
        })
        .unwrap();
    }
    for force_terminate in [false, true] {
        let outcome = server
            .cleanup_failed_start("main", "fake-test-id", "fake", force_terminate)
            .await;
        assert!(matches!(outcome, FailedStartCleanup::Stopped));
        let record = record_of(&server).await.expect("record must be kept");
        assert_eq!(record.phase, Phase::Stopped);
    }
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

/// The provider has no such machine (the fake knows nothing of this id): that
/// is a positive fact, so its record goes, whatever the policy.
#[tokio::test]
async fn failed_start_on_a_machine_the_provider_no_longer_has_clears_it() {
    let (server, _dir) = server_with_instance(Cleanup::Stop).await;
    let outcome = server
        .cleanup_failed_start("main", "fake-test-id", "fake", false)
        .await;
    assert!(matches!(outcome, FailedStartCleanup::Stopped));
    assert!(record_of(&server).await.is_none());
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
