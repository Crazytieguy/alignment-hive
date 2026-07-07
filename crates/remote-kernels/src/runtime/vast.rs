//! vast.ai backend: marketplace GPU instances behind the [`Runtime`] trait.
//!
//! Offers are searched with configurable filters (plus a price ceiling), then
//! accepted cheapest-first — if one offer is snapped up by someone else, the
//! next is tried. `vm = true` creates a KVM virtual machine instead of a
//! container; VMs support Docker inside (required for Inspect's sandboxes),
//! containers do not (vast bans Docker-in-Docker platform-wide). Two
//! undocumented vendor traps, both observed live 2026-07: the VM image must
//! be registry-qualified (`docker.io/vastai/kvm:...`) or vast silently
//! provisions a *container* running that image, and vast's SSH proxy cannot
//! tunnel into a KVM guest, so VMs are placed on direct-port hosts and
//! reached via the host's mapped port only. `provision` handles both.
//!
//! Connectivity: SSH only (direct to the host's public IP when it has open
//! ports, else vast's `sshN.vast.ai` proxy; VMs direct-only). Jupyter is
//! launched over SSH and reached through a local `ssh -N -L` tunnel process.
//! File sync is rsync over the same SSH, identical to `RunPod`.
//!
//! Stop/resume is officially unreliable on vast (a stopped instance stays
//! bound to its GPU and can wait in "scheduling" forever if someone else rents
//! it) — capability is `Unreliable`, and destroy is the recommended cleanup.
//! The on-machine watchdog's self-cleanup is best-effort (`shutdown`/kill —
//! there is no credential-free API to destroy from inside); the server-side
//! budget/heartbeat supervision is the primary enforcement.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::config::{Config, VastConfig};
use crate::vast::client::VastClient;
use crate::vast::types::{CreateInstanceRequest, Offer};

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StillProvisioning, StopSupport, WatchdogPolicy,
};

const JUPYTER_PORT: u16 = 18888;

/// Baseline host-picking advice shown by `search_vast_offers()`, ahead of the
/// user's `[vast] selection-guidance`.
const SELECTION_ADVICE: &str = "\
    Picking a host: prefer verified hosts with reliability >= 0.98 — the \
    cheapest offer is often cheap because the host is flaky (dead SSH, \
    glacial image pulls). Static-IP hosts fail less. dlperf_per_dph is \
    vast's value-for-money score: a slightly pricier host with much higher \
    dlperf/$ usually wins. High inet_down speeds up image pull and sync \
    (min 200 Mbps is already enforced by default); prefer geographic \
    proximity when moving a lot of data. VMs (vm = true) run only on hosts \
    with direct ports — already filtered. Rank 2-3 candidates and pass them \
    to start(vast_offers=[...]); offers churn, so the runner-up matters.";

/// Per-call overrides for the offer search, layered over the config the same
/// way `start(gpu_type=...)` overrides `gpu-type-ids`. Precedence:
/// baseline filters < `[vast.query]` < these.
#[derive(Debug, Default, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct OfferQueryOverrides {
    /// GPU names to search instead of the configured `gpu-name` list (vast
    /// naming, e.g. "RTX 4090").
    pub gpu_name: Option<Vec<String>>,
    /// Exact GPU count per offer.
    pub num_gpus: Option<u32>,
    /// Price ceiling in $/hr (overrides `max-dph`).
    pub max_dph: Option<f64>,
    /// Search VM-capable hosts (overrides `[vast] vm` for this search ONLY —
    /// whether `start()` actually creates a VM is still governed by the
    /// `[vast] vm` config; set that before starting from a VM shortlist).
    pub vm: Option<bool>,
    /// Minimum available disk space in GB.
    pub min_disk_gb: Option<f64>,
    /// Max offers to return (overrides `search-limit`).
    pub limit: Option<u32>,
    /// Any other vast query filter, e.g. `{"geolocation": {"in": ["US"]}}`
    /// or `{"static_ip": true}` (scalars mean equality). Same semantics as
    /// `[vast.query]` in the config.
    pub query: Option<std::collections::HashMap<String, serde_json::Value>>,
}

pub struct VastRuntime {
    client: VastClient,
    vast: VastConfig,
    /// Instance label prefix (from the top-level `name` config).
    name_prefix: String,
    /// Pre-SSH orphan guard window (config `orphan-halt-mins`).
    orphan_halt_mins: u64,
}

impl VastRuntime {
    pub fn new(api_key: String, config: &Config) -> Self {
        Self {
            client: VastClient::new(api_key),
            vast: config.vast.clone().unwrap_or_default(),
            name_prefix: config.name.clone(),
            orphan_halt_mins: config.orphan_halt_mins,
        }
    }

    /// Test-only: inject a client pointed at a local fake API server.
    #[cfg(test)]
    fn new_with_client(client: VastClient, config: &Config) -> Self {
        Self {
            client,
            vast: config.vast.clone().unwrap_or_default(),
            name_prefix: config.name.clone(),
            orphan_halt_mins: config.orphan_halt_mins,
        }
    }

    /// Build the offer-search filter object in explicit precedence stages,
    /// where later stages overwrite same-key entries from earlier ones:
    /// baseline filters, then vm-derived connectivity constraints, then
    /// config `gpu-name`/`max-dph`, then `[vast.query]`, then per-call typed
    /// overrides, then per-call `query`. The baselines are documented in the
    /// config template — keep the two in sync.
    fn offer_filters(
        &self,
        gpu_override: Option<&str>,
        overrides: &OfferQueryOverrides,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut filters = serde_json::Map::new();
        // Stage 1 — baselines tuned for reliability; each overridable via
        // [vast.query] or per-call query.
        filters.insert("verified".to_string(), json!({"eq": true}));
        filters.insert("reliability".to_string(), json!({"gte": 0.95}));
        filters.insert("num_gpus".to_string(), json!({"gte": 1}));
        // Image pull dominates startup; slow-link hosts can sit in "loading"
        // past the provision timeout — spend with nothing to show for it.
        filters.insert("inet_down".to_string(), json!({"gte": 200.0}));

        // Stage 2 — vm-derived constraints. The per-call vm flag only toggles
        // WHETHER these are inserted; they sit below [vast.query] so an
        // explicit query entry (e.g. a higher direct_port_count) still wins.
        if overrides.vm.unwrap_or(self.vast.vm) {
            filters.insert("vms_enabled".to_string(), json!({"eq": true}));
            // VMs are reachable via direct SSH only: vast's SSH proxy never
            // opens a working tunnel to a KVM guest (observed live 2026-07 —
            // connection refused at the proxy long after cloud-init finished
            // inside the VM, while the direct-mapped port worked instantly).
            // One port suffices: jupyter tunnels over the SSH connection.
            filters.insert("direct_port_count".to_string(), json!({"gte": 1}));
        }

        // Stage 3 — config-level typed fields.
        if !self.vast.gpu_name.is_empty() {
            filters.insert("gpu_name".to_string(), json!({"in": self.vast.gpu_name}));
        }
        if let Some(max) = self.vast.max_dph {
            filters.insert("dph_total".to_string(), json!({"lte": max}));
        }

        // Stage 4 — [vast.query] passthrough beats everything config-derived.
        for (key, value) in &self.vast.query {
            let json_value = toml_value_to_json(value);
            filters.insert(key.clone(), wrap_eq(json_value));
        }

        // Stage 5 — per-call typed overrides beat config AND [vast.query].
        // An explicit empty gpu_name list removes the GPU filter entirely.
        let gpu_names: Option<Vec<String>> = match (&overrides.gpu_name, gpu_override) {
            (Some(names), _) => Some(names.clone()),
            (None, Some(gpu)) => Some(vec![gpu.to_string()]),
            (None, None) => None,
        };
        if let Some(names) = gpu_names {
            if names.is_empty() {
                filters.remove("gpu_name");
            } else {
                filters.insert("gpu_name".to_string(), json!({"in": names}));
            }
        }
        if let Some(max) = overrides.max_dph {
            filters.insert("dph_total".to_string(), json!({"lte": max}));
        }
        if let Some(n) = overrides.num_gpus {
            filters.insert("num_gpus".to_string(), json!({"eq": n}));
        }
        if let Some(disk) = overrides.min_disk_gb {
            filters.insert("disk_space".to_string(), json!({"gte": disk}));
        }

        // Stage 6 — per-call raw query is the final word.
        if let Some(query) = &overrides.query {
            for (key, value) in query {
                filters.insert(key.clone(), wrap_eq(value.clone()));
            }
        }
        filters
    }

