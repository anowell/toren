use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "station", about = "Reverse proxy management for local dev services")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Register a named proxy route
    Proxy {
        /// Route name (becomes subdomain: <name>.$STATION_DOMAIN)
        name: String,

        /// Upstream: port number (e.g., 5173) or full URI (e.g., http://127.0.0.1:3000/v2)
        #[arg(short = 'u', long = "upstream")]
        upstream: String,

        /// Listen port on the public-facing side (default: 80, or 443 with --tls)
        #[arg(short, long)]
        port: Option<u16>,

        /// Enable TLS (Caddy handles cert provisioning)
        #[arg(long)]
        tls: bool,
    },

    /// Remove all routes for a named service
    Forget {
        /// Route name to remove
        name: String,
    },

    /// List all active station-managed routes
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let station = station::Station::from_env();

    match cli.command {
        Commands::Proxy {
            name,
            upstream,
            port,
            tls,
        } => {
            let port = port.unwrap_or(if tls { 443 } else { 80 });
            station.proxy(&name, &upstream, port, tls).await?;
            let host = format!("{}.{}", name, station.domain());
            let scheme = if tls { "https" } else { "http" };
            let port_suffix = match (tls, port) {
                (true, 443) | (false, 80) => String::new(),
                _ => format!(":{}", port),
            };
            println!("{}://{}{} -> {}", scheme, host, port_suffix, upstream);
        }
        Commands::Forget { name } => {
            station.forget(&name).await?;
            println!("Removed routes for {}", name);
        }
        Commands::List => {
            let routes = station.list().await?;
            if routes.is_empty() {
                println!("No active routes");
            } else {
                for route in routes {
                    let scheme = if route.tls { "https" } else { "http" };
                    let port_suffix = match (route.tls, route.port) {
                        (true, 443) | (false, 80) => String::new(),
                        _ => format!(":{}", route.port),
                    };
                    println!(
                        "{}://{}{} -> {}",
                        scheme, route.host, port_suffix, route.upstream
                    );
                }
            }
        }
    }

    Ok(())
}
