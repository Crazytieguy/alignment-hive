//! Direct provider API access for the harness: inventory checks, reaping and
//! billing queries. Nothing here goes through the server under test, and
//! nothing here prints a key. Every mutating call takes a machine that a name
//! prefix selected, so a pod the harness did not create is out of reach.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

const RUNPOD: &str = "https://api.runpod.io/v2";
const RUNPOD_GRAPHQL: &str = "https://api.runpod.io/graphql";
const VAST: &str = "https://console.vast.ai";

/// Default location of the repo's credentials; `RK_LIVE_ENV_FILE` overrides.
const ENV_FILE: &str = "/Users/yoav/projects/alignment-hive/.env.local";

fn credentials() -> &'static HashMap<String, String> {
    static CREDS: OnceLock<HashMap<String, String>> = OnceLock::new();
    CREDS.get_or_init(|| {
        let mut found = HashMap::new();
        let path = std::env::var("RK_LIVE_ENV_FILE").unwrap_or_else(|_| ENV_FILE.to_string());
        if let Ok(iter) = dotenvy::from_path_iter(&path) {
            for (key, value) in iter.flatten() {
                found.insert(key, value);
            }
        }
        for name in ["RUNPOD_API_KEY", "VAST_API_KEY"] {
            if let Ok(value) = std::env::var(name) {
                found.insert(name.to_string(), value);
            }
        }
        found
    })
}

pub fn runpod_key() -> String {
    credentials()
        .get("RUNPOD_API_KEY")
        .cloned()
        .expect("RUNPOD_API_KEY: set it or add it to .env.local")
}

pub fn vast_key() -> String {
    credentials()
        .get("VAST_API_KEY")
        .cloned()
        .expect("VAST_API_KEY: set it or add it to .env.local")
}

/// Remove the provider keys from text bound for a log or the terminal.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for key in credentials().values() {
        if key.len() >= 8 {
            out = out.replace(key.as_str(), "<REDACTED>");
        }
    }
    out
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        remote_kernels::init_tls();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("http runtime")
    })
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let _ = runtime();
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .no_proxy()
            .user_agent("rk-live-harness/1.0")
            .build()
            .expect("http client")
    })
}

/// One API call with retries on transport errors and 429. Returns the status
/// and the body (JSON when it parses, else a string).
pub fn http(
    method: reqwest::Method,
    url: &str,
    key: &str,
    body: Option<&Value>,
) -> anyhow::Result<(u16, Value)> {
    let mut last = None;
    for attempt in 0..5u64 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(2 * attempt));
        }
        let mut request = client().request(method.clone(), url).bearer_auth(key);
        if let Some(body) = body {
            request = request.json(body);
        }
        let outcome = runtime().block_on(async {
            let response = request.send().await?;
            let status = response.status().as_u16();
            let text = response.text().await?;
            Ok::<_, reqwest::Error>((status, text))
        });
        match outcome {
            Ok((429, _)) => last = Some(anyhow::anyhow!("HTTP 429 from {url}")),
            Ok((status, text)) => {
                let value =
                    serde_json::from_str(&text).unwrap_or_else(|_| Value::String(scrub(&text)));
                return Ok((status, value));
            }
            Err(error) => last = Some(anyhow::anyhow!(scrub(&error.to_string()))),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt made")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Runpod,
    Vast,
}

impl Provider {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Runpod => "runpod",
            Self::Vast => "vast",
        }
    }
}

/// What the provider says about one machine.
#[derive(Clone, Debug)]
pub struct Machine {
    pub provider: Provider,
    pub id: String,
    pub name: String,
    pub status: String,
    /// Compute plus disk while it runs, $/hr.
    pub running_rate: f64,
    /// Storage only, $/hr.
    pub stopped_rate: f64,
    pub raw: Value,
}