    /// Search offers and render the curated table + picking advice for
    /// `search_vast_offers()`.
    pub async fn search_offers_report(
        &self,
        overrides: &OfferQueryOverrides,
    ) -> anyhow::Result<String> {
        let filters = self.offer_filters(None, overrides);
        let limit = overrides.limit.unwrap_or(self.vast.search_limit);
        let offers = self.client.search_offers(filters, limit).await?;
        if offers.is_empty() {
            return Ok(
                "No vast.ai offers matched. Loosen the filters: raise max_dph, drop \
                 gpu_name constraints, or override the baseline filters (see the \
                 [vast] section of remote-kernels.toml) via the query parameter."
                    .to_string(),
            );
        }

        let mut out = String::from(
            "| offer_id | $/hr | GPU | n | VRAM_GB | reliability | dlperf | dlperf/$ \
             | net_down | net_up | disk_MBps | CUDA | static_ip | ports | geo | verified |\n\
             |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for o in &offers {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                o.id,
                fmt_f64(o.dph_total, 4),
                o.gpu_name.as_deref().unwrap_or("?"),
                o.num_gpus.map_or("?".into(), |n| n.to_string()),
                fmt_f64(o.gpu_ram.map(|mb| mb / 1024.0), 0),
                fmt_f64(o.reliability2, 4),
                fmt_f64(o.dlperf, 0),
                fmt_f64(o.dlperf_per_dphtotal, 0),
                fmt_f64(o.inet_down, 0),
                fmt_f64(o.inet_up, 0),
                fmt_f64(o.disk_bw, 0),
                fmt_f64(o.cuda_max_good, 1),
                o.static_ip.map_or("?".into(), |b| b.to_string()),
                o.direct_port_count.map_or("?".into(), |n| n.to_string()),
                o.geolocation.as_deref().unwrap_or("?"),
                o.verification.as_deref().unwrap_or("?"),
            );
        }
        let _ = write!(out, "\n{SELECTION_ADVICE}");
        if let Some(guidance) = &self.vast.selection_guidance {
            let _ = write!(out, "\n\nUser criteria: {guidance}");
        }
        Ok(out)
    }

    /// Candidates for the provision attempt loop: Claude's ranked shortlist
    /// (resolved and validated), or a fresh search taken cheapest-first. Both
    /// flow through the same loop — the money guards (fail-fast on auth,
    /// lost-response reconciliation) must not depend on how the offers were
    /// chosen.
    async fn offer_candidates(&self, req: &ProvisionRequest) -> anyhow::Result<Vec<Offer>> {
        if let Some(ids) = &req.vast_offers {
            anyhow::ensure!(!ids.is_empty(), "vast_offers is empty");
            return self.resolve_shortlist(ids).await;
        }
        let filters = self.offer_filters(req.gpu_type.as_deref(), &OfferQueryOverrides::default());
        let offers = self
            .client
            .search_offers(filters, self.vast.search_limit)
            .await?;
        if offers.is_empty() {
            anyhow::bail!(
                "No vast.ai offers matched the filters (gpu-name {:?}, vm={}, max-dph {:?}). \
                 Loosen [vast] settings in remote-kernels.toml, try a different gpu_type, or \
                 pick hosts explicitly via search_vast_offers().",
                self.vast.gpu_name,
                self.vast.vm,
                self.vast.max_dph
            );
        }
        Ok(offers
            .into_iter()
            .take(self.vast.attempt_limit as usize)
            .collect())
    }

    /// Resolve a ranked shortlist of offer ids into live, validated offers,
    /// preserving the caller's order. Bare ids are never trusted with money:
    /// each is re-fetched from the marketplace (a stale/typo'd id simply
    /// doesn't resolve), must still satisfy the vm-mode connectivity
    /// constraints, must have a known price, and must respect the configured
    /// `max-dph` ceiling — the shortlist path gets the exact same money rails
    /// as the automatic path. Unusable ids are skipped (with reasons in the
    /// error if ALL are unusable); resolving also restores full offer
    /// metadata, so the durable record never starts at $0/hr.
    async fn resolve_shortlist(&self, ids: &[i64]) -> anyhow::Result<Vec<Offer>> {
        let mut filters = serde_json::Map::new();
        // Vendor trap, observed live 2026-07: filtering bundles by `id`
        // silently returns nothing — the working filter key is
        // `ask_contract_id`, even though the response calls the same value
        // `id` (the response carries both, equal). Not in the spec's
        // documented filter properties; validated by the live e2e search
        // test, which round-trips real ids through this exact filter.
        filters.insert("ask_contract_id".to_string(), json!({"in": ids}));
        if self.vast.vm {
            filters.insert("vms_enabled".to_string(), json!({"eq": true}));
            filters.insert("direct_port_count".to_string(), json!({"gte": 1}));
        }
        let limit = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        let found = self.client.search_offers(filters, limit).await?;
        let by_id: std::collections::HashMap<i64, &Offer> =
            found.iter().map(|o| (o.id, o)).collect();

        let mut offers = Vec::new();
        let mut skipped = Vec::new();
        for id in ids {
            let Some(offer) = by_id.get(id) else {
                skipped.push(format!(
                    "{id}: not rentable anymore{}",
                    if self.vast.vm {
                        " (or not VM-capable/direct-port)"
                    } else {
                        ""
                    }
                ));
                continue;
            };
            let Some(dph) = offer.dph_total else {
                // No price, no rental — spend tracking and the budget
                // supervisor need a burn rate.
                skipped.push(format!("{id}: price unknown"));
                continue;
            };
            if let Some(max) = self.vast.max_dph
                && dph > max
            {
                skipped.push(format!("{id}: ${dph}/hr exceeds max-dph {max}"));
                continue;
            }
            offers.push((*offer).clone());
        }
        if !skipped.is_empty() {
            tracing::warn!(?skipped, "some vast_offers entries were skipped");
        }
        anyhow::ensure!(
            !offers.is_empty(),
            "None of the {} offers in vast_offers are usable: {}. Offers churn quickly — \
             call search_vast_offers() again and pass a fresh shortlist (max-dph and vm \
             constraints from remote-kernels.toml still apply).",
            ids.len(),
            skipped.join("; ")
        );
        Ok(offers)
    }

    /// The user lands unquoted in `chown -R user:user` and in the
    /// `authorized_keys` path of the onstart script — hold it to POSIX
    /// username characters, not just quote-safety.
    fn validate_ssh_user(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.vast
                .ssh_user
                .chars()
                .next()
                // POSIX: must start with a letter or underscore — also rules
                // out "-foo" (parsed as options by chown/ssh) and "."/"..".
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && self
                    .vast
                    .ssh_user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
            "[vast] ssh-user must be a plain POSIX username (got {:?})",
            self.vast.ssh_user
        );
        Ok(())
    }

    /// The single builder of the on-machine self-halt command (C3: one
    /// implementation for the onstart guard and the watchdog). Halting needs
    /// root; when the session user is not root, try passwordless sudo first
    /// and fall back to the bare command (which then only works if the image
    /// runs sshd as root anyway). No single quotes — both consumers wrap the
    /// command in a single-quoted script.
    fn halt_command(ssh_user: &str) -> String {
        if ssh_user == "root" {
            "(shutdown -h now || kill -9 1) 2>/dev/null".to_string()
        } else {
            "(sudo -n shutdown -h now || shutdown -h now || sudo -n kill -9 1 || kill -9 1) \
             2>/dev/null"
                .to_string()
        }
    }

    /// The onstart script authorizes our per-instance SSH key (as root,
    /// before any SSH attempt succeeds) and then runs the user's startup
    /// lines. Key injection via onstart needs no account-key API permission —
    /// vast restricts SSH-key management to 2FA-authenticated keys.
    ///
    /// The append is re-asserted in a background loop for the first 10
    /// minutes (an inline first assert runs synchronously; the loop covers
    /// later clobbers): vast's container entrypoint rewrites
    /// `authorized_keys` from the instance's attached keys on a schedule of
    /// its own, and on some hosts that clobbers a one-shot append (observed
    /// live 2026-07 via container sshd logs). Each pass also repairs
    /// ownership/modes, which matters on the container *fallback* — when
    /// vast quietly runs the `vastai/kvm` image as a container (see module
    /// docs), that image's entrypoint writes `authorized_keys` owned by a
    /// non-root build uid (117:1001 observed live 2026-07), and sshd's
    /// `StrictModes` then rejects every key until the file is root-owned.
    /// Real KVM VMs need none of this (cloud-init injects account keys
    /// correctly), but the same script runs there harmlessly and keeps the
    /// fallback diagnosable over SSH instead of bricked.
    ///
    /// The orphan guard ([`crate::ssh_exec::orphan_guard_line`]) is the
    /// last-resort money guard for the window before the real watchdog
    /// installs (which requires working SSH). Halt stops GPU billing
    /// (storage remains); no credentials live on the machine, so halting is
    /// all it can do.
    fn onstart_script(&self, ssh_public_key: &str) -> String {
        let key = ssh_public_key.trim();
        // onstart runs as root; sshd checks authorized_keys in the home of
        // the user we later SSH in as. With the default root the paths are
        // identical to `~`; a non-root `ssh-user` gets the key in ITS home
        // and ownership — injecting into root's home (the old behavior)
        // would silently lock a non-root config out of the machine.
        let user = self.vast.ssh_user.as_str();
        let (home, owner) = if user == "root" {
            ("/root".to_string(), "root:root".to_string())
        } else {
            (format!("/home/{user}"), format!("{user}:{user}"))
        };
        let assert_key = format!(
            "grep -qF '{key}' '{home}/.ssh/authorized_keys' 2>/dev/null \
             || echo '{key}' >> '{home}/.ssh/authorized_keys'; \
             chown -R {owner} '{home}/.ssh'; chmod 700 '{home}/.ssh'; \
             chmod 600 '{home}/.ssh/authorized_keys'"
        );
        let mut lines = vec![
            "#!/bin/bash".to_string(), // VMs require an explicit shebang
            format!("mkdir -p '{home}/.ssh'"),
            assert_key.clone(),
            crate::ssh_exec::orphan_guard_line(
                // onstart runs as root on vast regardless of ssh-user.
                &Self::halt_command("root"),
                None,
                self.orphan_halt_mins,
            ),
            format!(
                "(for _ in $(seq 120); do {assert_key}; sleep 5; done) </dev/null >/dev/null 2>&1 &"
            ),
        ];
        lines.extend(self.vast.onstart.iter().cloned());
        // Completion marker, AFTER the user lines: `open()` delays the
        // jupyter launch until this appears, because user onstart lines
        // often install the very tooling the jupyter command needs (uv,
        // conda envs) — SSH can come up minutes before they finish. It lives
        // in /var/tmp because /tmp is cleared on a VM reboot and cloud-init
        // does not re-run onstart on resume.
        lines.push("touch /var/tmp/rk_onstart_done".to_string());
        lines.join("\n") + "\n"
    }
}

