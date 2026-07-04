use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use std::path::PathBuf;

use remote_kernels::{config, server, state};

#[derive(Parser)]
#[command(
    name = "remote-kernels",
    about = "MCP server for cloud GPU machines with Jupyter kernels"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Project directory (where remote-kernels.toml lives).
    #[arg(long, default_value = ".", global = true)]
    project_dir: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Print a commented TOML config template to stdout.
    ConfigTemplate,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::ConfigTemplate) => {
            print!("{}", config::Config::template());
            return Ok(());
        }
        None => serve(cli.project_dir).await,
    }
}

async fn serve(project_dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    remote_kernels::init_tls();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "remote_kernels=info,rmcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let project_dir = project_dir.canonicalize().unwrap_or(project_dir);

    // Load .env.local (then .env) from project directory if present.
    // Runtime credentials (RUNPOD_API_KEY, ...) are checked lazily at first
    // use of each runtime, not here — a runtime you never touch needs no key.
    let _ = dotenvy::from_path(project_dir.join(".env.local"));
    let _ = dotenvy::from_path(project_dir.join(".env"));

    let config = config::Config::load(&project_dir)?;

    // Budget: env var overrides config. Env var is typically set via .claude/settings.json
    // so Claude can't modify it.
    let budget = std::env::var("REMOTE_KERNELS_BUDGET")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .or(config.budget_cap);

    // Budget and cleanup:disabled are incompatible — disabled means the user wants the
    // machine to keep running, which conflicts with budget enforcement stopping/terminating it.
    if budget.is_some() && config.cleanup == config::Cleanup::Disabled {
        return Err(
            "Configuration error: budget-cap (or REMOTE_KERNELS_BUDGET) cannot be used with cleanup = \"disabled\". \
             Budget enforcement requires the ability to stop/terminate the machine.".into()
        );
    }

    let app_state = state::AppState::new(project_dir);
    let server = server::RemoteKernelsServer::new(config, app_state, budget);
    let shutdown_server = server.clone();

    tracing::info!("Starting remote-kernels MCP server");

    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("Failed to start MCP server: {e}"))?;

    running.waiting().await?;

    // Graceful shutdown: apply each machine's cleanup policy.
    tracing::info!("MCP server disconnected, cleaning up...");
    shutdown_server.shutdown_cleanup().await;

    Ok(())
}
