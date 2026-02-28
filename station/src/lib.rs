//! Station — reverse proxy management for local dev services.
//!
//! Station manages named routes through a proxy backend (currently Caddy),
//! mapping `<name>.<domain>:<port>` to local upstream services.

mod caddy;

pub use caddy::CaddyBackend;

use anyhow::Result;

/// Configuration for a Station instance.
pub struct StationConfig {
    /// Base domain for proxied services (e.g., "localhost", "lvh.me").
    pub domain: String,
    /// Caddy admin API URL (default: "http://localhost:2019").
    pub caddy_admin_url: String,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            domain: "localhost".to_string(),
            caddy_admin_url: "http://localhost:2019".to_string(),
        }
    }
}

/// Info about an active proxy route.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteInfo {
    /// The full hostname (e.g., "myapp.localhost").
    pub host: String,
    /// The upstream address (e.g., "localhost:5173").
    pub upstream: String,
    /// The listen port on the public-facing side.
    pub port: u16,
    /// Whether TLS is enabled.
    pub tls: bool,
}

/// Station manages named proxy routes via a backend (Caddy).
pub struct Station {
    domain: String,
    backend: CaddyBackend,
}

impl Station {
    /// Create a new Station with the given config.
    pub fn new(config: StationConfig) -> Self {
        Self {
            backend: CaddyBackend::new(config.caddy_admin_url),
            domain: config.domain,
        }
    }

    /// Create from environment variables.
    ///
    /// - `STATION_DOMAIN` — base domain (default: "localhost")
    /// - `STATION_CADDY_URL` — Caddy admin API URL (default: "http://localhost:2019")
    pub fn from_env() -> Self {
        let domain = std::env::var("STATION_DOMAIN").unwrap_or_else(|_| "localhost".to_string());
        let caddy_url = std::env::var("STATION_CADDY_URL")
            .unwrap_or_else(|_| "http://localhost:2019".to_string());
        Self::new(StationConfig {
            domain,
            caddy_admin_url: caddy_url,
        })
    }

    /// Register a named proxy route.
    ///
    /// The name becomes the subdomain: `<name>.<domain>:<port>`.
    /// Upstream can be a port number ("5173") or full URI ("http://127.0.0.1:5173/api").
    pub async fn proxy(&self, name: &str, upstream: &str, port: u16, tls: bool) -> Result<()> {
        let host = self.host_for_name(name);
        self.backend.add_route(&host, upstream, port, tls).await
    }

    /// Remove all routes for a named service across all ports.
    pub async fn forget(&self, name: &str) -> Result<()> {
        let host = self.host_for_name(name);
        self.backend.remove_routes_for_host(&host).await
    }

    /// List all active station-managed routes.
    pub async fn list(&self) -> Result<Vec<RouteInfo>> {
        self.backend.list_routes().await
    }

    /// The configured domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    fn host_for_name(&self, name: &str) -> String {
        format!("{}.{}", name, self.domain)
    }
}