fn handle_for_offer(contract: i64, offer: &crate::vast::types::Offer) -> InstanceHandle {
    InstanceHandle {
        external_id: contract.to_string(),
        gpu_name: format!(
            "{} x{}",
            offer.gpu_name.as_deref().unwrap_or("unknown"),
            offer.num_gpus.unwrap_or(1)
        ),
        cost_per_hr: offer.dph_total,
        proxy_port_mapped: false,
        note: None,
    }
}

fn toml_value_to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Operator objects pass through; scalars become equality filters — the
/// shared convention for `[vast.query]` and per-call query maps.
fn wrap_eq(value: serde_json::Value) -> serde_json::Value {
    if value.is_object() {
        value
    } else {
        json!({ "eq": value })
    }
}

fn fmt_f64(v: Option<f64>, decimals: usize) -> String {
    v.map_or("?".to_string(), |v| format!("{v:.decimals$}"))
}

/// Whether an image reference names its registry (`docker.io/vastai/kvm:...`).
/// True when the first path component looks like a host (contains `.` or `:`,
/// or is `localhost` — mirroring Docker's own reference parsing).
fn image_registry_qualified(image: &str) -> bool {
    image
        .split('/')
        .next()
        .is_some_and(|first| first.contains('.') || first.contains(':') || first == "localhost")
}

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see [`super::validate_config`]).
pub(crate) fn capabilities(vast: &VastConfig) -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Unreliable,
        metered: true,
        provision_timeout: Some(vast.provision_timeout()),
        // vast registers authorized keys account-wide and bakes every
        // account key into new instances — use the stable plugin key.
        account_ssh_keys: true,
    }
}

