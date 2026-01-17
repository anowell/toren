use anyhow::Result;
use tracing::{info, Level};

mod ancillary;
mod api;
mod plugins;
mod security;
mod services;

// Re-export from toren-lib for internal use
use toren_lib::{AssignmentManager, Config, SegmentManager, WorkspaceManager};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("Toren initializing, version {}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = Config::load()?;
    info!("Loaded configuration from: {}", config.config_path);

    // Initialize security context
    let security_ctx = security::SecurityContext::new(&config)?;

    // Log pairing token (indicate if it's from env var)
    if std::env::var("PAIRING_TOKEN").is_ok() {
        info!("Security initialized. Using fixed pairing token: {}", security_ctx.pairing_token());
    } else {
        info!("Security initialized. Pairing token: {}", security_ctx.pairing_token());
    }

    // Initialize plugin manager
    let mut plugin_manager = plugins::PluginManager::new();
    plugin_manager.add_plugin_dir(".toren/commands".into());
    if let Some(home) = dirs::home_dir() {
        plugin_manager.add_plugin_dir(home.join(".config/toren/commands"));
    }
    plugin_manager.load_all()?;
    info!("Ancillary systems initialized");

    // Start services
    let services = services::Services::new(&config, &security_ctx).await?;
    info!("Services initialized");

    // Initialize ancillary manager
    let ancillary_manager = ancillary::AncillaryManager::new();
    info!("Ancillary manager initialized");

    // Initialize assignment manager
    let assignment_manager = AssignmentManager::new()?;
    info!("Assignment manager initialized");

    // Initialize segment manager
    let segment_manager = SegmentManager::new(&config)?;
    info!("Segment manager initialized");

    // Initialize workspace manager (if workspace_root is configured)
    let workspace_manager = config.ancillary.workspace_root.clone().map(|root| {
        info!("Workspace manager initialized with root: {}", root.display());
        WorkspaceManager::new(root)
    });

    // Initialize work manager (for embedded ancillary runtime)
    let work_manager = ancillary::WorkManager::new();
    info!("Work manager initialized");

    // Start API server
    let addr = format!("{}:{}", config.host(), config.port());
    info!("Starting API server on {}", addr);

    api::serve(&addr, config, services, security_ctx, plugin_manager, ancillary_manager, assignment_manager, segment_manager, workspace_manager, work_manager).await?;

    Ok(())
}
