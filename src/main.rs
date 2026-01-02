//! Datacube daemon - Data Provider Service
//!
//! A backend service that aggregates data from multiple sources to power
//! application launchers and desktop utilities.

use clap::Parser;
use datacube::{ApplicationsProvider, CalculatorProvider, Config, ProviderManager, Server};
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(name = "datacube")]
#[command(author, version, about = "Data provider service for desktop utilities")]
struct Args {
    /// Config file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Socket path (overrides config)
    #[arg(short, long)]
    socket: Option<PathBuf>,

    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    /// Run in foreground (don't daemonize)
    #[arg(short, long)]
    foreground: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.debug { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("datacube v{} starting...", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let mut config = if let Some(config_path) = args.config {
        match std::fs::read_to_string(&config_path) {
            Ok(content) => toml::from_str(&content)?,
            Err(e) => {
                tracing::warn!("Failed to load config from {:?}: {}", config_path, e);
                Config::default()
            }
        }
    } else {
        Config::load()
    };

    // Override socket path if specified
    if let Some(socket) = args.socket {
        config.socket_path = socket;
    }

    // Create provider manager and register providers
    let manager = ProviderManager::new();

    if config.providers.applications.enabled {
        let extra_dirs = config.providers.applications.extra_dirs.clone();
        manager
            .register(ApplicationsProvider::with_extra_dirs(extra_dirs))
            .await;
    }

    if config.providers.calculator.enabled {
        manager.register(CalculatorProvider::new()).await;
    }


    info!("Registered {} providers", manager.list_providers().await.len());

    // Create and run server
    let server = Server::new(config, manager);
    server.run().await?;

    Ok(())
}
