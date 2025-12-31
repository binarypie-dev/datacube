//! Applications provider - searches installed desktop applications

use super::{Action, Item, Provider};
use freedesktop_desktop_entry::{DesktopEntry, Iter};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::RwLock;
use tracing::{debug, info, warn};

/// A cached application entry
#[derive(Debug, Clone)]
struct AppEntry {
    /// Desktop entry ID (filename without .desktop)
    id: String,
    /// Application name
    name: String,
    /// Generic name (e.g., "Web Browser")
    generic_name: Option<String>,
    /// Description/comment
    comment: Option<String>,
    /// Exec command
    exec: String,
    /// Icon name
    icon: String,
    /// Keywords for searching
    keywords: Vec<String>,
    /// Whether this is a terminal app
    terminal: bool,
    /// Path to the .desktop file
    #[allow(dead_code)]
    path: PathBuf,
    /// Launch count for ranking
    launch_count: u32,
}

/// Provider for installed applications
pub struct ApplicationsProvider {
    /// Cached application entries
    apps: RwLock<Vec<AppEntry>>,
    /// Fuzzy matcher
    matcher: SkimMatcherV2,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        let provider = Self {
            apps: RwLock::new(Vec::new()),
            matcher: SkimMatcherV2::default(),
        };
        provider.load_applications();
        provider
    }

    /// Load all desktop entries from XDG directories
    fn load_applications(&self) {
        let mut apps = Vec::new();

        // Iterate through all XDG data directories
        for path in Iter::new(freedesktop_desktop_entry::default_paths()) {
            match DesktopEntry::from_path::<&str>(&path, None) {
                Ok(entry) => {
                    // Skip entries marked as hidden or no-display
                    if entry.no_display() {
                        continue;
                    }

                    // Empty slice for default locale
                    let locales: &[&str] = &[];

                    // Skip entries without a name or exec
                    let name = match entry.name(locales) {
                        Some(n) => n.to_string(),
                        None => continue,
                    };

                    let exec = match entry.exec() {
                        Some(e) => e.to_string(),
                        None => continue,
                    };

                    // Get the desktop file ID (filename without extension)
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();

                    let app = AppEntry {
                        id,
                        name,
                        generic_name: entry.generic_name(locales).map(|s| s.to_string()),
                        comment: entry.comment(locales).map(|s| s.to_string()),
                        exec,
                        icon: entry.icon().unwrap_or("application-x-executable").to_string(),
                        keywords: entry
                            .keywords(locales)
                            .map(|k| k.into_iter().map(String::from).collect())
                            .unwrap_or_default(),
                        terminal: entry.terminal(),
                        path: path.clone(),
                        launch_count: 0,
                    };

                    apps.push(app);
                }
                Err(e) => {
                    warn!("Failed to read desktop entry {:?}: {}", path, e);
                }
            }
        }

        info!("Loaded {} applications", apps.len());

        if let Ok(mut guard) = self.apps.write() {
            *guard = apps;
        }
    }

    /// Calculate a search score for an app against a query
    fn score_app(&self, app: &AppEntry, query: &str) -> Option<i64> {
        let query_lower = query.to_lowercase();

        // Try matching against name first (highest priority)
        if let Some(score) = self.matcher.fuzzy_match(&app.name.to_lowercase(), &query_lower) {
            return Some(score + 1000); // Boost name matches
        }

        // Try desktop entry ID (e.g., "org.mozilla.firefox" for flatpak apps)
        if let Some(score) = self.matcher.fuzzy_match(&app.id.to_lowercase(), &query_lower) {
            return Some(score + 750);
        }

        // Try generic name
        if let Some(ref generic) = app.generic_name {
            if let Some(score) = self.matcher.fuzzy_match(&generic.to_lowercase(), &query_lower) {
                return Some(score + 500);
            }
        }

        // Try keywords
        for keyword in &app.keywords {
            if let Some(score) = self.matcher.fuzzy_match(&keyword.to_lowercase(), &query_lower) {
                return Some(score + 250);
            }
        }

        // Try comment/description
        if let Some(ref comment) = app.comment {
            if let Some(score) = self.matcher.fuzzy_match(&comment.to_lowercase(), &query_lower) {
                return Some(score);
            }
        }

        None
    }

    /// Clean the exec string by removing field codes (%f, %F, %u, %U, etc.)
    fn clean_exec(exec: &str) -> String {
        let mut result = String::new();
        let mut chars = exec.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '%' {
                // Skip the field code character
                chars.next();
            } else {
                result.push(c);
            }
        }

        result.trim().to_string()
    }

    fn query_impl(&self, query: &str, max_results: usize) -> Vec<Item> {
        let apps = match self.apps.read() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };

        if query.is_empty() {
            // Return most frequently used apps when query is empty
            let mut items: Vec<_> = apps
                .iter()
                .take(max_results)
                .map(|app| {
                    Item::new(&app.name, "applications")
                        .with_subtext(
                            app.comment
                                .as_deref()
                                .or(app.generic_name.as_deref())
                                .unwrap_or(""),
                        )
                        .with_icon(&app.icon)
                        .with_score(app.launch_count as f32 / 100.0)
                        .with_exec(Self::clean_exec(&app.exec))
                        .with_metadata("desktop_id", &app.id)
                        .with_metadata("terminal", if app.terminal { "true" } else { "false" })
                })
                .collect();

            items.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return items;
        }

        // Score and filter apps
        let mut scored: Vec<_> = apps
            .iter()
            .filter_map(|app| self.score_app(app, query).map(|score| (app, score)))
            .collect();

        // Sort by score (highest first)
        scored.sort_by(|a, b| b.1.cmp(&a.1));

        // Convert to Items
        scored
            .into_iter()
            .take(max_results)
            .map(|(app, score)| {
                // Normalize score to 0.0-1.0 range
                let normalized_score = (score as f32 / 2000.0).min(1.0).max(0.0);

                Item::new(&app.name, "applications")
                    .with_subtext(
                        app.comment
                            .as_deref()
                            .or(app.generic_name.as_deref())
                            .unwrap_or(""),
                    )
                    .with_icon(&app.icon)
                    .with_score(normalized_score)
                    .with_exec(Self::clean_exec(&app.exec))
                    .with_metadata("desktop_id", &app.id)
                    .with_metadata("terminal", if app.terminal { "true" } else { "false" })
                    .with_action(Action {
                        id: "launch".to_string(),
                        name: "Launch".to_string(),
                        icon: "system-run".to_string(),
                    })
            })
            .collect()
    }

    fn activate_impl(&self, item: &Item) -> anyhow::Result<()> {
        let exec = &item.exec;
        let is_terminal = item
            .metadata
            .get("terminal")
            .map(|v| v == "true")
            .unwrap_or(false);

        debug!(
            "Activating application: {} (terminal: {})",
            exec, is_terminal
        );

        use std::process::Stdio;

        // Launch detached from datacube so apps survive if datacube exits
        // Use setsid to create a new session, preventing SIGHUP propagation
        if is_terminal {
            std::process::Command::new("setsid")
                .arg("-f")
                .arg("foot")
                .arg("-e")
                .arg("sh")
                .arg("-c")
                .arg(exec)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
        } else {
            std::process::Command::new("setsid")
                .arg("-f")
                .arg("sh")
                .arg("-c")
                .arg(exec)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
        }

        // TODO: Increment launch count for this app

        Ok(())
    }
}

impl Default for ApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for ApplicationsProvider {
    fn name(&self) -> &str {
        "applications"
    }

    fn description(&self) -> &str {
        "Search installed applications"
    }

    fn query(&self, query: &str, max_results: usize) -> Pin<Box<dyn Future<Output = Vec<Item>> + Send + '_>> {
        let result = self.query_impl(query, max_results);
        Box::pin(async move { result })
    }

    fn activate(&self, item: &Item) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let result = self.activate_impl(item);
        Box::pin(async move { result })
    }
}
