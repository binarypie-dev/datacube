//! Datacube - Data Provider Service
//!
//! A backend service that aggregates data from multiple sources to power
//! application launchers and desktop utilities.

pub mod config;
pub mod providers;
pub mod server;

// Include generated protobuf code
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/datacube.rs"));
}

pub use config::Config;
pub use providers::{ApplicationsProvider, CalculatorProvider, Item, Provider, ProviderManager};
pub use server::Server;
