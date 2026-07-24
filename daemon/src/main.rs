use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::{info, Level};

mod ancillary;
mod api;
mod security;
mod services;

// Re-export from toren-lib for internal use
use toren_lib::{Config, SegmentManager, WorkspaceManager};

#[derive(Parser)]
#[command(name = "toren-daemon")]
#[command(about = "Toren daemon - API server for bead-driven development")]
struct Cli {
    /// Path to config file (default: auto-discovered toren.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let cli = Cli::parse();

    info!("Toren initializing, version {}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = Config::load_from(cli.config.as_deref())?;
    info!("Loaded configuration from: {}", config.config_path);

    // Initialize security context
    let security_ctx = security::SecurityContext::new(&config)?;

    // Log pairing token (indicate if it's from env var)
    if std::env::var("PAIRING_TOKEN").is_ok() {
        info!(
            "Security initialized. Using fixed pairing token: {}",
            security_ctx.pairing_token()
        );
    } else {
        info!(
            "Security initialized. Pairing token: {}",
            security_ctx.pairing_token()
        );
    }

    // Initialize Rhai plugin manager (shared with breq CLI)
    let rhai_plugins = toren_lib::PluginManager::new(&toren_lib::toren_root().join("plugins"))?;
    info!(
        "Rhai agent plugins loaded: {:?}",
        rhai_plugins.list_agents()
    );
    info!("Ancillary systems initialized");

    // Start services
    let services = services::Services::new(&config, &security_ctx).await?;
    info!("Services initialized");

    // Initialize ancillary manager
    let ancillary_manager = ancillary::AncillaryManager::new();
    info!("Ancillary manager initialized");

    // Initialize segment manager
    let segment_manager = SegmentManager::new(&config)?;
    info!("Segment manager initialized");

    // Initialize workspace manager
    let local_domain = Some(config.proxy.domain.clone());
    let workspace_root = config.ancillaries.workspace_root.clone();
    info!(
        "Workspace manager initialized with root: {}",
        workspace_root.display()
    );
    let workspace_manager = Some(WorkspaceManager::new(workspace_root, local_domain));

    // Agents run in rmux panes; transcript paths come from toren_lib::transcripts.
    let pane_runner = services::pane_runner::PaneRunner::new();
    info!("Pane runner initialized");

    // Start API server
    let addr = format!("{}:{}", config.host(), config.port());
    info!("Starting API server on {}", addr);

    api::serve(
        &addr,
        config,
        services,
        security_ctx,
        rhai_plugins,
        ancillary_manager,
        segment_manager,
        workspace_manager,
        pane_runner,
    )
    .await?;

    Ok(())
}