impl Machine {
    /// The GPU has been released: only storage bills now.
    pub fn is_stopped(&self) -> bool {
        let status = self.status.to_ascii_lowercase();
        // A vast VM that shut itself down reads "created", not "exited". Its
        // /charges entry confirms the GPU stops billing there, storage doesn't.
        matches!(status.as_str(), "exited" | "stopped" | "paused")
            || (self.provider == Provider::Vast && status == "created")
    }

    pub fn has_ssh(&self) -> bool {
        match self.provider {
            Provider::Runpod => self
                .raw
                .pointer("/ssh/direct")
                .is_some_and(|v| !v.is_null()),
            Provider::Vast => self.raw.get("ssh_host").is_some_and(|v| !v.is_null()),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {:?} status={} rate=${:.3}/hr",
            self.provider.name(),
            self.id,
            self.name,
            self.status,
            self.running_rate
        )
    }

    fn runpod(pod: &Value) -> Option<Self> {
        let id = pod.get("id")?.as_str()?.to_string();
        let status = pod
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        // v2 reports `cost`; a pod that doesn't is priced at the top of the
        // GPU ladder rather than at nothing.
        let compute = pod.get("cost").and_then(Value::as_f64).unwrap_or(0.30);
        let gb = |key: &str| pod.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let disk_gb = gb("containerDiskInGb") + gb("disk");
        let volume_gb = gb("volumeInGb")
            + pod
                .pointer("/volume/size")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
        Some(Self {
            provider: Provider::Runpod,
            id,
            name: pod
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: status.to_string(),
            // RunPod's published rates: $0.10/GB/month on running pods,
            // $0.20/GB/month for a stopped pod's volume.
            running_rate: compute + (disk_gb + volume_gb) * 0.10 / 730.0,
            stopped_rate: volume_gb * 0.20 / 730.0,
            raw: pod.clone(),
        })
    }

    fn vast(instance: &Value) -> Option<Self> {
        let id = instance.get("id")?.as_i64()?.to_string();
        let compute = instance
            .get("dph_total")
            .and_then(Value::as_f64)
            .unwrap_or(0.5);
        let storage = instance
            .get("storage_total_cost")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        Some(Self {
            provider: Provider::Vast,
            id,
            name: instance
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: instance
                .get("actual_status")
                .and_then(Value::as_str)
                .unwrap_or("provisioning")
                .to_string(),
            // dph_total already includes storage (checked against /charges,
            // which itemizes the two).
            running_rate: compute,
            stopped_rate: storage,
            raw: instance.clone(),
        })
    }
}

pub fn list(provider: Provider, prefix: &str) -> anyhow::Result<Vec<Machine>> {
    let machines = match provider {
        Provider::Runpod => {
            let (status, body) = http(
                reqwest::Method::GET,
                &format!("{RUNPOD}/pods"),
                &runpod_key(),
                None,
            )?;
            anyhow::ensure!(status == 200, "list pods: HTTP {status}: {body}");
            let pods = body.get("pods").unwrap_or(&body);
            pods.as_array()
                .ok_or_else(|| anyhow::anyhow!("list pods: not an array: {body}"))?
                .iter()
                .filter_map(Machine::runpod)
                .collect::<Vec<_>>()
        }
        Provider::Vast => {
            let (status, body) = http(
                reqwest::Method::GET,
                &format!("{VAST}/api/v0/instances/"),
                &vast_key(),
                None,
            )?;
            anyhow::ensure!(status == 200, "list instances: HTTP {status}: {body}");
            body.get("instances")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("list instances: no array: {body}"))?
                .iter()
                .filter_map(Machine::vast)
                .collect::<Vec<_>>()
        }
    };
    Ok(machines
        .into_iter()
        .filter(|machine| machine.name.starts_with(prefix))
        .collect())
}