/// Shell loop that waits for the onstart completion marker in 5s steps,
/// bounded by `onstart-timeout-mins`. The skip decision is made ONCE, from
/// the uptime at wait start: a machine already old when the wait begins has
/// its onstart long settled and the marker may structurally never appear
/// (reconnects to long-running machines, pre-marker instances) — but a
/// fresh machine must get the full configured wait, never a silent mid-wait
/// skip that would mask a genuine onstart timeout. The threshold tracks the
/// configured window so a raised `onstart-timeout-mins` isn't capped at the
/// old 30-minute default.
fn wait_onstart_script(onstart_mins: u64) -> String {
    let skip_uptime_secs = 1800.max(onstart_mins.saturating_mul(60));
    format!(
        "if [ \"$(cut -d. -f1 /proc/uptime)\" -ge {skip_uptime_secs} ]; then echo rk-skip; else \
         i=0; s=rk-timeout; while [ \"$i\" -lt {} ]; do \
         if [ -f /var/tmp/rk_onstart_done ]; then s=rk-done; break; fi; \
         sleep 5; i=$((i+1)); done; echo \"$s\"; fi",
        onstart_mins.saturating_mul(12) // 5s steps
    )
}

impl Runtime for VastRuntime {
    type Conn = VastConnection;

    fn name(&self) -> &'static str {
        "vast"
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(&self.vast)
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        // Both get embedded single-quoted in the onstart script. The key is
        // plugin-generated (can't contain quotes today), but validate anyway —
        // the onstart script is the one place a quote would become code.
        crate::ssh_exec::validate_shell_safe("ssh_public_key", &req.ssh_public_key)?;
        self.validate_ssh_user()?;

        // Account-level key registration BEFORE creation is load-bearing:
        // vast auto-attaches account keys to the instance at create time, and
        // the SSH proxy (sshN.vast.ai) only honors create-time attached keys
        // reliably (observed live 2026-07: keys attached after create show in
        // the API but the proxy keeps rejecting them). VMs additionally
        // require it — the create API rejects vm=true without an account key
        // (`no_ssh_key_for_vm`). The key is the plugin's stable keypair
        // (`Capabilities::account_ssh_keys`), so this registers exactly one
        // key ever; `ensure_ssh_key` is a no-op once it exists.
        if let Err(e) = self.client.ensure_ssh_key(&req.ssh_public_key).await {
            if self.vast.vm {
                anyhow::bail!(
                    "VM instances require an SSH key registered on the vast.ai account \
                     before creation, and this API key can't manage keys ({e}). If the \
                     account has 2FA enabled, disable it (cloud.vast.ai → Account → \
                     Security) and use a plain console key — the plugin does not support \
                     2FA accounts. Otherwise add any SSH key once by hand at \
                     https://cloud.vast.ai/manage-keys/ — the plugin key is still \
                     injected via the startup script."
                );
            }
            tracing::warn!(
                "vast account SSH key registration failed ({e}); onstart injection \
                 still covers direct-port hosts, but proxy-SSH hosts will reject \
                 the connection"
            );
        }

        let mut image = req.image.clone().unwrap_or_else(|| self.vast.image.clone());
        if self.vast.vm && !image_registry_qualified(&image) {
            // Load-bearing, observed live 2026-07: with vm=true and an image
            // that is not registry-qualified, vast SILENTLY provisions a
            // container running the image instead of a KVM VM (the create
            // API accepts the vm flag either way). The container fallback is
            // useless for the VM use case — Docker-in-Docker is banned.
            image = format!("docker.io/{image}");
            tracing::info!(%image, "registry-qualified the VM image (unqualified images silently create containers)");
        }

        let candidates = self.offer_candidates(req).await?;

        let label = format!("{}-{}", self.name_prefix, req.name);
        let create = CreateInstanceRequest {
            image: image.clone(),
            disk: self.vast.disk_gb,
            runtype: "ssh".to_string(),
            label: Some(label.clone()),
            env: crate::vast::types::docker_env_flags(&req.env)?,
            onstart: Some(self.onstart_script(&req.ssh_public_key)),
            vm: self.vast.vm.then_some(true),
            template_hash_id: self.vast.template_hash.clone(),
            extra: self
                .vast
                .extra
                .iter()
                .map(|(k, v)| (k.clone(), toml_value_to_json(v)))
                .collect(),
        };

