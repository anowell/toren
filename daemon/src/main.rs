use anyhow::Result;
use tracing::{info, Level};
use tracing_subscriber;

mod ancillary;
mod api;
mod config;
mod plugins;
mod security;
mod services;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("Toren initializing, version {}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = config::Config::load()?;
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

    // Start API server
    let addr = format!("{}:{}", config.host, config.port);
    info!("Starting API server on {}", addr);

    api::serve(&addr, services, security_ctx, plugin_manager, ancillary_manager).await?;

    Ok(())
}
