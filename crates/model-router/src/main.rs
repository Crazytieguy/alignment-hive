#![warn(clippy::pedantic)]

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use model_router::config::{Config, UpstreamMode};
use model_router::state::{Dirs, InstanceLock};
use model_router::supervisor::Supervisor;
use model_router::{acquire, doctor, proxy, service, supervisor};

#[derive(Debug, Parser)]
#[command(
    name = "model-router",
    about = "Loopback Anthropic-format model routing gateway"
)]
struct Cli {
    /// TOML configuration file (default: ~/.config/model-router/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
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
    /// Exit successfully; `bootstrap.sh prefetch` normally stops before exec,
    /// but an older bootstrap passes the argument through to the binary and
    /// this keeps that combination a successful no-op.
    #[command(hide = true)]
    Prefetch,
    /// Install and manage the OS user service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
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
}

/// Everything that differs between the OAuth flows. Adding a provider is one
/// more descriptor — `CLIProxyAPI` 7.2.110 also ships `-claude-login`,
/// `-kimi-login`, and `-antigravity-login` — never a second copy of the
/// login routine.
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
                auth_prefix: model_router::state::CODEX_AUTH_PREFIX,
                // Codex is the default family; nothing gates it.
                is_enabled: |_| true,
                start_notice: Some(
                    "Opening the Codex OAuth flow (browser + loopback callback on port 1455)...",
                ),
            },
            Self::Grok => &LoginDescriptor {
                label: "Grok",
                flag: "-xai-login",
                auth_prefix: model_router::state::GROK_AUTH_PREFIX,
                is_enabled: |config| config.grok.enabled,
                // The xAI device flow prints its own verification URL and
                // code; nothing to add.
                start_notice: None,
            },
        }
    }
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Populate the stable launcher, install the unit, and start the service.
    Install {
        /// Plugin root containing scripts/bootstrap.sh and binary-version.
        #[arg(long)]
        plugin_root: Option<PathBuf>,
    },
    /// Stop the service and remove its unit, retaining all router data.
    Uninstall,
    /// Summarize service, unit-file, and launcher-version status.
    Status,
    /// Restart the installed service.
    Restart,
    /// Refresh the launcher from the plugin and restart the service.
    Refresh {
        /// Plugin root containing scripts/bootstrap.sh and binary-version.
        #[arg(long)]
        plugin_root: Option<PathBuf>,
    },
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
        Command::Prefetch => Ok(()),
        Command::Login { provider } => login(&dirs, &config_path, provider).await,
        Command::VerifyProviders { name, json } => {
            let reports = model_router::verify::run(&config_path, name.as_deref()).await?;
            let (rendered, all_ok) = model_router::verify::render(&reports);
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
            ServiceCommand::Install { plugin_root } => {
                service::install(&dirs, plugin_root.as_deref())
            }
            ServiceCommand::Uninstall => service::uninstall(),
            ServiceCommand::Status => service::status(&dirs),
            ServiceCommand::Restart => service::restart(),
            ServiceCommand::Refresh { plugin_root } => {
                service::refresh(&dirs, plugin_root.as_deref())
            }
        },
    }
}

async fn serve(dirs: &Dirs, config_path: &std::path::Path) -> anyhow::Result<()> {
    let mut config = Config::load(config_path)?;
    if config.ingress_token.is_none() {
        config.ingress_token = Some(model_router::state::load_or_create_ingress_token(dirs)?);
    }
    // The OS service runs with an arbitrary working directory (launchd: /),
    // so a relative capture path must not depend on cwd.
    if config.capture.enabled && config.capture.file.is_relative() {
        config.capture.file = dirs.state_dir.join(&config.capture.file);
    }
    // Ask the hosts for the windows of any route that opted into scaling
    // without naming one. Best-effort: an unreachable host leaves the route
    // unscaled rather than blocking Claude traffic.
    model_router::discovery::fill_context_windows(&mut config, dirs).await;
    config.prepare()?;
    let _lock = InstanceLock::acquire(dirs)?;

    // Bind the router port BEFORE spawning the managed child so a taken port
    // fails fast without leaving a child behind.
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
    if let Some(token) = &config.ingress_token {
        tracing::info!(
            "gateway base URL (for ANTHROPIC_BASE_URL): {}",
            proxy::tokened_base_url(&address, token)
        );
    }

    let managed_config = Some(config.cliproxy_upstream().clone())
        .filter(|upstream| upstream.mode == UpstreamMode::Managed);
    // A failing cliproxy upstream must never take down Claude routing: on
    // supervisor-start failure we serve degraded (GPT requests get 502)
    // instead of exiting.
    let (handle, supervisor) = match managed_config {
        Some(upstream) => {
            match Supervisor::start(
                dirs,
                &upstream,
                config.openai_providers.clone(),
                model_router::supervisor::Tuning::default(),
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
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installs");
        tokio::select! {
            () = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
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
        "{} routing is off; enable it in {} before logging in",
        descriptor.label,
        config_path.display()
    );
    let binary = acquire::ensure_upstream(dirs).await?;
    let paths = supervisor::prepare_managed_state(dirs, &upstream, &config.openai_providers)?;
    if let Some(existing) = model_router::state::find_auth(&paths.auth_dir, descriptor.auth_prefix)
    {
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
    // reporting success. One enumeration serves both the check and the
    // report.
    model_router::state::harden_auth_files(&paths.auth_dir)?;
    match model_router::state::auth_files(&paths.auth_dir, descriptor.auth_prefix)?.first() {
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