        // Tried in order; an offer can be rented out between search/pick and
        // accept. Auth/permission errors fail fast — no offer will fix those.
        let mut last_err = None;
        for offer in &candidates {
            tracing::info!(
                offer_id = offer.id,
                gpu = offer.gpu_name.as_deref().unwrap_or("?"),
                dph = offer.dph_total.unwrap_or(0.0),
                "Trying vast.ai offer..."
            );
            match self.client.create_instance(offer.id, &create).await {
                Ok(contract) => {
                    tracing::info!(instance_id = contract, "vast.ai instance created");
                    // Best-effort belt-and-braces: attach the key to this
                    // instance too. Observed to race the instance's own
                    // registration (a create-time attach can be dropped), so
                    // the account-level registration above remains the
                    // load-bearing mechanism; this occasionally helps and
                    // never hurts.
                    if let Err(e) = self
                        .client
                        .attach_ssh_key(contract, &req.ssh_public_key)
                        .await
                    {
                        tracing::warn!("attaching SSH key to vast instance failed: {e}");
                    }
                    return Ok(handle_for_offer(contract, offer));
                }
                Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                    return Err(e);
                }
                Err(e) => {
                    // A transport-level failure (timeout, lost response) is
                    // ambiguous: vast may have created the instance and we
                    // never saw the contract id. Reconcile by our unique
                    // label before trying another offer — otherwise a paid
                    // machine could exist with no record anywhere.
                    if e.downcast_ref::<crate::vast::client::ApiStatusError>()
                        .is_none()
                        && let Ok(Some(orphan)) = self.client.find_instance_by_label(&label).await
                    {
                        tracing::warn!(
                            instance_id = orphan,
                            "create response was lost but the instance exists — adopting it"
                        );
                        return Ok(handle_for_offer(orphan, offer));
                    }
                    tracing::info!(offer_id = offer.id, "Offer failed, trying next: {e}");
                    last_err = Some(e);
                }
            }
        }
        if req.vast_offers.is_some() {
            let detail = last_err.map_or(String::new(), |e| format!(" Last error: {e}"));
            anyhow::bail!(
                "None of the {} offers in vast_offers could be rented — offers churn quickly \
                 and these may have been taken. Call search_vast_offers() again and pass a \
                 fresh shortlist.{detail}",
                candidates.len()
            );
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all offers failed")))
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let id: i64 = external_id.parse()?;
        let instance = self
            .client
            .get_instance(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("vast.ai instance {external_id} not found"))?;
        Ok(InstanceHandle {
            external_id: external_id.to_string(),
            gpu_name: format!(
                "{} x{}",
                instance.gpu_name.as_deref().unwrap_or("unknown"),
                instance.num_gpus.unwrap_or(1)
            ),
            cost_per_hr: instance.dph_total,
            note: None,
            // RunPod-only concept; vast Jupyter is tunnel-only by design.
            proxy_port_mapped: false,
        })
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        let id: i64 = external_id.parse()?;
        // Query failures must not become hard errors here — callers treat
        // describe() failures as machine problems (the background finalizer
        // would terminate a healthy machine over a rate limit or a local
        // network blip). Only definitive auth failures propagate; everything
        // else degrades to Unknown, which keeps the record and keeps polling
        // (the provision timeout bounds total patience).
        let instance = match self.client.get_instance(id).await {
            Ok(i) => i,
            Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => return Err(e),
            Err(e) => {
                return Ok(InstanceStatus::Unknown(format!("query failed: {e}")));
            }
        };
        let Some(instance) = instance else {
            return Ok(InstanceStatus::Gone);
        };
        Ok(match instance.actual_status.as_deref() {
            Some("running") => InstanceStatus::Running,
            Some("exited" | "stopped") => InstanceStatus::Stopped,
            // Note: "scheduling" after a stop/resume can hang forever (the
            // GPU may be rented out) — it still maps to Provisioning, the
            // wait path surfaces StillProvisioning rather than terminating.
            Some("created" | "loading" | "connecting" | "scheduling") | None => {
                InstanceStatus::Provisioning
            }
            Some(other) => InstanceStatus::Unknown(format!(
                "{other}{}",
                instance
                    .status_msg
                    .as_deref()
                    .map(|m| format!(" — {m}"))
                    .unwrap_or_default()
            )),
        })
    }

    /// Poll until running (up to 5 minutes — image pulls dominate). Transient
    /// non-running statuses within the window are tolerated (a flaky read or a
    /// container restart during onstart must not destroy the machine); only a
    /// definitive `Gone` fails early. At the deadline, [`StillProvisioning`]
    /// keeps the machine and continues finalization in the background.
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        loop {
            match self.describe(external_id).await? {
                InstanceStatus::Running => match self.get_handle(external_id).await {
                    Ok(handle) => return Ok(handle),
                    // A transient query failure at the moment the machine
                    // turns Running must not become a hard error (the
                    // finalizer would terminate a machine that just finished
                    // its image pull). Keep polling instead.
                    Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                        return Err(e);
                    }
                    Err(e) => {
                        tracing::warn!(external_id, "handle query failed transiently: {e}");
                    }
                },
                InstanceStatus::Gone => {
                    anyhow::bail!("vast.ai instance disappeared while starting")
                }
                other => {
                    tracing::debug!(external_id, ?other, "vast instance not running yet");
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(StillProvisioning.into());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Officially unreliable: a stopped instance stays bound to its GPU and
    /// resume can wait in "scheduling" indefinitely. Prefer terminate.
    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.set_state(id, "stopped").await
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.set_state(id, "running").await
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.destroy_instance(id).await
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<VastConnection> {
        let id: i64 = external_id.parse()?;
        crate::ssh_exec::validate_shell_safe("workdir", &self.vast.workdir)?;
        // Config can drift between provision and reconnect — a post-provision
        // edit must not be able to inject ssh options via the user string.
        self.validate_ssh_user()?;
        crate::ssh_exec::validate_shell_safe("jupyter-command", &self.vast.jupyter_command)?;

        let user = self.vast.ssh_user.clone();

        // SSH endpoint info can lag the running status briefly. A timeout
        // here is StillProvisioning — the machine is fine, just not ready;
        // it must not be torn down.
        let (ssh_host, ssh_port) = {
            let mut endpoint = None;
            for attempt in 1..=40 {
                // Transient query errors are just a skipped attempt — a hard
                // error here would make the finalizer terminate the machine
                // over a network blip. Only definitive auth failures escape.
                match self.client.get_instance(id).await {
                    Ok(Some(instance)) => {
                        if let Some(ep) = instance.ssh_endpoint(self.vast.vm) {
                            endpoint = Some(ep);
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                        return Err(e);
                    }
                    Err(e) => {
                        tracing::warn!(attempt, "instance query failed transiently: {e}");
                    }
                }
                tracing::debug!(attempt, "vast SSH endpoint not yet available");
                // Gentle cadence — this endpoint is shared with describe()
                // polling and vast rate-limits around 1 req/s per endpoint.
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            match endpoint {
                Some(ep) => ep,
                None => return Err(StillProvisioning.into()),
            }
        };
        tracing::info!(%ssh_host, ssh_port, "vast SSH endpoint resolved");

        let ssh = crate::ssh_exec::SshEndpoint {
            key_path: ctx.ssh_key_path.clone(),
            known_hosts_path: ctx.known_hosts_path.clone(),
            user,
            host: ssh_host,
            port: ssh_port,
        };

        self.wait_ssh_reachable(id, ctx, &ssh).await?;

        // SSH readiness no longer implies onstart completion (the key
        // re-assert makes SSH usable within seconds of boot), and user
        // onstart lines often install what the jupyter command runs (uv,
        // docker, envs). Block on the marker our generated onstart touches
        // last — one remote wait, not a local poll (each SSH exec is a full
        // handshake, and per-attempt timeouts would stretch the bound).
        // Machines up over 30 minutes skip the wait: their onstart is long
        // settled, and the marker may structurally never appear (instances
        // created before the marker existed; a resumed VM, whose cloud-init
        // does not re-run onstart — the marker lives in /var/tmp to survive
        // that reboot). Blocking reconnects here would also delay the
        // heartbeat restart. On timeout, warn and proceed — the jupyter
        // launch's liveness check surfaces the real error if tooling is
        // genuinely missing.
        let onstart_mins = self.vast.onstart_timeout_mins;
        let wait_onstart = wait_onstart_script(onstart_mins);
        match ssh
            .cmd(
                &wait_onstart,
                Duration::from_secs(onstart_mins.saturating_mul(60).saturating_add(100)),
            )
            .await
        {
            Ok(out) if out.contains("rk-timeout") => {
                tracing::warn!(
                    "onstart has not finished after {onstart_mins} minutes \
                     (config onstart-timeout-mins); launching jupyter anyway"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("onstart-completion wait failed ({e}); launching jupyter anyway");
            }
        }

        // Launch Jupyter (idempotent) with the token passed via environment.
        let launch = format!(
            "export REMOTE_KERNELS_JUPYTER_TOKEN='{}'; {}",
            ctx.jupyter_token,
            crate::ssh_exec::jupyter_launch_script(
                &self.vast.workdir,
                &self.vast.jupyter_command,
                JUPYTER_PORT
            )
        );
        ssh.cmd(&launch, Duration::from_secs(60)).await?;

        // Local tunnel to the machine's Jupyter port (shared SshTunnel — the
        // one tunnel implementation for every SSH runtime).
        let tunnel = crate::ssh_exec::SshTunnel::open(&ssh, JUPYTER_PORT).await?;

        Ok(VastConnection {
            jupyter: JupyterEndpoint::loopback(tunnel.local_port(), ctx.jupyter_token.clone()),
            ssh,
            workdir: self.vast.workdir.clone(),
            tunnel,
        })
    }
}

impl VastRuntime {
    /// Wait for SSH before launching Jupyter — the endpoint must be live when
    /// `open` returns (`finalize_start` builds the client from it).
    ///
    /// vast's SSH proxy answers "Permission denied" while the instance is
    /// still loading (image pull — can be many minutes), and for a while
    /// after it runs on some proxy hosts (attached-key propagation latency
    /// varies per host: ssh3 accepted within seconds of Running, ssh9 was
    /// still rejecting minutes later — observed live 2026-07). So denial is
    /// never treated as fatal here: sustained denial triggers one key
    /// re-attach per call (attach-at-create can race the instance's proxy
    /// registration; attaching to a still-loading instance is harmless), then
    /// the loop keeps waiting and returns [`StillProvisioning`] at the end
    /// for the background finalizer to retry. The runtime's
    /// `provision_timeout` is the money-safety backstop that eventually
    /// terminates a machine that never accepts us.
    async fn wait_ssh_reachable(
        &self,
        id: i64,
        ctx: &ConnectionContext,
        ssh: &crate::ssh_exec::SshEndpoint,
    ) -> anyhow::Result<()> {
        let mut denials = 0;
        for attempt in 1..=36 {
            match ssh.cmd("echo ok", Duration::from_secs(10)).await {
                Ok(_) => {
                    tracing::info!(attempt, "vast SSH is reachable");
                    return Ok(());
                }
                // A host-key pin mismatch cannot heal by retrying — don't
                // burn the 36-attempt window against it (nor re-attach keys).
                Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => return Err(e),
                Err(e) => {
                    denials += i32::from(e.to_string().contains("Permission denied"));
                    if denials == 12 {
                        tracing::warn!(
                            "vast SSH keeps rejecting our key; re-attaching it and retrying"
                        );
                        match crate::ssh::public_key_for(&ctx.ssh_key_path) {
                            Ok(pubkey) => {
                                if let Err(e) = self.client.attach_ssh_key(id, &pubkey).await {
                                    tracing::warn!("SSH key re-attach failed: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("could not re-derive public key: {e}"),
                        }
                        denials += 1; // one re-attach per pass
                    }
                    tracing::debug!(attempt, error = %e, "vast SSH not ready yet");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        Err(StillProvisioning.into())
    }
}

pub struct VastConnection {
    jupyter: JupyterEndpoint,
    ssh: crate::ssh_exec::SshEndpoint,
    workdir: String,
    /// Local Jupyter tunnel (shared implementation; health-checked and
    /// respawned on every heartbeat tick — a dead tunnel would otherwise
    /// silently strand all kernel traffic).
    tunnel: crate::ssh_exec::SshTunnel,
}

impl VastConnection {
    async fn exec_inner(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.ssh.cmd(command, timeout).await
    }
}

impl Connection for VastConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn startup_note(&self) -> Option<String> {
        (self.ssh.user != "root").then(|| {
            format!(
                "Non-root ssh-user ({:?}): the on-machine self-halt (watchdog / budget \
                 deadline) needs passwordless sudo on this image; without it, only the \
                 server-side supervision can stop this machine.",
                self.ssh.user
            )
        })
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.exec_inner(command, timeout).await
    }

    /// SSH reachability was already established in `open()`.
    async fn wait_reachable(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        crate::sync::sync_to_pod(project_dir, &self.ssh, &self.workdir, extra_includes).await
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        crate::sync::download_from_pod(&self.ssh, remote_path, local_path, &self.workdir).await
    }

    /// Best-effort self-cleanup: there is no credential-free way to destroy a
    /// vast instance from inside it, so the watchdog halts the machine
    /// (VMs: `shutdown`; containers: kill PID 1 → instance exits). Storage
    /// billing continues until the server or user destroys it — the
    /// server-side supervision is the primary enforcement.
    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        if policy.cleanup == crate::config::Cleanup::Disabled {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        }
        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }
        let script = crate::ssh_exec::watchdog_script(
            &VastRuntime::halt_command(&self.ssh.user),
            policy.stale_secs,
        );
        self.exec_inner(&script, Duration::from_secs(10)).await?;
        tracing::info!("Watchdog installed on vast instance (halt-only — see docs)");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        self.tunnel.ensure_alive().await;
        self.exec_inner("touch /tmp/heartbeat", Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        self.exec_inner(
            &format!("echo $(($(date +%s) + {secs_from_now})) > /tmp/budget_deadline"),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};

    use serde_json::json;

    use super::{OfferQueryOverrides, Runtime, VastRuntime, image_registry_qualified};
    use crate::config::Config;
    use crate::vast::client::VastClient;

    /// Config money-windows resolve correctly and reach the onstart guard.
    #[test]
    fn provision_timeout_and_orphan_window_from_config() {
        use std::time::Duration;
        let caps = |toml: &str| {
            let config: Config = toml::from_str(toml).unwrap();
            super::capabilities(&config.vast.clone().unwrap_or_default())
        };
        // Defaults: 20 min containers, 35 min VMs (disk image pull + kernel boot).
        assert_eq!(
            caps("").provision_timeout,
            Some(Duration::from_secs(20 * 60))
        );
        assert_eq!(
            caps("[vast]\nvm = true").provision_timeout,
            Some(Duration::from_secs(35 * 60))
        );
        // An explicit provision-timeout-mins wins over the vm auto-bump.
        assert_eq!(
            caps("[vast]\nvm = true\nprovision-timeout-mins = 50").provision_timeout,
            Some(Duration::from_secs(50 * 60))
        );

        // orphan-halt-mins reaches the onstart guard script.
        let config: Config = toml::from_str("orphan-halt-mins = 10").unwrap();
        let rt = VastRuntime::new("test-key".to_string(), &config);
        let script = rt.onstart_script("ssh-ed25519 AAAATEST test");
        assert!(script.contains("sleep 600"), "{script}");
    }

    #[test]
    fn image_qualification() {
        // Unqualified VM images get docker.io/ prepended by provision().
        assert!(!image_registry_qualified("vastai/kvm:ubuntu_terminal"));
        assert!(!image_registry_qualified(
            "vastai/base-image:@vastai-automatic-tag"
        ));
        assert!(image_registry_qualified(
            "docker.io/vastai/kvm:ubuntu_terminal"
        ));
        assert!(image_registry_qualified("nvcr.io/nvidia/pytorch:24.01-py3"));
        assert!(image_registry_qualified("localhost:5000/x/y"));
    }

    fn runtime_from(config_toml: &str) -> VastRuntime {
        let config: Config = toml::from_str(config_toml).unwrap();
        VastRuntime::new("test-key".to_string(), &config)
    }

    #[test]
    fn onstart_injects_key_into_ssh_users_home() {
        let key = "ssh-ed25519 AAAATEST test";

        // Default root: root's home, root ownership (previous behavior).
        let script = runtime_from("").onstart_script(key);
        assert!(script.contains("'/root/.ssh/authorized_keys'"), "{script}");
        assert!(script.contains("chown -R root:root '/root/.ssh'"));
        assert!(!script.contains("/home/"));

        // Non-root ssh-user: ITS home and ownership — injecting into root's
        // home would silently lock the configured user out.
        let script = runtime_from("[vast]\nssh-user = \"ubuntu\"").onstart_script(key);
        assert!(
            script.contains("'/home/ubuntu/.ssh/authorized_keys'"),
            "{script}"
        );
        assert!(script.contains("chown -R ubuntu:ubuntu '/home/ubuntu/.ssh'"));
        assert!(script.contains("mkdir -p '/home/ubuntu/.ssh'"));
        assert!(!script.contains("/root/.ssh"));
    }

    #[test]
    fn filter_precedence_baseline_config_query_per_call() {
        let rt = runtime_from(
            r#"
            [vast]
            gpu-name = ["RTX 3090"]
            max-dph = 0.5

            [vast.query]
            reliability = { gte = 0.8 }
            geolocation = "US"
            "#,
        );

        // Baselines present unless overridden; [vast.query] wins over them.
        let f = rt.offer_filters(None, &OfferQueryOverrides::default());
        assert_eq!(f["verified"], json!({"eq": true}));
        assert_eq!(f["reliability"], json!({"gte": 0.8})); // config beat baseline
        assert_eq!(f["num_gpus"], json!({"gte": 1}));
        assert_eq!(f["inet_down"], json!({"gte": 200.0}));
        assert_eq!(f["geolocation"], json!({"eq": "US"})); // scalar wrapped as eq
        assert_eq!(f["gpu_name"], json!({"in": ["RTX 3090"]}));
        assert_eq!(f["dph_total"], json!({"lte": 0.5}));
        assert!(!f.contains_key("vms_enabled"));

        // start(gpu_type=...) beats config gpu-name.
        let f = rt.offer_filters(Some("H100 SXM"), &OfferQueryOverrides::default());
        assert_eq!(f["gpu_name"], json!({"in": ["H100 SXM"]}));

        // Per-call overrides beat everything, including [vast.query] and the
        // gpu_type override.
        let overrides = OfferQueryOverrides {
            gpu_name: Some(vec!["RTX 4090".to_string()]),
            num_gpus: Some(2),
            max_dph: Some(1.5),
            vm: Some(true),
            min_disk_gb: Some(200.0),
            limit: None,
            query: Some(HashMap::from([
                ("reliability".to_string(), json!({"gte": 0.99})),
                ("static_ip".to_string(), json!(true)),
            ])),
        };
        let f = rt.offer_filters(Some("H100 SXM"), &overrides);
        assert_eq!(f["gpu_name"], json!({"in": ["RTX 4090"]}));
        assert_eq!(f["num_gpus"], json!({"eq": 2}));
        assert_eq!(f["dph_total"], json!({"lte": 1.5}));
        assert_eq!(f["disk_space"], json!({"gte": 200.0}));
        assert_eq!(f["reliability"], json!({"gte": 0.99}));
        assert_eq!(f["static_ip"], json!({"eq": true})); // scalar wrapped
        // vm=true (per-call) forces the VM connectivity constraints.
        assert_eq!(f["vms_enabled"], json!({"eq": true}));
        assert_eq!(f["direct_port_count"], json!({"gte": 1}));
    }

    /// Same-key conflicts across layers (the codex-flagged cases): per-call
    /// typed overrides must beat `[vast.query]`, and explicit `[vast.query]`
    /// entries must beat the vm-derived constraints.
    #[test]
    fn same_key_conflicts_resolve_by_layer() {
        let rt = runtime_from(
            r#"
            [vast]
            vm = true
            max-dph = 0.5

            [vast.query]
            dph_total = { lte = 5.0 }
            gpu_name = { in = ["A100 PCIE"] }
            direct_port_count = { gte = 4 }
            "#,
        );

        // [vast.query] beats config typed fields and vm-derived constraints.
        let f = rt.offer_filters(None, &OfferQueryOverrides::default());
        assert_eq!(f["dph_total"], json!({"lte": 5.0}));
        assert_eq!(f["gpu_name"], json!({"in": ["A100 PCIE"]}));
        assert_eq!(f["direct_port_count"], json!({"gte": 4}));

        // Per-call typed overrides beat [vast.query] on the same keys.
        let f = rt.offer_filters(
            None,
            &OfferQueryOverrides {
                max_dph: Some(1.0),
                gpu_name: Some(vec!["RTX 4090".to_string()]),
                ..Default::default()
            },
        );
        assert_eq!(f["dph_total"], json!({"lte": 1.0}));
        assert_eq!(f["gpu_name"], json!({"in": ["RTX 4090"]}));

        // An explicit empty per-call gpu list clears the filter entirely.
        let f = rt.offer_filters(
            Some("H100 SXM"),
            &OfferQueryOverrides {
                gpu_name: Some(vec![]),
                ..Default::default()
            },
        );
        assert!(!f.contains_key("gpu_name"));
    }

    #[test]
    fn per_call_vm_false_beats_config_vm_true() {
        let rt = runtime_from("[vast]\nvm = true");
        let f = rt.offer_filters(None, &OfferQueryOverrides::default());
        assert!(f.contains_key("vms_enabled"));
        let f = rt.offer_filters(
            None,
            &OfferQueryOverrides {
                vm: Some(false),
                ..Default::default()
            },
        );
        assert!(!f.contains_key("vms_enabled"));
        assert!(!f.contains_key("direct_port_count"));
    }

    /// Minimal HTTP/1.1 responder for exercising the provision loop against
    /// canned vast API responses. Closes each connection after one response
    /// (`Connection: close`) so reqwest never reuses a socket.
    struct FakeVast {
        base_url: String,
        requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeVast {
        /// `routes`: (method-and-path prefix, status, body).
        fn spawn(routes: Vec<(&'static str, u16, String)>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = std::sync::Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    // Read headers.
                    let header_end = loop {
                        use std::io::Read;
                        let Ok(n) = stream.read(&mut tmp) else {
                            break 0;
                        };
                        if n == 0 {
                            break 0;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    if header_end == 0 {
                        continue;
                    }
                    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    // Drain the body so the write isn't racing the request.
                    let content_length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < header_end + content_length {
                        let Ok(n) = stream.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    seen.lock().unwrap().push(request_line.clone());
                    let (status, body) = routes
                        .iter()
                        .find(|(prefix, _, _)| request_line.starts_with(prefix))
                        .map_or((404, "{}".to_string()), |(_, s, b)| (*s, b.clone()));
                    let reason = if status < 400 { "OK" } else { "ERR" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            Self { base_url, requests }
        }

        fn asks_tried(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.contains("/api/v0/asks/"))
                .cloned()
                .collect()
        }
    }

    fn shortlist_request(offers: Vec<i64>) -> super::ProvisionRequest {
        super::ProvisionRequest {
            name: "main".to_string(),
            gpu_type: None,
            image: None,
            vast_offers: Some(offers),
            priority: None,
            cleanup: crate::config::Cleanup::Terminate,
            env: HashMap::new(),
            ssh_public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITESTKEY test".to_string(),
            jupyter_token: "tok".to_string(),
        }
    }

    /// Bundles response resolving ids 111 (cheap) and 222 (pricier). Returned
    /// cheapest-first, i.e. the OPPOSITE of the ranked order the tests pass —
    /// order preservation is what's under test.
    const RESOLVE_BODY: &str = r#"{"offers": [
        {"id": 111, "gpu_name": "RTX 3090", "num_gpus": 1, "dph_total": 0.30},
        {"id": 222, "gpu_name": "RTX 4090", "num_gpus": 2, "dph_total": 0.51}
    ]}"#;

    #[tokio::test]
    async fn shortlist_falls_through_taken_offers_in_ranked_order() {
        let fake = FakeVast::spawn(vec![
            ("GET /api/v0/ssh/", 200, "[]".to_string()),
            ("POST /api/v0/ssh/", 200, "{}".to_string()),
            ("POST /api/v0/bundles/", 200, RESOLVE_BODY.to_string()),
            // Ranked-first offer 222 was rented out in the meantime; the
            // runner-up 111 succeeds.
            (
                "PUT /api/v0/asks/222/",
                400,
                r#"{"success": false, "error": "no_such_ask"}"#.to_string(),
            ),
            (
                "PUT /api/v0/asks/111/",
                200,
                r#"{"success": true, "new_contract": 999}"#.to_string(),
            ),
            ("POST /api/v0/instances/999/ssh/", 200, "{}".to_string()),
        ]);
        let config: Config = toml::from_str("").unwrap();
        let rt = VastRuntime::new_with_client(
            VastClient::new_with_base_url("k".to_string(), fake.base_url.clone()),
            &config,
        );

        let handle = rt
            .provision(&shortlist_request(vec![222, 111]))
            .await
            .unwrap();
        assert_eq!(handle.external_id, "999");
        // Metadata comes from the RESOLVED offer, not a post-create query.
        assert_eq!(handle.gpu_name, "RTX 3090 x1");
        assert_eq!(handle.cost_per_hr, Some(0.30));
        // Claude's ranked order respected (NOT the API's cheapest-first
        // order): 222 attempted before 111.
        let asks = fake.asks_tried();
        assert_eq!(asks.len(), 2, "{asks:?}");
        assert!(asks[0].contains("/asks/222/"), "{asks:?}");
        assert!(asks[1].contains("/asks/111/"), "{asks:?}");
        // Exactly one bundles call: the resolve — no auto-search fallback.
        let bundles = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.contains("/bundles/"))
            .count();
        assert_eq!(bundles, 1);
    }

    #[tokio::test]
    async fn shortlist_enforces_max_dph_and_skips_stale_ids_before_renting() {
        let fake = FakeVast::spawn(vec![
            ("GET /api/v0/ssh/", 200, "[]".to_string()),
            ("POST /api/v0/ssh/", 200, "{}".to_string()),
            // Only 222 still exists — and it violates the price ceiling.
            (
                "POST /api/v0/bundles/",
                200,
                r#"{"offers": [{"id": 222, "gpu_name": "RTX 4090", "num_gpus": 2, "dph_total": 0.51}]}"#
                    .to_string(),
            ),
        ]);
        let config: Config = toml::from_str("[vast]\nmax-dph = 0.4").unwrap();
        let rt = VastRuntime::new_with_client(
            VastClient::new_with_base_url("k".to_string(), fake.base_url.clone()),
            &config,
        );

        let err = rt
            .provision(&shortlist_request(vec![111, 222]))
            .await
            .unwrap_err()
            .to_string();
        // Fail closed: nothing may be rented when every id is stale or
        // over the configured ceiling — and the reasons are spelled out.
        assert_eq!(fake.asks_tried().len(), 0, "{err}");
        assert!(err.contains("not rentable"), "{err}");
        assert!(err.contains("exceeds max-dph"), "{err}");
        assert!(err.contains("search_vast_offers"), "{err}");
    }

    #[tokio::test]
    async fn shortlist_exhaustion_says_re_search() {
        let fake = FakeVast::spawn(vec![
            ("GET /api/v0/ssh/", 200, "[]".to_string()),
            ("POST /api/v0/ssh/", 200, "{}".to_string()),
            ("POST /api/v0/bundles/", 200, RESOLVE_BODY.to_string()),
            (
                "PUT /api/v0/asks/",
                400,
                r#"{"success": false, "error": "no_such_ask"}"#.to_string(),
            ),
            (
                "GET /api/v1/instances/",
                200,
                r#"{"instances": []}"#.to_string(),
            ),
        ]);
        let config: Config = toml::from_str("").unwrap();
        let rt = VastRuntime::new_with_client(
            VastClient::new_with_base_url("k".to_string(), fake.base_url.clone()),
            &config,
        );

        let err = rt
            .provision(&shortlist_request(vec![111, 222]))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("search_vast_offers"), "{err}");
        assert_eq!(fake.asks_tried().len(), 2);
    }

    #[tokio::test]
    async fn shortlist_auth_error_fails_fast() {
        let fake = FakeVast::spawn(vec![
            ("GET /api/v0/ssh/", 200, "[]".to_string()),
            ("POST /api/v0/ssh/", 200, "{}".to_string()),
            ("POST /api/v0/bundles/", 200, RESOLVE_BODY.to_string()),
            (
                "PUT /api/v0/asks/",
                401,
                r#"{"error": "Two Factor Authentication required"}"#.to_string(),
            ),
        ]);
        let config: Config = toml::from_str("").unwrap();
        let rt = VastRuntime::new_with_client(
            VastClient::new_with_base_url("k".to_string(), fake.base_url.clone()),
            &config,
        );

        let err = rt
            .provision(&shortlist_request(vec![111, 222]))
            .await
            .unwrap_err()
            .to_string();
        // Fail-fast: the second offer must not be attempted on an auth error,
        // and the error carries the 2FA guidance.
        assert_eq!(fake.asks_tried().len(), 1);
        assert!(err.contains("disable"), "{err}");
    }

    /// A 404 on DELETE means "already gone" (success) — EXCEPT vast's
    /// session-expired auth 404, which must stay an error: treating it as
    /// gone would falsely confirm termination of a machine that keeps
    /// billing.
    #[tokio::test]
    async fn destroy_404_gone_ok_but_session_expired_errors() {
        let fake = FakeVast::spawn(vec![
            (
                "DELETE /api/v0/instances/5/",
                404,
                r#"{"error": "auth_error", "msg": "Session expired. Please log in again."}"#
                    .to_string(),
            ),
            (
                "DELETE /api/v0/instances/6/",
                404,
                r#"{"error": "no_such_instance"}"#.to_string(),
            ),
        ]);
        let client = VastClient::new_with_base_url("k".to_string(), fake.base_url.clone());
        let err = client.destroy_instance(5).await.unwrap_err().to_string();
        assert!(err.contains("Session expired"), "{err}");
        assert!(err.contains("disable"), "guidance missing: {err}");
        client.destroy_instance(6).await.unwrap();
    }
}
