//! Provider system for datacube
//!
//! Providers are the core abstraction for data sources. Each provider
//! implements the `Provider` trait and can respond to queries.

pub mod applications;
pub mod calculator;
pub mod command;
pub mod manager;

pub use applications::ApplicationsProvider;
pub use calculator::CalculatorProvider;
pub use command::CommandProvider;
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
    /// Icon name or path
    pub icon: String,
    /// Provider that generated this item
    pub provider: String,
    /// Relevance score (0.0 - 1.0)
    pub score: f32,
    /// Execution command or action
    pub exec: String,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
    /// Available actions
    pub actions: Vec<Action>,
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
            provider: provider.into(),
            score: 0.0,
            exec: String::new(),
            metadata: HashMap::new(),
            actions: Vec::new(),
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

    pub fn with_score(mut self, score: f32) -> Self {
        self.score = score;
        self
    }

    pub fn with_exec(mut self, exec: impl Into<String>) -> Self {
        self.exec = exec.into();
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(action);
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
            provider: item.provider,
            score: item.score,
            exec: item.exec,
            metadata: item.metadata,
            actions: item.actions.into_iter().map(Into::into).collect(),
        }
    }
}

/// An action that can be performed on an item
#[derive(Debug, Clone)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub icon: String,
}

impl From<Action> for crate::proto::Action {
    fn from(action: Action) -> Self {
        crate::proto::Action {
            id: action.id,
            name: action.name,
            icon: action.icon,
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

    /// Activate an item (execute its action)
    fn activate(&self, item: &Item) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

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