/// `Ok(None)` when the provider says the machine is gone.
pub fn get(provider: Provider, id: &str) -> anyhow::Result<Option<Machine>> {
    match provider {
        Provider::Runpod => {
            let (status, body) = http(
                reqwest::Method::GET,
                &format!("{RUNPOD}/pods/{id}"),
                &runpod_key(),
                None,
            )?;
            match status {
                404 => Ok(None),
                200 => {
                    let machine = Machine::runpod(&body);
                    if machine.as_ref().is_some_and(|m| m.status == "TERMINATED") {
                        return Ok(None);
                    }
                    Ok(machine)
                }
                other => anyhow::bail!("get pod {id}: HTTP {other}: {body}"),
            }
        }
        Provider::Vast => {
            let (status, body) = http(
                reqwest::Method::GET,
                &format!("{VAST}/api/v0/instances/{id}/"),
                &vast_key(),
                None,
            )?;
            match status {
                404 => Ok(None),
                200 => Ok(body
                    .get("instances")
                    .filter(|value| value.is_object())
                    .and_then(Machine::vast)),
                other => anyhow::bail!("get instance {id}: HTTP {other}: {body}"),
            }
        }
    }
}

pub fn destroy(machine: &Machine) -> anyhow::Result<()> {
    let (status, body) = match machine.provider {
        Provider::Runpod => http(
            reqwest::Method::DELETE,
            &format!("{RUNPOD}/pods/{}", machine.id),
            &runpod_key(),
            None,
        )?,
        Provider::Vast => http(
            reqwest::Method::DELETE,
            &format!("{VAST}/api/v0/instances/{}/", machine.id),
            &vast_key(),
            None,
        )?,
    };
    anyhow::ensure!(
        status < 300 || status == 404,
        "destroy {}: HTTP {status}: {body}",
        machine.id
    );
    Ok(())
}

/// Stop a pod at the provider, behind the server's back.
pub fn runpod_stop(id: &str) -> anyhow::Result<()> {
    let (status, body) = http(
        reqwest::Method::POST,
        &format!("{RUNPOD}/pods/{id}/action"),
        &runpod_key(),
        Some(&json!({"action": "stop"})),
    )?;
    anyhow::ensure!(status < 300, "stop pod {id}: HTTP {status}: {body}");
    Ok(())
}

/// One name-prefixed network volume, for L4. Returns its id.
pub fn create_network_volume(name: &str, datacenter: &str, size_gb: u32) -> anyhow::Result<String> {
    let (status, body) = http(
        reqwest::Method::POST,
        &format!("{RUNPOD}/network-volumes"),
        &runpod_key(),
        Some(&json!({"name": name, "size": size_gb, "dataCenter": datacenter})),
    )?;
    anyhow::ensure!(status < 300, "create volume: HTTP {status}: {body}");
    body.get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("create volume: no id: {body}"))
}

