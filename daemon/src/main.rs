use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

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
    /// Path to config file (default: auto-discovered ~/.toren/config.kdl)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Terminal for whoever is watching, rolling JSON file for whoever asks later.
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(LevelFilter::from_level(Level::INFO)))
        .with(toren_lib::logging::file_layer("toren-daemon"))
        .init();

    let cli = Cli::parse();

    // The daemon is restartable by design — agents outlive it — so every start is worth a line.
    info!(
        event = "daemon.start",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "Toren initializing, version {}",
        env!("CARGO_PKG_VERSION")
    );

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

    // Agents run in rmux panes, mirrored to whatever browsers attach.
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
