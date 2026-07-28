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
    /// Run `CLIProxyAPI`'s interactive Codex OAuth login for the managed upstream.
    Login,
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
        Command::Login => login(&dirs, &config_path).await,
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

async fn login(dirs: &Dirs, config_path: &std::path::Path) -> anyhow::Result<()> {
    let config = Config::load(config_path)?;
    let upstream = config.cliproxy_upstream().clone();
    anyhow::ensure!(
        upstream.mode == UpstreamMode::Managed,
        "`model-router login` manages Codex auth for managed mode; [upstreams.cliproxy] is {:?}",
        upstream.mode
    );
    let binary = acquire::ensure_upstream(dirs).await?;
    let paths = supervisor::prepare_managed_state(dirs, &upstream, &config.openai_providers)?;
    if let Some(existing) = model_router::state::find_codex_auth(&paths.auth_dir) {
        println!(
            "A Codex login already exists at {}; continuing will add another.",
            existing.display()
        );
    }
    println!("Opening the Codex OAuth flow (browser + loopback callback on port 1455)...");
    let status = std::process::Command::new(&binary)
        .arg("-config")
        .arg(&paths.upstream_config)
        .arg("-codex-login")
        .status()
        .with_context(|| format!("failed to run {}", binary.display()))?;
    anyhow::ensure!(status.success(), "codex login exited with {status}");
    match model_router::state::find_codex_auth(&paths.auth_dir) {
        Some(auth) => {
            println!("Login stored at {}", auth.display());
            Ok(())
        }
        None => anyhow::bail!(
            "login finished but no codex-*.json appeared in {}",
            paths.auth_dir.display()
        ),
    }
}
