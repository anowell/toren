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

    #[cfg(test)]
    fn new_with_admin_url(config: ProxyConfig, admin_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            admin_url,
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

    /// Ensure a per-port Caddy server exists with a routes array (e.g., `toren-443`).
    /// Creates the server if missing. If it exists but `routes` was removed
    /// (e.g. after all routes were deleted), re-creates the empty array.
    async fn ensure_port_server(&self, port: u16, tls: bool) -> Result<()> {
        let server_name = server_name_for_port(port);
        let server_url = format!(
            "{}/config/apps/http/servers/{}",
            self.admin_url, server_name
        );

        // Check if server already exists (Caddy returns null/200 for missing keys,
        // so we must verify the body is an actual JSON object)
        if let Ok(r) = self.client.get(&server_url).send().await {
            if r.status().is_success() {
                let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
                if body.is_object() {
                    // Server exists — verify routes array exists
                    // (Caddy may remove it when the last route is deleted)
                    if body.get("routes").and_then(|v| v.as_array()).is_some() {
                        debug!("Caddy server '{}' ready", server_name);
                        return Ok(());
                    }
                    // Routes array missing — recreate it
                    debug!("Caddy server '{}' missing routes array, recreating", server_name);
                    let routes_url = format!("{}/routes", server_url);
                    let resp = self
                        .client
                        .put(&routes_url)
                        .json(&serde_json::json!([]))
                        .send()
                        .await
                        .context("Failed to connect to Caddy admin API")?;
                    if resp.status().is_success() {
                        return Ok(());
                    }
                    let body = resp.text().await.unwrap_or_default();
                    anyhow::bail!(
                        "Failed to recreate routes array for Caddy server '{}': {}",
                        server_name,
                        body
                    );
                }
                // Body is null — server doesn't actually exist, fall through to create
            }
        }

        // Create server config for this port
        let listen = format!(":{}", port);
        let mut server_config = serde_json::json!({
            "listen": [listen],
            "routes": []
        });

        if tls {
            server_config["tls_connection_policies"] = serde_json::json!([{}]);
        } else {
            // Disable Caddy's automatic HTTPS (it auto-enables TLS on non-standard
            // ports when host matchers are present)
            server_config["automatic_https"] = serde_json::json!({"disable": true});
        }

        let resp = self
            .client
            .put(&server_url)
            .json(&server_config)
            .send()
            .await
            .context("Failed to connect to Caddy admin API")?;

        if resp.status().is_success() {
            info!("Created Caddy server '{}' listening on :{}", server_name, port);
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to create Caddy server '{}': {}", server_name, body)
        }
    }

    /// Add a route for a proxy directive (upsert).
    /// Removes any existing route for the same host+port first,
    /// creates the per-port server if needed, then adds the route.
    pub async fn add_route(&self, directive: &ProxyDirective) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Remove existing route for this host+port to avoid duplicates
        self.remove_route(&directive.host, directive.port).await?;

        // Ensure the per-port server exists
        self.ensure_port_server(directive.port, directive.tls).await?;

        let server_name = server_name_for_port(directive.port);
        let route_id = route_id_for_directive(&directive.host, directive.port);
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

    /// Remove a route by host and port
    pub async fn remove_route(&self, host: &str, port: u16) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let route_id = route_id_for_directive(host, port);
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
            self.remove_route(&directive.host, directive.port).await?;
        }
        Ok(())
    }

    /// List all active toren proxy routes from Caddy
    pub async fn list_routes(&self) -> Result<Vec<ProxyRouteInfo>> {
        if !self.config.enabled {
            return Ok(vec![]);
        }

        let url = format!("{}/config/apps/http/servers/", self.admin_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to Caddy admin API")?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let servers: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Caddy servers response")?;

        let mut routes = Vec::new();

        if let Some(obj) = servers.as_object() {
            for (server_name, server_config) in obj {
                if !server_name.starts_with("toren-") {
                    continue;
                }

                let port = server_name
                    .strip_prefix("toren-")
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(0);

                let tls = server_config.get("tls_connection_policies").is_some();

                if let Some(route_array) = server_config["routes"].as_array() {
                    for route in route_array {
                        let host = route["match"][0]["host"][0]
                            .as_str()
                            .unwrap_or("?")
                            .to_string();
                        let upstream = route["handle"][0]["upstreams"][0]["dial"]
                            .as_str()
                            .unwrap_or("?")
                            .to_string();

                        routes.push(ProxyRouteInfo {
                            host,
                            upstream,
                            port,
                            tls,
                        });
                    }
                }
            }
        }

        Ok(routes)
    }
}

/// Info about an active proxy route
#[derive(Debug, serde::Serialize)]
pub struct ProxyRouteInfo {
    pub host: String,
    pub upstream: String,
    pub port: u16,
    pub tls: bool,
}

/// Generate a Caddy server name from a port number
fn server_name_for_port(port: u16) -> String {
    format!("toren-{}", port)
}

