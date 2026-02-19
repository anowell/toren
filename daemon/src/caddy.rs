//! Caddy reverse proxy manager.
//!
//! Manages Caddy routes via the admin API (http://localhost:2019) for
//! per-workspace subdomain routing. Each unique port gets its own Caddy
//! server (named `toren-{port}`), with routes differentiated by hostname.

use anyhow::{Context, Result};
use toren_lib::config::ProxyConfig;
use toren_lib::workspace_setup::ProxyDirective;
use tracing::{debug, info, warn};

/// Manages Caddy routes via the admin API
pub struct CaddyManager {
    client: reqwest::Client,
    admin_url: String,
    config: ProxyConfig,
}

impl CaddyManager {
    pub fn new(config: ProxyConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            admin_url: "http://localhost:2019".to_string(),
            config,
        }
    }

    /// Get the proxy config
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.config
    }

    /// Ensure Caddy is reachable. Logs a warning if not, but does not fail.
    pub async fn ensure_server(&self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let url = format!("{}/config/", self.admin_url);
        match self.client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                info!("Caddy admin API reachable");
            }
            Ok(r) => {
                warn!(
                    "Caddy admin API returned status {}: proxy routes may not work",
                    r.status()
                );
            }
            Err(e) => {
                warn!(
                    "Caddy admin API not reachable ({}): proxy routes will not work",
                    e
                );
            }
        }

        Ok(())
    }

    /// Ensure a per-port Caddy server exists (e.g., `toren-443`).
    /// Creates it if missing; no-op if it already exists.
    async fn ensure_port_server(&self, port: u16, tls: bool) -> Result<()> {
        let server_name = server_name_for_port(port);
        let url = format!(
            "{}/config/apps/http/servers/{}",
            self.admin_url, server_name
        );

        // Check if server already exists
        if let Ok(r) = self.client.get(&url).send().await {
            if r.status().is_success() {
                debug!("Caddy server '{}' already exists", server_name);
                return Ok(());
            }
        }

        // Create server config for this port
        let listen = format!(":{}", port);
        let mut server_config = serde_json::json!({
            "listen": [listen],
            "routes": []
        });

        // Add TLS connection policies when TLS is enabled
        if tls {
            server_config["tls_connection_policies"] = serde_json::json!([{}]);
        }

        let resp = self
            .client
            .put(&url)
            .json(&server_config)
            .send()
            .await
            .context("Failed to connect to Caddy admin API")?;

        if resp.status().is_success() {
            info!("Created Caddy server '{}' listening on :{}", server_name, port);
        } else {
            let body = resp.text().await.unwrap_or_default();
            warn!("Failed to create Caddy server '{}': {}", server_name, body);
        }

        Ok(())
    }

    /// Add a route for a proxy directive.
    /// Creates the per-port server if it doesn't exist, then adds the route.
    pub async fn add_route(&self, directive: &ProxyDirective) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Ensure the per-port server exists
        self.ensure_port_server(directive.port, directive.tls).await?;

        let server_name = server_name_for_port(directive.port);
        let route_id = route_id_for_host(&directive.host);
        let route = build_route(&route_id, directive);

        let url = format!(
            "{}/config/apps/http/servers/{}/routes",
            self.admin_url, server_name
        );
        let resp = self
            .client
            .post(&url)
            .json(&route)
            .send()
            .await
            .context("Failed to add Caddy route")?;

        if resp.status().is_success() {
            info!(
                "Added Caddy route: {} -> {} on :{} (@{})",
                directive.host, directive.upstream, directive.port, route_id
            );
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Failed to add Caddy route for {} on :{}: {}",
                directive.host,
                directive.port,
                body
            )
        }
    }

    /// Remove a route by host
    pub async fn remove_route(&self, host: &str) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let route_id = route_id_for_host(host);
        let url = format!("{}/id/{}", self.admin_url, route_id);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .context("Failed to remove Caddy route")?;

        if resp.status().is_success() {
            info!("Removed Caddy route @{}", route_id);
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 {
                debug!("Caddy route @{} not found (already removed?)", route_id);
                Ok(())
            } else {
                anyhow::bail!("Failed to remove Caddy route @{}: {}", route_id, body)
            }
        }
    }

    /// Add routes for multiple proxy directives
    pub async fn add_routes(&self, directives: &[ProxyDirective]) -> Result<()> {
        for directive in directives {
            self.add_route(directive).await?;
        }
        Ok(())
    }

    /// Remove routes for multiple proxy directives
    pub async fn remove_routes(&self, directives: &[ProxyDirective]) -> Result<()> {
        for directive in directives {
            self.remove_route(&directive.host).await?;
        }
        Ok(())
    }
}

/// Generate a Caddy server name from a port number
fn server_name_for_port(port: u16) -> String {
    format!("toren-{}", port)
}

/// Generate a deterministic route ID from a hostname
fn route_id_for_host(host: &str) -> String {
    format!("toren-{}", host.replace('.', "-"))
}

/// Normalize an upstream value to `host:port` format for Caddy's `dial` field.
/// Handles: bare port ("8001"), host:port ("localhost:8001"), full URL ("http://localhost:8001").
fn normalize_upstream(upstream: &str) -> String {
    // Strip URL scheme if present
    let stripped = upstream
        .trim_start_matches("http://")
        .trim_start_matches("https://");

    // If it's a bare number, prepend localhost
    if stripped.parse::<u16>().is_ok() {
        return format!("localhost:{}", stripped);
    }

    stripped.to_string()
}

/// Build a Caddy route JSON object for a proxy directive
fn build_route(route_id: &str, directive: &ProxyDirective) -> serde_json::Value {
    serde_json::json!({
        "@id": route_id,
        "match": [{
            "host": [directive.host]
        }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{
                "dial": normalize_upstream(&directive.upstream)
            }]
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_upstream_bare_port() {
        assert_eq!(normalize_upstream("8001"), "localhost:8001");
        assert_eq!(normalize_upstream("443"), "localhost:443");
    }

    #[test]
    fn test_normalize_upstream_host_port() {
        assert_eq!(normalize_upstream("localhost:5173"), "localhost:5173");
        assert_eq!(normalize_upstream("myhost:8080"), "myhost:8080");
    }

    #[test]
    fn test_normalize_upstream_url() {
        assert_eq!(
            normalize_upstream("http://localhost:5173"),
            "localhost:5173"
        );
        assert_eq!(
            normalize_upstream("https://localhost:8443"),
            "localhost:8443"
        );
    }

    #[test]
    fn test_server_name_for_port() {
        assert_eq!(server_name_for_port(443), "toren-443");
        assert_eq!(server_name_for_port(8080), "toren-8080");
    }

    #[test]
    fn test_route_id_for_host() {
        assert_eq!(
            route_id_for_host("one.toren.lvh.me"),
            "toren-one-toren-lvh-me"
        );
    }

    #[test]
    fn test_build_route_normalizes_upstream() {
        let directive = ProxyDirective {
            host: "one.toren.lvh.me".to_string(),
            upstream: "8001".to_string(),
            tls: false,
            port: 443,
        };
        let route = build_route("test-id", &directive);
        let dial = route["handle"][0]["upstreams"][0]["dial"].as_str().unwrap();
        assert_eq!(dial, "localhost:8001");
    }
}
