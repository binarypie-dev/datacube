//! Provider manager - orchestrates all providers

use super::{Item, Provider, ProviderInfo};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Manages all registered providers
pub struct ProviderManager {
    providers: RwLock<Vec<Arc<dyn Provider>>>,
}

impl ProviderManager {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(Vec::new()),
        }
    }

    /// Register a new provider
    pub async fn register<P: Provider + 'static>(&self, provider: P) {
        let name = provider.name().to_string();
        self.providers.write().await.push(Arc::new(provider));
        info!("Registered provider: {}", name);
    }

    /// List all registered providers
    pub async fn list_providers(&self) -> Vec<ProviderInfo> {
        self.providers
            .read()
            .await
            .iter()
            .map(|p| p.info())
            .collect()
    }

    /// Query all applicable providers
    pub async fn query(&self, query: &str, max_results: usize, providers: &[String]) -> Vec<Item> {
        let all_providers = self.providers.read().await;

        // Filter to requested providers, or all if empty
        let applicable: Vec<_> = all_providers
            .iter()
            .filter(|p| {
                if !providers.is_empty() {
                    providers.iter().any(|name| name == p.name())
                } else {
                    p.can_handle(query) && p.enabled()
                }
            })
            .cloned()
            .collect();

        debug!(
            "Querying {} providers for '{}'",
            applicable.len(),
            query
        );

        // Query all applicable providers concurrently
        let futures: Vec<_> = applicable
            .iter()
            .map(|p| {
                let query = query.to_string();
                let provider = Arc::clone(p);
                async move { provider.query(&query, max_results).await }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Combine and sort by score
        let mut items: Vec<Item> = results.into_iter().flatten().collect();
        items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        items.truncate(max_results);

        debug!("Query returned {} items", items.len());
        items
    }

}

impl Default for ProviderManager {
    fn default() -> Self {
        Self::new()
    }
}
