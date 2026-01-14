//! Provider system for datacube
//!
//! Providers are the core abstraction for data sources. Each provider
//! implements the `Provider` trait and can respond to queries.

pub mod applications;
pub mod calculator;
pub mod manager;

pub use applications::ApplicationsProvider;
pub use calculator::CalculatorProvider;
pub use manager::ProviderManager;

use std::collections::HashMap;

/// A single result item from a provider
#[derive(Debug, Clone)]
pub struct Item {
    /// Unique identifier for this item
    pub id: String,
    /// Primary display text (e.g., app name)
    pub text: String,
    /// Secondary display text (e.g., description)
    pub subtext: String,
    /// Icon name (from .desktop file)
    pub icon: String,
    /// Resolved icon file path (SVG preferred, then largest PNG)
    pub icon_path: String,
    /// Provider that generated this item
    pub provider: String,
    /// Relevance score (0.0 - 1.0)
    pub score: f32,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
    /// Source of the item (e.g., "native", "flatpak", "snap")
    pub source: String,
}

impl Item {
    pub fn new(text: impl Into<String>, provider: impl Into<String>) -> Self {
        let text = text.into();
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            text,
            subtext: String::new(),
            icon: String::new(),
            icon_path: String::new(),
            provider: provider.into(),
            score: 0.0,
            metadata: HashMap::new(),
            source: String::new(),
        }
    }

    pub fn with_subtext(mut self, subtext: impl Into<String>) -> Self {
        self.subtext = subtext.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn with_icon_path(mut self, icon_path: impl Into<String>) -> Self {
        self.icon_path = icon_path.into();
        self
    }

    pub fn with_score(mut self, score: f32) -> Self {
        self.score = score;
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

impl From<Item> for crate::proto::Item {
    fn from(item: Item) -> Self {
        crate::proto::Item {
            id: item.id,
            text: item.text,
            subtext: item.subtext,
            icon: item.icon,
            icon_path: item.icon_path,
            provider: item.provider,
            score: item.score,
            metadata: item.metadata,
            source: item.source,
        }
    }
}

/// Information about a provider
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub name: String,
    pub description: String,
    pub prefix: Option<String>,
    pub enabled: bool,
}

impl From<ProviderInfo> for crate::proto::ProviderInfo {
    fn from(info: ProviderInfo) -> Self {
        crate::proto::ProviderInfo {
            name: info.name,
            description: info.description,
            prefix: info.prefix.unwrap_or_default(),
            enabled: info.enabled,
        }
    }
}

use std::future::Future;
use std::pin::Pin;

/// The core provider trait
///
/// All data providers must implement this trait to integrate with datacube.
/// Uses boxed futures for dyn-compatibility.
pub trait Provider: Send + Sync {
    /// Returns the unique name of this provider
    fn name(&self) -> &str;

    /// Returns a human-readable description
    fn description(&self) -> &str;

    /// Returns the query prefix that triggers this provider (e.g., "=" for calculator)
    /// Returns None if the provider handles all queries
    fn prefix(&self) -> Option<&str> {
        None
    }

    /// Returns whether this provider is currently enabled
    fn enabled(&self) -> bool {
        true
    }

    /// Check if this provider can handle the given query
    fn can_handle(&self, query: &str) -> bool {
        match self.prefix() {
            Some(prefix) => query.starts_with(prefix),
            None => true,
        }
    }

    /// Query the provider for matching items
    fn query(&self, query: &str, max_results: usize) -> Pin<Box<dyn Future<Output = Vec<Item>> + Send + '_>>;

    /// Get provider info
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name().to_string(),
            description: self.description().to_string(),
            prefix: self.prefix().map(String::from),
            enabled: self.enabled(),
        }
    }
}
