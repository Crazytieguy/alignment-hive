use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use tokio::sync::oneshot;

use crate::config::Cleanup;
use crate::heartbeat::HeartbeatState;
use crate::jupyter::messages::ExecutionOutput;
use crate::jupyter::rest::JupyterClient;
use crate::jupyter::ws::KernelConnection;
use crate::notebook::Notebook;
use crate::runpod::types::Pod;

/// Persisted state — written to `.claude/remote-kernels/state.json` so the stop hook can read it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<String>,
    /// Accumulated session spend in dollars. Monotonically increasing, never resets.
    #[serde(default)]
    pub accumulated_spend: f64,
    /// Jupyter token for reconnecting to a pod across sessions/crashes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jupyter_token: Option<String>,
    /// Path to the SSH private key for reconnecting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,
    /// GPU name for display on reconnection (REST `get_pod` doesn't always include machine info).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gpu_name: Option<String>,
}

/// Runtime state held in memory by the MCP server.
pub struct AppState {
    pub project_dir: PathBuf,
    pub pod: Option<PodState>,
    /// Accumulated spend from previous pods in this session. Monotonically increasing.
    pub accumulated_spend: f64,
}

pub struct PodState {
    pub pod_id: String,
    pub gpu_name: String,
    pub cost_per_hr: f64,
    pub started_at: std::time::Instant,
    pub jupyter: JupyterClient,
    pub jupyter_token: String,
    pub session_id: String,
    pub kernel_ids: Vec<String>,
    pub kernel_connections: HashMap<String, KernelConnection>,
    pub notebooks: HashMap<String, Notebook>,
    pub ssh_key_path: PathBuf,
    pub public_ip: Option<String>,
    pub ssh_port: Option<u16>,
    pub heartbeat: Option<HeartbeatState>,
    /// Pending executions that timed out. Keyed by (`kernel_id`, `cell_number`).
    pub pending_executions: HashMap<(String, u32), oneshot::Receiver<ExecutionOutput>>,
}

impl PodState {
    /// Cost incurred by the current pod since it started.
    pub fn current_pod_cost(&self) -> f64 {
        self.cost_per_hr * self.started_at.elapsed().as_secs_f64() / 3600.0
    }
}

