//! Applications provider - searches installed desktop applications

use super::{Item, Provider};
use freedesktop_desktop_entry::{DesktopEntry, Iter};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use tracing::{debug, error, info, warn};

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
    apps: Arc<RwLock<Vec<AppEntry>>>,
    /// Fuzzy matcher
    matcher: SkimMatcherV2,
    /// Extra directories to scan (from config)
    #[allow(dead_code)]
    extra_dirs: Vec<PathBuf>,
    /// Keep watcher alive - dropping it stops watching
    #[allow(dead_code)]
    watcher: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        Self::with_extra_dirs(Vec::new())
    }

    pub fn with_extra_dirs(extra_dirs: Vec<PathBuf>) -> Self {
        let apps = Arc::new(RwLock::new(Vec::new()));

        // Load applications initially
        Self::load_applications_into(&apps, &extra_dirs);

        // Set up file watching
        let watcher = Self::start_watching(Arc::clone(&apps), &extra_dirs);

        Self {
            apps,
            matcher: SkimMatcherV2::default(),
            extra_dirs,
            watcher,
        }
    }

    /// Get all directories that should be watched for .desktop files
    fn get_watch_directories(extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
        let mut dirs = HashSet::new();

        // Standard XDG application directories
        for path in freedesktop_desktop_entry::default_paths() {
            if let Some(parent) = path.parent() {
                // default_paths() returns individual .desktop files, we want the directories
                if parent.is_dir() {
                    dirs.insert(parent.to_path_buf());
                }
            }
        }

        // Also add the standard locations explicitly in case default_paths is empty
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        // User applications
        let user_apps = home.join(".local/share/applications");
        if user_apps.is_dir() {
            dirs.insert(user_apps);
        }

        // System applications
        for sys_path in &[
            "/usr/share/applications",
            "/usr/local/share/applications",
        ] {
            let p = PathBuf::from(sys_path);
            if p.is_dir() {
                dirs.insert(p);
            }
        }

        // Flatpak directories
        let flatpak_system = PathBuf::from("/var/lib/flatpak/exports/share/applications");
        if flatpak_system.is_dir() {
            dirs.insert(flatpak_system);
        }

        let flatpak_user = home.join(".local/share/flatpak/exports/share/applications");
        if flatpak_user.is_dir() {
            dirs.insert(flatpak_user);
        }

        // Snap directory
        let snap_apps = PathBuf::from("/var/lib/snapd/desktop/applications");
        if snap_apps.is_dir() {
            dirs.insert(snap_apps);
        }

        // Extra directories from config
        for dir in extra_dirs {
            if dir.is_dir() {
                dirs.insert(dir.clone());
            }
        }

        dirs.into_iter().collect()
    }

    /// Start watching application directories for changes
    fn start_watching(
        apps: Arc<RwLock<Vec<AppEntry>>>,
        extra_dirs: &[PathBuf],
    ) -> Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
        let watch_dirs = Self::get_watch_directories(extra_dirs);

        if watch_dirs.is_empty() {
            warn!("No application directories found to watch");
            return None;
        }

        // Clone extra_dirs for the closure
        let extra_dirs_clone = extra_dirs.to_vec();

        // Create debounced watcher with 500ms timeout
        let debouncer_result = new_debouncer(
            Duration::from_millis(500),
            move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
                match result {
                    Ok(events) => {
                    // Check if any event is relevant (.desktop file change)
                    let dominated_path = events.iter().any(|e| {
                        matches!(e.kind, DebouncedEventKind::Any | DebouncedEventKind::AnyContinuous)
                            && e.path
                                .extension()
                                .map(|ext| ext == "desktop")
                                .unwrap_or(false)
                    });

                    if dominated_path {
                        debug!("Desktop file change detected, reloading applications");
                        Self::load_applications_into(&apps, &extra_dirs_clone);
                    }
                }
                Err(e) => {
                    error!("File watcher error: {:?}", e);
                }
            }
        });

        match debouncer_result {
            Ok(mut debouncer) => {
                // Watch all directories
                for dir in &watch_dirs {
                    match debouncer
                        .watcher()
                        .watch(dir, notify::RecursiveMode::NonRecursive)
                    {
                        Ok(()) => {
                            info!("Watching for application changes: {:?}", dir);
                        }
                        Err(e) => {
                            warn!("Failed to watch {:?}: {}", dir, e);
                        }
                    }
                }

                Some(debouncer)
            }
            Err(e) => {
                error!("Failed to create file watcher: {}. Application index will not auto-update.", e);
                None
            }
        }
    }

    /// Load all desktop entries from XDG directories into the provided cache
    fn load_applications_into(apps: &Arc<RwLock<Vec<AppEntry>>>, extra_dirs: &[PathBuf]) {
        let mut entries = Vec::new();

        // Collect all paths to scan
        let mut all_paths: Vec<PathBuf> = Iter::new(freedesktop_desktop_entry::default_paths()).collect();

        // Add extra directories
        for dir in extra_dirs {
            if dir.is_dir() {
                if let Ok(read_dir) = std::fs::read_dir(dir) {
                    for entry in read_dir.flatten() {
                        let path = entry.path();
                        if path.extension().map(|e| e == "desktop").unwrap_or(false) {
                            all_paths.push(path);
                        }
                    }
                }
            }
        }

        // Parse all desktop entries
        for path in all_paths {
            match DesktopEntry::from_path::<&str>(&path, None) {
                Ok(entry) => {
                    // Skip entries marked as hidden or no-display
                    if entry.no_display() {
                        continue;
                    }

                    // Empty slice for default locale
                    let locales: &[&str] = &[];

                    // Skip entries without a name
                    let name = match entry.name(locales) {
                        Some(n) => n.to_string(),
                        None => continue,
                    };

                    // Skip entries without an exec command (not launchable)
                    if entry.exec().is_none() {
                        continue;
                    }

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
                        icon: entry.icon().unwrap_or("application-x-executable").to_string(),
                        keywords: entry
                            .keywords(locales)
                            .map(|k| k.into_iter().map(String::from).collect())
                            .unwrap_or_default(),
                        terminal: entry.terminal(),
                        path: path.clone(),
                        launch_count: 0,
                    };

                    entries.push(app);
                }
                Err(e) => {
                    // Only log at debug level - many .desktop files have minor parsing issues
                    // (e.g., localized keys without default values) that don't affect functionality
                    debug!("Failed to read desktop entry {:?}: {}", path, e);
                }
            }
        }

        info!("Loaded {} applications", entries.len());

        if let Ok(mut guard) = apps.write() {
            *guard = entries;
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
                    .with_metadata("desktop_id", &app.id)
                    .with_metadata("terminal", if app.terminal { "true" } else { "false" })
            })
            .collect()
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
}