pub fn network_volumes(prefix: &str) -> anyhow::Result<Vec<(String, String)>> {
    let (status, body) = http(
        reqwest::Method::GET,
        &format!("{RUNPOD}/network-volumes"),
        &runpod_key(),
        None,
    )?;
    anyhow::ensure!(status == 200, "list volumes: HTTP {status}: {body}");
    let volumes = body.get("networkVolumes").unwrap_or(&body);
    Ok(volumes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|volume| {
                    let name = volume.get("name")?.as_str()?;
                    let id = volume.get("id")?.as_str()?;
                    name.starts_with(prefix)
                        .then(|| (id.to_string(), name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn delete_network_volume(id: &str) -> anyhow::Result<()> {
    let (status, body) = http(
        reqwest::Method::DELETE,
        &format!("{RUNPOD}/network-volumes/{id}"),
        &runpod_key(),
        None,
    )?;
    anyhow::ensure!(
        status < 300 || status == 404,
        "delete volume {id}: HTTP {status}: {body}"
    );
    Ok(())
}

/// What `/v2/billing/pods` reports for one pod. The window's end is passed
/// explicitly: left out, it stops at the current hour's start and hides a pod
/// younger than that.
pub fn runpod_billed(pod_id: &str) -> anyhow::Result<f64> {
    let end = chrono::Utc::now() + chrono::Duration::hours(2);
    let start = end - chrono::Duration::days(2);
    let (status, body) = http(
        reqwest::Method::GET,
        &format!(
            "{RUNPOD}/billing/pods?podId={pod_id}&bucketSize=hour&startTime={}&endTime={}",
            start.format("%Y-%m-%dT%H:00:00Z"),
            end.format("%Y-%m-%dT%H:00:00Z"),
        ),
        &runpod_key(),
        None,
    )?;
    anyhow::ensure!(status == 200, "billing/pods: HTTP {status}: {body}");
    Ok(body
        .pointer("/metadata/totals/totalAmount")
        .and_then(Value::as_f64)
        .unwrap_or(0.0))
}

/// The account's remaining RunPod credit, from GraphQL (the REST API has no
/// balance call).
pub fn runpod_balance() -> anyhow::Result<f64> {
    let (status, body) = http(
        reqwest::Method::POST,
        RUNPOD_GRAPHQL,
        &runpod_key(),
        Some(&json!({"query": "query { myself { clientBalance } }"})),
    )?;
    anyhow::ensure!(status == 200, "balance: HTTP {status}: {body}");
    body.pointer("/data/myself/clientBalance")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("balance: unreadable: {body}"))
}

/// What `RunPod` says the whole account is billing per hour right now. A way to
/// compare the server's rates with the provider's: storage included, unlike a
/// pod's own cost.
pub fn runpod_spend_per_hr() -> anyhow::Result<f64> {
    let (_, body) = http(
        reqwest::Method::POST,
        RUNPOD_GRAPHQL,
        &runpod_key(),
        Some(&json!({"query": "{ myself { currentSpendPerHr } }"})),
    )?;
    body["data"]["myself"]["currentSpendPerHr"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("no currentSpendPerHr in the reply"))
}

pub fn vast_credit() -> anyhow::Result<f64> {
    let (status, body) = http(
        reqwest::Method::GET,
        &format!("{VAST}/api/v0/users/current/"),
        &vast_key(),
        None,
    )?;
    anyhow::ensure!(status == 200, "vast user: HTTP {status}: {body}");
    Ok(body.get("credit").and_then(Value::as_f64).unwrap_or(0.0))
}

/// Remove from the vast account every SSH key whose public half sits under
/// `keys_root`: each scratch project's key is registered at the account, and
/// nothing else would ever remove it.
pub fn vast_remove_scratch_keys(keys_root: &Path) -> anyhow::Result<usize> {
    let mut wanted = Vec::new();
    collect_public_keys(keys_root, &mut wanted);
    if wanted.is_empty() {
        return Ok(0);
    }
    let (status, body) = http(
        reqwest::Method::GET,
        &format!("{VAST}/api/v0/ssh/"),
        &vast_key(),
        None,
    )?;
    anyhow::ensure!(status == 200, "list ssh keys: HTTP {status}: {body}");
    let keys = body.get("ssh_keys").unwrap_or(&body);
    let mut removed = 0;
    for key in keys.as_array().map_or(&[][..], Vec::as_slice) {
        let public = key
            .get("public_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !wanted.iter().any(|mine| same_key(mine, public)) {
            continue;
        }
        if let Some(id) = key.get("id").and_then(Value::as_i64) {
            let (status, body) = http(
                reqwest::Method::DELETE,
                &format!("{VAST}/api/v0/ssh/{id}/"),
                &vast_key(),
                None,
            )?;
            anyhow::ensure!(status < 300, "delete ssh key {id}: HTTP {status}: {body}");
            removed += 1;
        }
    }
    Ok(removed)
}

/// Keys compare on type and material, ignoring the comment.
fn same_key(a: &str, b: &str) -> bool {
    let head = |key: &str| key.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    !a.trim().is_empty() && head(a) == head(b)
}

fn collect_public_keys(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_public_keys(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "pub")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            out.push(text);
        }
    }
}
