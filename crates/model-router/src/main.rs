//! The `model-router` binary: CLI parsing and the glue from each subcommand
//! to the library. `serve` owns the startup order: bind the port first so a
//! taken one fails fast, then discover provider windows, then start the
//! managed child, serving Claude traffic even when the child cannot start.

#![warn(clippy::pedantic)]

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use model_router::config::{Config, GptAuth, UpstreamMode};
use model_router::{
    acquire, discovery, doctor, muse, proxy, service, settings_edit, state, supervisor, verify,
};
use state::{Dirs, InstanceLock};
use supervisor::Supervisor;

#[derive(Parser)]
#[command(
    name = "model-router",
    about = "Loopback Anthropic-format model routing gateway"
)]
struct Cli {
    /// TOML configuration file (default: model-router/config.toml under the
    /// XDG config dir, ~/.config).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the routing gateway (the default command).
    Serve,
    /// Print a commented TOML config template to stdout.
    ConfigTemplate,
    /// Download and verify the pinned `CLIProxyAPI` release (no-op when cached).
    EnsureUpstream,
    /// Run `CLIProxyAPI`'s interactive OAuth login for the managed upstream.
    Login {
        /// Which provider to log in to.
        #[arg(value_enum, default_value_t = LoginProvider::Codex)]
        provider: LoginProvider,
    },
    /// Check configured openai-providers against their /models endpoints
    /// (never prints API keys).
    VerifyProviders {
        /// Verify only this provider.
        #[arg(long)]
        name: Option<String>,
        /// Emit the reports as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Diagnose config, binaries, auth, and the running router.
    Doctor {
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install and manage the OS user service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Print the Proxy-Authorization value for Claude Code's
    /// `proxyAuthHelper` (the HTTPS-proxy front).
    ProxyAuthorization,
    /// Trust the HTTPS-proxy CA in the login keychain (macOS; asks for your
    /// password), creating it first if needed.
    TrustCa {
        /// Remove the trust and the certificate from the keychain instead.
        #[arg(long)]
        remove: bool,
    },
    /// Edit Claude Code's settings files for the router; prints one JSON
    /// object.
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Bring the /model picker in ~/.claude/settings.json up to the shipped
    /// GPT routes (a report, unless --apply).
    Models {
        /// Add optional Fast twins after their existing base rows instead.
        #[arg(long)]
        fast: bool,
        /// Make the change rather than report it.
        #[arg(long)]
        apply: bool,
        /// The project whose settings files decide the wiring (default: cwd).
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// Bring Claude Code's context windows for the routes up to setup's:
    /// a 1M declaration and a per-model compaction window for each route
    /// (a report, unless --apply).
    Windows {
        /// Make the change rather than report it.
        #[arg(long)]
        apply: bool,
        /// The project whose settings files decide the wiring (default: cwd).
        #[arg(long)]
        project_dir: Option<PathBuf>,
        /// The settings file to write (the user's, or the project's
        /// settings.json or settings.local.json; created if missing), rather
        /// than the one that declares the window or wires the router.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Wire Claude Code to the router as its HTTPS proxy, replacing this
    /// install's `ANTHROPIC_BASE_URL` (a report, unless --apply).
    Proxy {
        /// Make the change rather than report it.
        #[arg(long)]
        apply: bool,
        /// The project whose settings files decide the wiring (default: cwd).
        #[arg(long)]
        project_dir: Option<PathBuf>,
        /// The settings file to write, rather than the one that wires the
        /// router now (or the user's).
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Take this install's wiring (`ANTHROPIC_BASE_URL`, or the HTTPS-proxy
    /// keys), and a default model naming a routed ID, out of the project's
    /// and the user's settings files.
    Bypass {
        /// The project whose settings files to edit.
        #[arg(long)]
        project_dir: PathBuf,
    },
}

/// Which OAuth provider `login` targets. A fieldless enum because
/// `ValueEnum` requires one; the per-provider data lives in
/// [`LoginDescriptor`].
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum LoginProvider {
    #[default]
    Codex,
    Grok,
    #[value(alias = "meta")]
    Muse,
}

/// Everything that differs between the OAuth flows. Adding a provider is one
/// more descriptor, never a second copy of the login routine.
struct LoginDescriptor {
    /// Human name used in the prompts.
    label: &'static str,
    /// The child binary's login flag.
    flag: &'static str,
    /// Auth-file prefix this flow writes.
    auth_prefix: &'static str,
    /// Whether the router would actually use this provider's credential.
    /// Keeps the login routine provider-blind — no `match` on the variant.
    is_enabled: fn(&Config) -> bool,
    /// Why the login is refused when `is_enabled` is false.
    disabled: &'static str,
    /// Printed before spawning the child, only when the flow does something
    /// the child's own output doesn't explain (Codex opens a browser).
    /// `None` when the child's output speaks for itself.
    start_notice: Option<&'static str>,
}

impl LoginProvider {
    const fn descriptor(self) -> &'static LoginDescriptor {
        match self {
            Self::Codex => &LoginDescriptor {
                label: "Codex",
                flag: "-codex-login",
                auth_prefix: state::CODEX_AUTH_PREFIX,
                is_enabled: |config| config.gpt.auth == GptAuth::Subscription,
                disabled: "GPT routes use an API key ([gpt] auth = \"api-key\"), so a Codex login \
                           would sit unused; remove that setting to use the subscription",
                start_notice: Some(
                    "Opening the Codex OAuth flow (browser + loopback callback on port 1455)...",
                ),
            },
            Self::Grok => &LoginDescriptor {
                label: "Grok",
                flag: "-xai-login",
                auth_prefix: state::GROK_AUTH_PREFIX,
                is_enabled: |config| config.grok.enabled,
                disabled: "Grok routing is off; enable it before logging in",
                // The xAI device flow prints its own verification URL and
                // code; nothing to add.
                start_notice: None,
            },
            Self::Muse => &LoginDescriptor {
                label: "Muse",
                flag: "-meta-login",
                auth_prefix: state::MUSE_AUTH_PREFIX,
                is_enabled: |config| config.muse.enabled,
                disabled: "Muse routing is off; enable it before logging in",
                // A device flow like xAI's: the child prints the URL and code.
                start_notice: None,
            },
        }
    }
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Populate the stable launcher, install the unit, and start the service.
    Install,
    /// Stop the service and remove its unit, retaining all router data.
    Uninstall,
    /// Summarize service, unit-file, and launcher-version status.
    Status,
    /// Restart the installed service.
    Restart,
    /// Refresh the launcher from the plugin and restart the service.
    Refresh,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::ConfigTemplate)) {
        print!("{}", Config::template());
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "model_router=info".parse().expect("valid log filter")),
        )
        .with_writer(std::io::stderr)
        .init();

    let dirs = Dirs::resolve()?;
    let config_path = cli.config.unwrap_or_else(|| dirs.config_file());

    match cli.command.unwrap_or(Command::Serve) {
        Command::ConfigTemplate => unreachable!("handled above"),
        Command::Serve => serve(&dirs, &config_path).await,
        Command::EnsureUpstream => {
            let binary = acquire::ensure_upstream(&dirs).await?;
            println!(
                "CLIProxyAPI v{} at {}",
                acquire::UPSTREAM_VERSION,
                binary.display()
            );
            Ok(())
        }
        Command::Login { provider } => login(&dirs, &config_path, provider).await,
        Command::VerifyProviders { name, json } => {
            let reports = verify::run(&config_path, name.as_deref()).await?;
            let (rendered, all_ok) = verify::render(&reports);
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                print!("{rendered}");
            }
            if all_ok {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        Command::Doctor { json } => {
            let report = doctor::run(&dirs, &config_path).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report.render());
            }
            if report.healthy {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Command::Service { command } => match command {
            ServiceCommand::Install => service::install(&dirs),
            ServiceCommand::Uninstall => service::uninstall(),
            ServiceCommand::Status => service::status(&dirs),
            ServiceCommand::Restart => service::restart(),
            ServiceCommand::Refresh => service::refresh(&dirs),
        },
        Command::ProxyAuthorization => {
            let config = Config::load(&config_path)?;
            let token = config
                .ingress_token
                .or_else(|| state::load_ingress_token(&dirs))
                .context("no ingress token yet: start the router once")?;
            println!("{}", model_router::mitm::proxy_authorization(&token));
            Ok(())
        }
        Command::TrustCa { remove } => trust_ca(&dirs, remove),
        Command::Settings { command } => {
            let report = settings(&dirs, &config_path, command)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
    }
}

fn settings(
    dirs: &Dirs,
    config_path: &std::path::Path,
    command: SettingsCommand,
) -> anyhow::Result<serde_json::Value> {
    let home = state::home_dir();
    let config = Config::load(config_path).map_err(|error| format!("{error:#}"));
    // Orders the settings edits.
    let lock = dirs.state_dir.join("settings.lock");
    let claude_version = || {
        let output = std::process::Command::new("claude")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        settings_edit::Version::parse(&String::from_utf8_lossy(&output.stdout))
    };
    // The token the service minted, and one pinned in the config.
    let tokens = |config: &Config| -> Vec<String> {
        state::load_ingress_token(dirs)
            .into_iter()
            .chain(config.ingress_token.clone())
            .collect()
    };
    let project = |project_dir: Option<PathBuf>| match project_dir {
        Some(dir) => std::path::absolute(dir),
        None => std::env::current_dir(),
    };
    match command {
        SettingsCommand::Models {
            fast,
            apply,
            project_dir,
        } => {
            let home = home.context("HOME is not set")?;
            let project = project(project_dir)?;
            settings_edit::models(
                &home,
                &project,
                config.as_ref().map_err(String::as_str),
                if fast {
                    settings_edit::Migration::FastTwins
                } else {
                    settings_edit::Migration::Shipped
                },
                claude_version,
                apply.then_some(lock.as_path()),
            )
        }
        SettingsCommand::Windows {
            apply,
            project_dir,
            file,
        } => {
            let home = home.context("HOME is not set")?;
            let project = project(project_dir)?;
            // With the windows the service discovered, as doctor judges them.
            let config = config.and_then(|mut config| {
                discovery::apply_cached_windows(&mut config, dirs)
                    .map(|()| config)
                    .map_err(|error| format!("{error:#}"))
            });
            settings_edit::windows(
                &home,
                &project,
                config.as_ref().map_err(String::as_str),
                file.map(std::path::absolute).transpose()?.as_deref(),
                claude_version,
                apply.then_some(lock.as_path()),
            )
        }
        SettingsCommand::Proxy {
            apply,
            project_dir,
            file,
        } => {
            let home = home.context("HOME is not set")?;
            let project = project(project_dir)?;
            let config = config.map_err(|error| anyhow::anyhow!(error))?;
            let ca_cert = model_router::mitm::ca_cert_path(&model_router::mitm::ca_dir(dirs));
            let install = settings_edit::Install {
                address: std::net::SocketAddr::new(config.bind_address, config.port),
                tokens: &tokens(&config),
                launcher: &dirs.launcher_dir().join("bootstrap.sh"),
                ca_cert: &ca_cert,
                trust: settings_edit::Trust::of(&ca_cert),
            };
            settings_edit::proxy(
                &settings_edit::ProxyEnv::read(),
                &home,
                &project,
                file.map(std::path::absolute).transpose()?.as_deref(),
                &install,
                apply.then_some(lock.as_path()),
            )
        }
        SettingsCommand::Bypass { project_dir } => {
            let tokens = config.map_or_else(
                |_| state::load_ingress_token(dirs).into_iter().collect(),
                |config| tokens(&config),
            );
            settings_edit::bypass(home.as_deref(), &project_dir, &tokens, &lock)
        }
    }
}

/// `trust-ca`: the keychain trusts the HTTPS-proxy CA (created first if
/// needed), or with `remove` no longer holds it.
fn trust_ca(dirs: &Dirs, remove: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "macos"),
        "the keychain is macOS's; elsewhere `model-router settings proxy --apply` names the CA \
         in NODE_EXTRA_CA_CERTS"
    );
    let ca_dir = model_router::mitm::ca_dir(dirs);
    if remove {
        let ca_cert = model_router::mitm::ca_cert_path(&ca_dir);
        if ca_cert.exists() {
            model_router::mitm::untrust(&ca_cert)?;
        }
        println!("The keychain no longer trusts the model-router CA.");
    } else {
        let ca_cert = model_router::mitm::ensure_ca(&ca_dir)?;
        if !model_router::mitm::is_trusted(&ca_cert)? {
            model_router::mitm::trust(&ca_cert)?;
        }
        println!(
            "The keychain trusts {} for api.anthropic.com.",
            ca_cert.display()
        );
    }
    Ok(())
}

async fn serve(dirs: &Dirs, config_path: &std::path::Path) -> anyhow::Result<()> {
    let mut config = Config::load(config_path)?;
    // The deprecated `context-window-scaling` reports usage against the
    // window Claude Code was configured with; read it rather than make the
    // config restate it.
    config.read_client_window(
        state::home_dir().as_deref(),
        &std::env::current_dir().unwrap_or_default(),
    );
    let token = match config.ingress_token.take() {
        Some(token) => token,
        None => state::load_or_create_ingress_token(dirs)?,
    };
    // The OS service runs with an arbitrary working directory (launchd: /),
    // so a relative capture path must not depend on cwd.
    if config.capture.enabled && config.capture.file.is_relative() {
        config.capture.file = dirs.state_dir.join(&config.capture.file);
    }
    let _lock = InstanceLock::acquire(dirs)?;

    // Bind the router port BEFORE discovery and the managed child so a taken
    // port fails fast, without a wait on the hosts or a child left behind.
    let address = std::net::SocketAddr::new(config.bind_address, config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| {
            format!(
                "failed to bind {address} — is another model-router already running, or is the \
                 port taken? (config knob: `port`)"
            )
        })?;
    tracing::info!(%address, "model-router listening");
    tracing::info!(
        "gateway base URL (for ANTHROPIC_BASE_URL): {}",
        proxy::tokened_base_url(&address, &token)
    );
    // A CA that cannot be loaded leaves the base-URL wiring working; only
    // Claude Code's tunnels through the proxy wiring are refused.
    match model_router::mitm::Interceptor::load(&model_router::mitm::ca_dir(dirs), &token) {
        Ok(interceptor) => {
            tracing::info!(
                ca = %interceptor.ca_cert().display(),
                "HTTPS proxy (for HTTPS_PROXY): http://{address}"
            );
            config.interceptor = Some(std::sync::Arc::new(interceptor));
        }
        Err(error) => tracing::error!("HTTPS proxy unavailable: {error:#}"),
    }
    config.ingress_token = Some(token);

    // Ask the hosts for the windows of the provider routes that did not name
    // one. Best-effort: an unreachable host leaves the cached windows in
    // place, and a route with none is still served (doctor reports it) rather
    // than blocking Claude traffic. Requests arriving meanwhile wait in the accept backlog.
    discovery::fetch_context_windows(&config, dirs).await;
    discovery::apply_cached_windows(&mut config, dirs)?;
    config.warn_unscaled_routes();

    let managed_config = Some(config.cliproxy_upstream().clone())
        .filter(|upstream| upstream.mode == UpstreamMode::Managed);
    config.auth_dir = config.managed_auth_dir(dirs);
    // A failing cliproxy upstream must never take down Claude routing: on
    // supervisor-start failure we serve degraded (GPT requests get 502)
    // instead of exiting.
    let (handle, supervisor) = match managed_config {
        Some(upstream) => {
            match Supervisor::start(
                dirs,
                &upstream,
                supervisor::ChildCredentials::of(&config),
                supervisor::Tuning::default(),
            ) {
                Ok(supervisor) => (Some(supervisor.handle()), Some(supervisor)),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "failed to start the cliproxy upstream supervisor; serving Claude traffic \
                         only (GPT requests will return errors — run `model-router doctor`)"
                    );
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let result = proxy::serve_listener(listener, config, handle, shutdown_signal()).await;
    if let Some(supervisor) = supervisor {
        tracing::info!("shutting down cliproxy upstream");
        supervisor.shutdown().await;
    }
    result
}

/// Resolves when SIGINT or SIGTERM arrives (launchd/systemd stop both send
/// SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler installs");
    tokio::select! {
        () = ctrl_c => {}
        _ = sigterm.recv() => {}
    }
    tracing::info!("shutdown signal received");
}

async fn login(
    dirs: &Dirs,
    config_path: &std::path::Path,
    provider: LoginProvider,
) -> anyhow::Result<()> {
    let descriptor = provider.descriptor();
    let config = Config::load(config_path)?;
    let upstream = config.cliproxy_upstream().clone();
    anyhow::ensure!(
        upstream.mode == UpstreamMode::Managed,
        "`model-router login` manages OAuth for managed mode; [upstreams.cliproxy] is {:?}",
        upstream.mode
    );
    // Refuse a login the router would then ignore: without the family
    // enabled, no route uses the credential and it would sit unused.
    anyhow::ensure!(
        (descriptor.is_enabled)(&config),
        "{} (config: {})",
        descriptor.disabled,
        config_path.display()
    );
    let binary = acquire::ensure_upstream(dirs).await?;
    let paths = supervisor::prepare_managed_state(
        dirs,
        &upstream,
        &supervisor::ChildCredentials::of(&config),
    )?;
    let existing = state::auth_files(&paths.auth_dir, descriptor.auth_prefix)?;
    if let Some(existing) = existing.first() {
        println!(
            "A {} login already exists at {}; continuing will add another.",
            descriptor.label,
            existing.display()
        );
    }
    if let Some(notice) = descriptor.start_notice {
        println!("{notice}");
    }
    let status = std::process::Command::new(&binary)
        .arg("-config")
        .arg(&paths.upstream_config)
        .arg(descriptor.flag)
        .status()
        .with_context(|| format!("failed to run {}", binary.display()))?;
    anyhow::ensure!(
        status.success(),
        "{} login exited with {status}",
        descriptor.label
    );
    // The child writes the xAI credential world-readable, so harden before
    // reporting success. Report the file this login added; when the child
    // rewrote an existing one, the first match is the best guess.
    state::harden_auth_files(&paths.auth_dir)?;
    // A new Meta login gets the relay Contributor mode needs, or none, as
    // the service's next spawn would give it. Fatal here, unlike in `serve`
    // (`supervisor::prepare_managed_state`): this login is interactive.
    muse::sync_relay(&paths.auth_dir, config.muse.mode()).context(
        "could not bring the Meta logins in line with [muse] contributor; Muse requests are \
         refused until `model-router service restart` succeeds",
    )?;
    let files = state::auth_files(&paths.auth_dir, descriptor.auth_prefix)?;
    match files
        .iter()
        .find(|file| !existing.contains(file))
        .or(files.first())
    {
        Some(auth) => {
            println!("Login stored at {}", auth.display());
            Ok(())
        }
        None => anyhow::bail!(
            "login finished but no {}*.json appeared in {}",
            descriptor.auth_prefix,
            paths.auth_dir.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login_provider(args: &[&str]) -> LoginProvider {
        let cli = Cli::try_parse_from(args).unwrap();
        let Some(Command::Login { provider }) = cli.command else {
            panic!("not a login: {args:?}");
        };
        provider
    }

    #[test]
    fn login_muse_and_its_meta_alias_reach_the_meta_flow() {
        for args in [
            ["model-router", "login", "muse"],
            ["model-router", "login", "meta"],
        ] {
            let descriptor = login_provider(&args).descriptor();
            assert_eq!(descriptor.label, "Muse");
            assert_eq!(descriptor.flag, "-meta-login");
            assert_eq!(descriptor.auth_prefix, state::MUSE_AUTH_PREFIX);
        }
        let config = |source: &str| -> Config { toml::from_str(source).unwrap() };
        let is_enabled = LoginProvider::Muse.descriptor().is_enabled;
        assert!(!is_enabled(&config("")));
        assert!(!is_enabled(&config("[grok]\nenabled = true\n")));
        assert!(is_enabled(&config("[muse]\nenabled = true\n")));
    }
}
