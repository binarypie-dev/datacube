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

        debug!("Querying {} providers for '{}'", applicable.len(), query);

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
        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Item;
    use std::future::Future;
    use std::pin::Pin;

    /// A configurable provider for exercising the manager's routing/sorting.
    struct MockProvider {
        name: String,
        prefix: Option<String>,
        /// (text, score) pairs returned for any query.
        items: Vec<(&'static str, f32)>,
    }

    impl Provider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "mock provider"
        }
        fn prefix(&self) -> Option<&str> {
            self.prefix.as_deref()
        }
        fn query(
            &self,
            _query: &str,
            _max_results: usize,
        ) -> Pin<Box<dyn Future<Output = Vec<Item>> + Send + '_>> {
            let name = self.name.clone();
            let items: Vec<Item> = self
                .items
                .iter()
                .map(|(text, score)| Item::new(*text, name.clone()).with_score(*score))
                .collect();
            Box::pin(async move { items })
        }
    }

    fn mock(name: &str, prefix: Option<&str>, items: Vec<(&'static str, f32)>) -> MockProvider {
        MockProvider {
            name: name.to_string(),
            prefix: prefix.map(String::from),
            items,
        }
    }

    #[tokio::test]
    async fn registers_and_lists_providers() {
        let manager = ProviderManager::new();
        manager.register(mock("alpha", None, vec![])).await;
        manager.register(mock("beta", Some("="), vec![])).await;

        let providers = manager.list_providers().await;
        assert_eq!(providers.len(), 2);
        let names: Vec<_> = providers.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[tokio::test]
    async fn query_combines_and_sorts_by_score() {
        let manager = ProviderManager::new();
        manager
            .register(mock("a", None, vec![("low", 0.1), ("high", 0.9)]))
            .await;
        manager.register(mock("b", None, vec![("mid", 0.5)])).await;

        let items = manager.query("anything", 10, &[]).await;
        let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["high", "mid", "low"]);
    }

    #[tokio::test]
    async fn query_truncates_to_max_results() {
        let manager = ProviderManager::new();
        manager
            .register(mock("a", None, vec![("x", 0.3), ("y", 0.2), ("z", 0.1)]))
            .await;

        let items = manager.query("q", 2, &[]).await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "x");
    }

    #[tokio::test]
    async fn explicit_provider_filter_is_respected() {
        let manager = ProviderManager::new();
        manager
            .register(mock("apps", None, vec![("app", 0.5)]))
            .await;
        manager
            .register(mock("calc", Some("="), vec![("calc-result", 0.5)]))
            .await;

        // Even without the prefix, an explicit provider request is honoured.
        let items = manager.query("apps query", 10, &["calc".to_string()]).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "calc-result");
    }

    #[tokio::test]
    async fn prefix_provider_only_matches_with_prefix() {
        let manager = ProviderManager::new();
        manager
            .register(mock("apps", None, vec![("app", 0.5)]))
            .await;
        manager
            .register(mock("calc", Some("="), vec![("calc-result", 0.9)]))
            .await;

        // No prefix: calculator should not contribute.
        let plain = manager.query("firefox", 10, &[]).await;
        assert!(plain.iter().all(|i| i.text != "calc-result"));

        // With prefix: calculator is included.
        let prefixed = manager.query("=2+2", 10, &[]).await;
        assert!(prefixed.iter().any(|i| i.text == "calc-result"));
    }
}