impl AppState {
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            pod: None,
            // Budget is per Claude session (MCP server lifetime), not per pod.
            // Each session starts fresh.
            accumulated_spend: 0.0,
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.project_dir.join(".claude/remote-kernels")
    }

    fn state_path(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }

    /// Total session spend: accumulated from previous pods + current pod's running cost.
    pub fn total_spend(&self) -> f64 {
        self.accumulated_spend + self.pod.as_ref().map_or(0.0, PodState::current_pod_cost)
    }

    /// Persist the current state to disk.
    pub fn save(&self, cleanup: Cleanup) -> anyhow::Result<()> {
        let pod = self.pod.as_ref();
        self.write_persisted(
            pod.map(|p| p.pod_id.as_str()),
            cleanup,
            pod.map(|p| p.jupyter_token.as_str()),
            pod.map(|p| p.ssh_key_path.display().to_string()),
            pod.map(|p| p.gpu_name.clone()),
        )
    }

    /// Persist state to disk with an explicit `pod_id`.
    ///
    /// Used by `stop()` and graceful shutdown which need to clear the in-memory
    /// `PodState` (to stop spend accumulation) while preserving the `pod_id` and
    /// reconnection details on disk.
    pub fn save_with_pod_id(&self, pod_id: Option<&str>, cleanup: Cleanup) -> anyhow::Result<()> {
        // If the pod is still in memory, grab its reconnection details.
        // If already taken, fall back to whatever we have from state.json.
        let (jupyter_token, ssh_key_path, gpu_name) = if let Some(p) = &self.pod {
            (
                Some(p.jupyter_token.clone()),
                Some(p.ssh_key_path.display().to_string()),
                Some(p.gpu_name.clone()),
            )
        } else {
            // Pod already taken — load reconnection details from disk.
            let existing = Self::load_persisted(&self.project_dir);
            (
                existing.as_ref().and_then(|s| s.jupyter_token.clone()),
                existing.as_ref().and_then(|s| s.ssh_key_path.clone()),
                existing.as_ref().and_then(|s| s.gpu_name.clone()),
            )
        };
        self.write_persisted(
            pod_id,
            cleanup,
            jupyter_token.as_deref(),
            ssh_key_path,
            gpu_name,
        )
    }

    fn write_persisted(
        &self,
        pod_id: Option<&str>,
        cleanup: Cleanup,
        jupyter_token: Option<&str>,
        ssh_key_path: Option<String>,
        gpu_name: Option<String>,
    ) -> anyhow::Result<()> {
        let dir = self.state_dir();
        std::fs::create_dir_all(&dir)?;

        let gitignore = dir.join(".gitignore");
        if !gitignore.exists() {
            let _ = std::fs::write(&gitignore, "*\n");
        }

        let persisted = PersistedState {
            pod_id: pod_id.map(String::from),
            cleanup: Some(match cleanup {
                Cleanup::Stop => "stop".to_string(),
                Cleanup::Terminate => "terminate".to_string(),
                Cleanup::Disabled => "disabled".to_string(),
            }),
            accumulated_spend: self.total_spend(),
            jupyter_token: jupyter_token.map(String::from),
            ssh_key_path,
            gpu_name,
        };

        let json = serde_json::to_string_pretty(&persisted)?;
        std::fs::write(self.state_path(), json)?;
        Ok(())
    }

    /// Snapshot accumulated spend (adds current pod cost to accumulated total).
    /// Called when a pod is stopped/terminated so the spend persists.
    pub fn snapshot_spend(&mut self) {
        if let Some(ref pod) = self.pod {
            self.accumulated_spend += pod.current_pod_cost();
        }
    }

    /// Clear persisted state (called after pod is stopped/terminated).
    pub fn clear(&self) -> anyhow::Result<()> {
        let path = self.state_path();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Record that a pod has started.
    pub fn set_pod(
        &mut self,
        pod: &Pod,
        jupyter: JupyterClient,
        jupyter_token: String,
        ssh_key_path: PathBuf,
    ) {
        self.pod = Some(PodState {
            pod_id: pod.id.clone(),
            gpu_name: pod.gpu_display_name().to_string(),
            cost_per_hr: pod.cost_per_hr.unwrap_or(0.0),
            started_at: std::time::Instant::now(),
            jupyter,
            jupyter_token,
            session_id: uuid::Uuid::new_v4().to_string(),
            kernel_ids: Vec::new(),
            kernel_connections: HashMap::new(),
            notebooks: HashMap::new(),
            ssh_key_path,
            public_ip: None,
            ssh_port: None,
            heartbeat: None,
            pending_executions: HashMap::new(),
        });
    }

    /// Load the `pod_id` from a previous state file (if any).
    pub fn load_existing(project_dir: &Path) -> Option<String> {
        Self::load_persisted(project_dir).and_then(|s| s.pod_id)
    }

    /// Load the full persisted state from disk.
    pub fn load_persisted(project_dir: &Path) -> Option<PersistedState> {
        let path = project_dir.join(".claude/remote-kernels/state.json");
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod_state(pod_id: &str, cost_per_hr: f64, running_for: std::time::Duration) -> PodState {
        PodState {
            pod_id: pod_id.to_string(),
            gpu_name: "Test GPU".to_string(),
            cost_per_hr,
            started_at: std::time::Instant::now().checked_sub(running_for).unwrap(),
            jupyter: JupyterClient::new(pod_id, "test-token"),
            jupyter_token: "test-token".to_string(),
            session_id: "test-session".to_string(),
            kernel_ids: Vec::new(),
            kernel_connections: HashMap::new(),
            notebooks: HashMap::new(),
            ssh_key_path: PathBuf::from("/tmp/test-key"),
            public_ip: None,
            ssh_port: None,
            heartbeat: None,
            pending_executions: HashMap::new(),
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.pod = Some(pod_state("pod-123", 0.5, std::time::Duration::ZERO));

        state.save(Cleanup::Stop).unwrap();

        let persisted = AppState::load_persisted(dir.path()).unwrap();
        assert_eq!(persisted.pod_id.as_deref(), Some("pod-123"));
        assert_eq!(persisted.cleanup.as_deref(), Some("stop"));
        assert_eq!(persisted.jupyter_token.as_deref(), Some("test-token"));
        assert_eq!(persisted.ssh_key_path.as_deref(), Some("/tmp/test-key"));
        assert_eq!(persisted.gpu_name.as_deref(), Some("Test GPU"));
        assert_eq!(
            AppState::load_existing(dir.path()).as_deref(),
            Some("pod-123")
        );
    }

    #[test]
    fn state_dir_gets_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        state.save(Cleanup::Terminate).unwrap();

        let gitignore = dir.path().join(".claude/remote-kernels/.gitignore");
        assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "*\n");
    }

    #[test]
    fn clear_removes_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        state.save(Cleanup::Terminate).unwrap();
        assert!(AppState::load_persisted(dir.path()).is_some());

        state.clear().unwrap();
        assert!(AppState::load_persisted(dir.path()).is_none());
        // Clearing twice is fine.
        state.clear().unwrap();
    }

    #[test]
    fn save_with_pod_id_falls_back_to_disk_for_reconnection_details() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.pod = Some(pod_state("pod-123", 0.5, std::time::Duration::ZERO));
        state.save(Cleanup::Stop).unwrap();

        // Simulate stop(): pod taken out of memory, but reconnection details
        // must survive via the state file.
        state.pod = None;
        state
            .save_with_pod_id(Some("pod-123"), Cleanup::Stop)
            .unwrap();

        let persisted = AppState::load_persisted(dir.path()).unwrap();
        assert_eq!(persisted.pod_id.as_deref(), Some("pod-123"));
        assert_eq!(persisted.jupyter_token.as_deref(), Some("test-token"));
        assert_eq!(persisted.gpu_name.as_deref(), Some("Test GPU"));
    }

    #[test]
    fn spend_is_monotonic_across_pods() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        assert!(state.total_spend().abs() < f64::EPSILON);

        // One hour at $0.50/hr.
        state.pod = Some(pod_state(
            "pod-1",
            0.5,
            std::time::Duration::from_secs(3600),
        ));
        let with_pod = state.total_spend();
        assert!((with_pod - 0.5).abs() < 0.01, "spend was {with_pod}");

        // Snapshot on stop/terminate folds the pod cost into the accumulated total.
        state.snapshot_spend();
        state.pod = None;
        let after_snapshot = state.total_spend();
        assert!((after_snapshot - with_pod).abs() < 0.01);

        // A second pod adds on top; the total never resets.
        state.pod = Some(pod_state(
            "pod-2",
            1.0,
            std::time::Duration::from_secs(1800),
        ));
        let with_second = state.total_spend();
        assert!((with_second - (after_snapshot + 0.5)).abs() < 0.01);
        assert!(with_second >= after_snapshot);
    }

    /// Characterizes current behavior: a fresh `AppState` (new MCP server process)
    /// does NOT hydrate `accumulated_spend` from the state file — spend tracking is
    /// per server lifetime, and a mid-session server restart forgets prior spend
    /// even though it was persisted. The multi-instance state refactor must decide
    /// this explicitly (and preserve persisted spend across restarts) rather than
    /// inherit it silently.
    #[test]
    fn fresh_app_state_does_not_hydrate_persisted_spend() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.pod = Some(pod_state(
            "pod-1",
            2.0,
            std::time::Duration::from_secs(3600),
        ));
        state.save(Cleanup::Stop).unwrap();

        // Simulate an MCP server restart in the same project.
        let restarted = AppState::new(dir.path().to_path_buf());
        assert!(restarted.total_spend().abs() < f64::EPSILON);
        // The persisted value is still on disk, just not loaded.
        let persisted = AppState::load_persisted(dir.path()).unwrap();
        assert!((persisted.accumulated_spend - 2.0).abs() < 0.01);
    }

    #[test]
    fn persisted_spend_survives_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.pod = Some(pod_state(
            "pod-1",
            2.0,
            std::time::Duration::from_secs(3600),
        ));
        state.save(Cleanup::Terminate).unwrap();

        let persisted = AppState::load_persisted(dir.path()).unwrap();
        assert!((persisted.accumulated_spend - 2.0).abs() < 0.01);
    }
}
