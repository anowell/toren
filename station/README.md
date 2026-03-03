# Station

Reverse proxy management for local dev services. Station maps named routes to local upstreams via Caddy's admin API.

## Prerequisites

Station requires Caddy running with its admin API available (default: `localhost:2019`).

```sh
brew install caddy
brew services start caddy
# Or: caddy start --config /dev/null
```

No special Caddy configuration is needed — Station manages routes dynamically.

## Usage

### Register a route

```sh
# Port upstream — proxies myapp.localhost:80 to localhost:5173
station proxy myapp -u 5173

# Full URI upstream (path is stripped; Caddy dial only supports host:port)
station proxy api -u http://127.0.0.1:3000

# TLS — proxies myapp.localhost:443 with automatic HTTPS
station proxy myapp -u 5173 --tls

# Custom listen port
station proxy myapp -p 3000 -u 5173
```

### Remove routes

```sh
station forget myapp
```

### List active routes

```sh
station list
```

## Configuration

### STATION_DOMAIN

Set the `STATION_DOMAIN` environment variable to control the base domain for routes. The route name becomes a subdomain: `<name>.<STATION_DOMAIN>`.

| STATION_DOMAIN | Route for `myapp` |
|---|---|
| `localhost` (default) | `myapp.localhost` |
| `lvh.me` | `myapp.lvh.me` |
| `myrepo.lvh.me` | `myapp.myrepo.lvh.me` |

`lvh.me` resolves to 127.0.0.1 via wildcard DNS, which is useful when `localhost` subdomains don't resolve in your browser.

### STATION_CADDY_URL

Override the Caddy admin API URL (default: `http://localhost:2019`).

## Toren integration

Station integrates with toren workspaces via `toren.kdl` configuration. Toren automatically sets `STATION_DOMAIN` to `{repo_name}.{local_domain}` for `run` and `proxy` actions.

### Using `proxy` directive (recommended)

```kdl
setup {
    proxy 80 upstream=3000
    proxy "https" upstream=4443 name="api"
}
```

Toren automatically cleans up proxy routes on workspace destroy.

### Using `run` actions (manual)

```kdl
setup {
    run "station proxy {{ ws.name }}.{{ repo.name }} -u {{ vars.port }}"
}
destroy {
    run "station forget {{ ws.name }}.{{ repo.name }}"
}
```

### Migration note

The `config.proxy.*` template variables previously available in `toren.kdl` have been removed. Use `STATION_DOMAIN` (set automatically by toren) or the `proxy` directive instead.
