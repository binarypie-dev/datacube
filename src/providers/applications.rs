//! Applications provider - searches installed desktop applications
//!
//! Uses incremental updates for efficient file watching - only the changed
//! .desktop file is parsed/removed rather than reloading all applications.

use super::{Item, Provider};
use freedesktop_desktop_entry::{DesktopEntry, Iter};
use freedesktop_icons::lookup;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use notify::{
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
    EventKind, RecommendedWatcher, Watcher,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Standard icon sizes to search (largest first)
const ICON_SIZES: &[u16] = &[512, 256, 128, 96, 64, 48, 32, 24, 22, 16];

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
    /// Icon name (from .desktop file)
    icon: String,
    /// Resolved icon file path (SVG or largest PNG)
    icon_path: Option<String>,
    /// Keywords for searching
    keywords: Vec<String>,
    /// Whether this is a terminal app
    terminal: bool,
    /// Launch count for ranking
    launch_count: u32,
}

/// Provider for installed applications
pub struct ApplicationsProvider {
    /// Cached application entries, keyed by path for O(1) lookup
    apps: Arc<RwLock<HashMap<PathBuf, AppEntry>>>,
    /// Fuzzy matcher
    matcher: SkimMatcherV2,
    /// Extra directories to scan (from config)
    #[allow(dead_code)]
    extra_dirs: Vec<PathBuf>,
    /// Keep watcher alive - dropping it stops watching
    #[allow(dead_code)]
    watcher: Option<RecommendedWatcher>,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        Self::with_extra_dirs(Vec::new())
    }

    pub fn with_extra_dirs(extra_dirs: Vec<PathBuf>) -> Self {
        let apps = Arc::new(RwLock::new(HashMap::new()));

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

    /// Resolve an icon name to a file path
    /// Prefers SVG, then falls back to the largest available PNG
    fn resolve_icon_path(icon: &str) -> Option<String> {
        // If it's already an absolute path, use it directly
        let icon_path = Path::new(icon);
        if icon_path.is_absolute() {
            if icon_path.exists() {
                return Some(icon.to_string());
            }
            // Absolute path but doesn't exist - try theme lookup anyway
        }

        // Try to find SVG first (scalable)
        if let Some(path) = lookup(icon).with_scale(1).with_theme("hicolor").find() {
            if path.extension().map(|e| e == "svg").unwrap_or(false) {
                return Some(path.to_string_lossy().to_string());
            }
        }

        // Try each size from largest to smallest to find the best PNG
        for &size in ICON_SIZES {
            if let Some(path) = lookup(icon).with_size(size).with_scale(1).with_theme("hicolor").find() {
                return Some(path.to_string_lossy().to_string());
            }
        }

        // Try without specifying theme (uses system default)
        for &size in ICON_SIZES {
            if let Some(path) = lookup(icon).with_size(size).with_scale(1).find() {
                return Some(path.to_string_lossy().to_string());
            }
        }

        // Check common fallback locations
        let fallback_dirs = [
            "/usr/share/pixmaps",
            "/usr/share/icons",
        ];

        for dir in fallback_dirs {
            for ext in ["svg", "png", "xpm"] {
                let path = PathBuf::from(dir).join(format!("{}.{}", icon, ext));
                if path.exists() {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }

        None
    }

    /// Parse a single .desktop file into an AppEntry
    fn parse_desktop_file(path: &Path) -> Option<AppEntry> {
        let entry = match DesktopEntry::from_path::<&str>(path, None) {
            Ok(e) => e,
            Err(e) => {
                debug!("Failed to read desktop entry {:?}: {}", path, e);
                return None;
            }
        };

        // Skip entries marked as hidden or no-display
        if entry.no_display() {
            return None;
        }

        // Empty slice for default locale
        let locales: &[&str] = &[];

        // Skip entries without a name
        let name = entry.name(locales)?.to_string();

        // Skip entries without an exec command (not launchable)
        if entry.exec().is_none() {
            return None;
        }

        // Get the desktop file ID (filename without extension)
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let icon = entry.icon().unwrap_or("application-x-executable").to_string();
        let icon_path = Self::resolve_icon_path(&icon);

        Some(AppEntry {
            id,
            name,
            generic_name: entry.generic_name(locales).map(|s| s.to_string()),
            comment: entry.comment(locales).map(|s| s.to_string()),
            icon,
            icon_path,
            keywords: entry
                .keywords(locales)
                .map(|k| k.into_iter().map(String::from).collect())
                .unwrap_or_default(),
            terminal: entry.terminal(),
            launch_count: 0,
        })
    }

    /// Add a single desktop entry to the cache
    fn add_entry(apps: &Arc<RwLock<HashMap<PathBuf, AppEntry>>>, path: &Path) {
        if let Some(entry) = Self::parse_desktop_file(path) {
            debug!("Adding application: {} from {:?}", entry.name, path);
            if let Ok(mut guard) = apps.write() {
                guard.insert(path.to_path_buf(), entry);
            }
        }
    }

    /// Remove a single entry from the cache by path
    fn remove_entry(apps: &Arc<RwLock<HashMap<PathBuf, AppEntry>>>, path: &Path) {
        if let Ok(mut guard) = apps.write() {
            if let Some(entry) = guard.remove(path) {
                debug!("Removed application: {} from {:?}", entry.name, path);
            }
        }
    }

    /// Update an existing entry (remove + add)
    fn update_entry(apps: &Arc<RwLock<HashMap<PathBuf, AppEntry>>>, path: &Path) {
        // For updates, we just re-parse and insert (HashMap will replace)
        if let Some(entry) = Self::parse_desktop_file(path) {
            debug!("Updated application: {} from {:?}", entry.name, path);
            if let Ok(mut guard) = apps.write() {
                guard.insert(path.to_path_buf(), entry);
            }
        } else {
            // If parsing fails (e.g., now hidden), remove it
            Self::remove_entry(apps, path);
        }
    }

    /// Check if a path is a .desktop file
    fn is_desktop_file(path: &Path) -> bool {
        path.extension().map(|e| e == "desktop").unwrap_or(false)
    }

    /// Start watching application directories for changes with incremental updates
    fn start_watching(
        apps: Arc<RwLock<HashMap<PathBuf, AppEntry>>>,
        extra_dirs: &[PathBuf],
    ) -> Option<RecommendedWatcher> {
        let watch_dirs = Self::get_watch_directories(extra_dirs);

        if watch_dirs.is_empty() {
            warn!("No application directories found to watch");
            return None;
        }

        // Create watcher with event handler for incremental updates
        let watcher_result = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Filter to only .desktop files
                    let desktop_paths: Vec<_> = event
                        .paths
                        .iter()
                        .filter(|p| Self::is_desktop_file(p))
                        .collect();

                    if desktop_paths.is_empty() {
                        return;
                    }

                    // Handle each event type appropriately
                    match event.kind {
                        EventKind::Create(CreateKind::File) => {
                            for path in desktop_paths {
                                debug!("Desktop file created: {:?}", path);
                                Self::add_entry(&apps, path);
                            }
                        }
                        EventKind::Remove(RemoveKind::File) => {
                            for path in desktop_paths {
                                debug!("Desktop file removed: {:?}", path);
                                Self::remove_entry(&apps, path);
                            }
                        }
                        EventKind::Modify(ModifyKind::Data(_)) |
                        EventKind::Modify(ModifyKind::Any) => {
                            for path in desktop_paths {
                                debug!("Desktop file modified: {:?}", path);
                                Self::update_entry(&apps, path);
                            }
                        }
                        // Handle rename as remove old + add new
                        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                            for path in desktop_paths {
                                debug!("Desktop file renamed from: {:?}", path);
                                Self::remove_entry(&apps, path);
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                            for path in desktop_paths {
                                debug!("Desktop file renamed to: {:?}", path);
                                Self::add_entry(&apps, path);
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                            // Both paths in event.paths: [old, new]
                            if event.paths.len() >= 2 {
                                let old_path = &event.paths[0];
                                let new_path = &event.paths[1];
                                if Self::is_desktop_file(old_path) {
                                    debug!("Desktop file renamed from: {:?}", old_path);
                                    Self::remove_entry(&apps, old_path);
                                }
                                if Self::is_desktop_file(new_path) {
                                    debug!("Desktop file renamed to: {:?}", new_path);
                                    Self::add_entry(&apps, new_path);
                                }
                            }
                        }
                        // Catch-all for other create/modify events
                        EventKind::Create(_) => {
                            for path in desktop_paths {
                                if path.exists() {
                                    debug!("Desktop file created (generic): {:?}", path);
                                    Self::add_entry(&apps, path);
                                }
                            }
                        }
                        EventKind::Remove(_) => {
                            for path in desktop_paths {
                                debug!("Desktop file removed (generic): {:?}", path);
                                Self::remove_entry(&apps, path);
                            }
                        }
                        EventKind::Modify(_) => {
                            for path in desktop_paths {
                                if path.exists() {
                                    debug!("Desktop file modified (generic): {:?}", path);
                                    Self::update_entry(&apps, path);
                                } else {
                                    debug!("Desktop file no longer exists: {:?}", path);
                                    Self::remove_entry(&apps, path);
                                }
                            }
                        }
                        _ => {
                            // Access, Other events - ignore
                        }
                    }
                }
                Err(e) => {
                    error!("File watcher error: {:?}", e);
                }
            }
        });

        match watcher_result {
            Ok(mut watcher) => {
                // Watch all directories
                for dir in &watch_dirs {
                    match watcher.watch(dir, notify::RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            info!("Watching for application changes: {:?}", dir);
                        }
                        Err(e) => {
                            warn!("Failed to watch {:?}: {}", dir, e);
                        }
                    }
                }

                Some(watcher)
            }
            Err(e) => {
                error!("Failed to create file watcher: {}. Application index will not auto-update.", e);
                None
            }
        }
    }

    /// Load all desktop entries from XDG directories into the provided cache
    fn load_applications_into(apps: &Arc<RwLock<HashMap<PathBuf, AppEntry>>>, extra_dirs: &[PathBuf]) {
        let mut entries = HashMap::new();

        // Collect all paths to scan
        let mut all_paths: Vec<PathBuf> = Iter::new(freedesktop_desktop_entry::default_paths()).collect();

        // Add extra directories
        for dir in extra_dirs {
            if dir.is_dir() {
                if let Ok(read_dir) = std::fs::read_dir(dir) {
                    for entry in read_dir.flatten() {
                        let path = entry.path();
                        if Self::is_desktop_file(&path) {
                            all_paths.push(path);
                        }
                    }
                }
            }
        }

        // Parse all desktop entries
        for path in all_paths {
            if let Some(app) = Self::parse_desktop_file(&path) {
                entries.insert(path, app);
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
                .values()
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
                        .with_icon_path(app.icon_path.as_deref().unwrap_or(""))
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
            .values()
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
                    .with_icon_path(app.icon_path.as_deref().unwrap_or(""))
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