/// Generate a deterministic route ID from a hostname and port
fn route_id_for_directive(host: &str, port: u16) -> String {
    format!("toren-{}-{}", host.replace('.', "-"), port)
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            tls: false,
            domain: "toren.lvh.me".to_string(),
            dns_port: None,
        }
    }

    fn test_directive(host: &str, upstream: &str, port: u16, tls: bool) -> ProxyDirective {
        ProxyDirective {
            host: host.to_string(),
            upstream: upstream.to_string(),
            port,
            tls,
        }
    }

    // -- ensure_port_server tests --

    #[tokio::test]
    async fn test_ensure_port_server_creates_new_server() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        // GET server returns 404 — server doesn't exist
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&mock)
            .await;

        // PUT creates the server
        Mock::given(method("PUT"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        mgr.ensure_port_server(8080, false).await.unwrap();

        // Verify PUT body via a recorded request
        let requests = mock.received_requests().await.unwrap();
        let put_req = requests.iter().find(|r| r.method == reqwest::Method::PUT).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&put_req.body).unwrap();

        assert_eq!(body["listen"], serde_json::json!([":8080"]));
        assert_eq!(body["routes"], serde_json::json!([]));
        assert_eq!(body["automatic_https"]["disable"], serde_json::json!(true));
        assert!(body.get("tls_connection_policies").is_none());
    }

    #[tokio::test]
    async fn test_ensure_port_server_creates_tls_server() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-443"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path("/config/apps/http/servers/toren-443"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        mgr.ensure_port_server(443, true).await.unwrap();

        let requests = mock.received_requests().await.unwrap();
        let put_req = requests.iter().find(|r| r.method == reqwest::Method::PUT).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&put_req.body).unwrap();

        assert_eq!(body["listen"], serde_json::json!([":443"]));
        assert_eq!(body["routes"], serde_json::json!([]));
        assert_eq!(body["tls_connection_policies"], serde_json::json!([{}]));
        assert!(body.get("automatic_https").is_none());
    }

    #[tokio::test]
    async fn test_ensure_port_server_null_200_creates_server() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        // Caddy quirk: returns `null` with HTTP 200 for non-existent keys
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .expect(1)
            .mount(&mock)
            .await;

        // Should fall through to full PUT (not the routes-repair path)
        Mock::given(method("PUT"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        mgr.ensure_port_server(8080, false).await.unwrap();

        let requests = mock.received_requests().await.unwrap();
        let put_req = requests.iter().find(|r| r.method == reqwest::Method::PUT).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&put_req.body).unwrap();

        // Full server config, not just routes array
        assert_eq!(body["listen"], serde_json::json!([":8080"]));
        assert_eq!(body["routes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_ensure_port_server_existing_server_with_routes() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        // Server exists with routes array
        let server_body = serde_json::json!({
            "listen": [":8080"],
            "routes": [{"@id": "some-route"}]
        });
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&server_body))
            .expect(1)
            .mount(&mock)
            .await;

        // No PUT should be made
        Mock::given(method("PUT"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;

        mgr.ensure_port_server(8080, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_ensure_port_server_repairs_missing_routes() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        // Server exists but routes array is missing (Caddy removes it when last route deleted)
        let server_body = serde_json::json!({
            "listen": [":8080"]
        });
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&server_body))
            .expect(1)
            .mount(&mock)
            .await;

        // Should PUT just the routes array (not full server config)
        Mock::given(method("PUT"))
            .and(path("/config/apps/http/servers/toren-8080/routes"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        mgr.ensure_port_server(8080, false).await.unwrap();

        let requests = mock.received_requests().await.unwrap();
        let put_req = requests.iter().find(|r| r.method == reqwest::Method::PUT).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&put_req.body).unwrap();

        // Just an empty array, not full server config
        assert_eq!(body, serde_json::json!([]));
    }

    // -- add_route / remove_route tests --

    #[tokio::test]
    async fn test_add_route_removes_existing_first() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());
        let directive = test_directive("app.toren.lvh.me", "localhost:3000", 8080, false);
        let route_id = route_id_for_directive(&directive.host, directive.port);

        // DELETE existing route (200 = it existed)
        Mock::given(method("DELETE"))
            .and(path(format!("/id/{}", route_id)))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        // GET server — exists with routes
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "listen": [":8080"],
                    "routes": []
                })),
            )
            .expect(1)
            .mount(&mock)
            .await;

        // POST adds the new route
        Mock::given(method("POST"))
            .and(path("/config/apps/http/servers/toren-8080/routes"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        mgr.add_route(&directive).await.unwrap();

        // Verify DELETE happened before POST by checking request order
        let requests = mock.received_requests().await.unwrap();
        let delete_idx = requests
            .iter()
            .position(|r| r.method == reqwest::Method::DELETE)
            .unwrap();
        let post_idx = requests
            .iter()
            .position(|r| r.method == reqwest::Method::POST)
            .unwrap();
        assert!(delete_idx < post_idx, "DELETE should happen before POST");
    }

    #[tokio::test]
    async fn test_add_route_upsert_on_repeated_call() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());
        let directive = test_directive("app.toren.lvh.me", "localhost:3000", 8080, false);
        let route_id = route_id_for_directive(&directive.host, directive.port);

        // First call: DELETE returns 404 (no existing route)
        // Second call: DELETE returns 200 (removes the one we just added)
        // Use up_to_n_times to handle the two-call sequence
        Mock::given(method("DELETE"))
            .and(path(format!("/id/{}", route_id)))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&mock)
            .await;

        // Server exists for both calls
        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/toren-8080"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "listen": [":8080"],
                    "routes": []
                })),
            )
            .mount(&mock)
            .await;

        // POST adds route (both calls)
        Mock::given(method("POST"))
            .and(path("/config/apps/http/servers/toren-8080/routes"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        // First add
        mgr.add_route(&directive).await.unwrap();

        // Register the 200 DELETE mock for the second call
        Mock::given(method("DELETE"))
            .and(path(format!("/id/{}", route_id)))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        // Second add (upsert — removes old, adds new)
        mgr.add_route(&directive).await.unwrap();

        let requests = mock.received_requests().await.unwrap();
        let delete_count = requests
            .iter()
            .filter(|r| r.method == reqwest::Method::DELETE)
            .count();
        let post_count = requests
            .iter()
            .filter(|r| r.method == reqwest::Method::POST)
            .count();
        assert_eq!(delete_count, 2, "DELETE called once per add_route");
        assert_eq!(post_count, 2, "POST called once per add_route");
    }

    #[tokio::test]
    async fn test_remove_route_404_is_ok() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        let route_id = route_id_for_directive("app.toren.lvh.me", 8080);

        // DELETE returns 404 — route doesn't exist
        Mock::given(method("DELETE"))
            .and(path(format!("/id/{}", route_id)))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&mock)
            .await;

        // Should succeed (idempotent delete)
        mgr.remove_route("app.toren.lvh.me", 8080).await.unwrap();
    }

    // -- list_routes tests --

    #[tokio::test]
    async fn test_list_routes_parses_servers() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        let servers = serde_json::json!({
            "toren-443": {
                "listen": [":443"],
                "tls_connection_policies": [{}],
                "routes": [{
                    "match": [{"host": ["app.toren.lvh.me"]}],
                    "handle": [{"handler": "reverse_proxy", "upstreams": [{"dial": "localhost:3000"}]}]
                }]
            },
            "toren-8080": {
                "listen": [":8080"],
                "routes": [{
                    "match": [{"host": ["api.toren.lvh.me"]}],
                    "handle": [{"handler": "reverse_proxy", "upstreams": [{"dial": "localhost:4000"}]}]
                }]
            },
            "other-server": {
                "listen": [":9999"],
                "routes": [{
                    "match": [{"host": ["external.example.com"]}],
                    "handle": [{"handler": "reverse_proxy", "upstreams": [{"dial": "localhost:5000"}]}]
                }]
            }
        });

        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&servers))
            .expect(1)
            .mount(&mock)
            .await;

        let routes = mgr.list_routes().await.unwrap();

        // Should only include toren-* servers (not other-server)
        assert_eq!(routes.len(), 2);

        let app_route = routes.iter().find(|r| r.host == "app.toren.lvh.me").unwrap();
        assert_eq!(app_route.upstream, "localhost:3000");
        assert_eq!(app_route.port, 443);
        assert!(app_route.tls);

        let api_route = routes.iter().find(|r| r.host == "api.toren.lvh.me").unwrap();
        assert_eq!(api_route.upstream, "localhost:4000");
        assert_eq!(api_route.port, 8080);
        assert!(!api_route.tls);
    }

    #[tokio::test]
    async fn test_list_routes_extracts_port_from_server_name() {
        let mock = MockServer::start().await;
        let mgr = CaddyManager::new_with_admin_url(test_config(), mock.uri());

        // listen field says :9999 but server name says toren-443
        // Port should come from the name, not listen
        let servers = serde_json::json!({
            "toren-443": {
                "listen": [":9999"],
                "routes": [{
                    "match": [{"host": ["app.toren.lvh.me"]}],
                    "handle": [{"handler": "reverse_proxy", "upstreams": [{"dial": "localhost:3000"}]}]
                }]
            }
        });

        Mock::given(method("GET"))
            .and(path("/config/apps/http/servers/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&servers))
            .expect(1)
            .mount(&mock)
            .await;

        let routes = mgr.list_routes().await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].port, 443); // from server name, not listen field
    }

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
    fn test_route_id_for_directive() {
        assert_eq!(
            route_id_for_directive("one.toren.lvh.me", 443),
            "toren-one-toren-lvh-me-443"
        );
        assert_eq!(
            route_id_for_directive("one.toren.lvh.me", 80),
            "toren-one-toren-lvh-me-80"
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
