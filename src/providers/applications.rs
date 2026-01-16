//! Applications provider - searches installed desktop applications
//!
//! Uses incremental updates for efficient file watching - only the changed
//! .desktop file is parsed/removed rather than reloading all applications.

use super::{Item, Provider};
use freedesktop_desktop_entry::DesktopEntry;
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

/// Source type for an application
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppSource {
    Native,
    Flatpak,
    Snap,
}

impl AppSource {
    fn as_str(&self) -> &'static str {
        match self {
            AppSource::Native => "native",
            AppSource::Flatpak => "flatpak",
            AppSource::Snap => "snap",
        }
    }

    /// Determine the source based on the .desktop file path
    fn from_path(path: &Path) -> Self {
        let path_str = path.to_string_lossy();
        if path_str.contains("/flatpak/") {
            AppSource::Flatpak
        } else if path_str.contains("/snapd/") {
            AppSource::Snap
        } else {
            AppSource::Native
        }
    }
}

/// A cached application entry
#[derive(Debug, Clone)]
struct AppEntry {
    /// Desktop entry ID (filename without .desktop)
    id: String,
    /// Full path to the .desktop file (for file watcher updates)
    path: PathBuf,
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
    /// Source of the application (native, flatpak, snap)
    source: AppSource,
}

/// Provider for installed applications
pub struct ApplicationsProvider {
    /// Cached application entries, keyed by Desktop Entry ID for XDG deduplication
    /// Per XDG spec, entries with the same ID from higher-priority directories override lower ones
    apps: Arc<RwLock<HashMap<String, AppEntry>>>,
    /// Reverse lookup: path -> Desktop Entry ID (for efficient file watcher updates)
    #[allow(dead_code)]
    path_to_id: Arc<RwLock<HashMap<PathBuf, String>>>,
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
        let path_to_id = Arc::new(RwLock::new(HashMap::new()));

        // Load applications initially
        Self::load_applications_into(&apps, &path_to_id, &extra_dirs);

        // Set up file watching
        let watcher = Self::start_watching(Arc::clone(&apps), Arc::clone(&path_to_id), &extra_dirs);

        Self {
            apps,
            path_to_id,
            matcher: SkimMatcherV2::default(),
            extra_dirs,
            watcher,
        }
    }

    /// Get directories in XDG precedence order (highest priority first)
    ///
    /// Per the XDG Base Directory Specification:
    /// 1. $XDG_DATA_HOME (default: ~/.local/share) - user overrides
    /// 2. $XDG_DATA_DIRS (default: /usr/local/share:/usr/share) - in listed order
    ///
    /// We extend this with:
    /// 3. Flatpak user directory (~/.local/share/flatpak/exports/share/applications)
    /// 4. Flatpak system directory (/var/lib/flatpak/exports/share/applications)
    /// 5. Snap directory (/var/lib/snapd/desktop/applications)
    /// 6. Extra directories from config (lowest priority)
    fn get_directories_in_precedence_order(extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        // 1. XDG_DATA_HOME/applications (highest priority - user overrides)
        let xdg_data_home = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"));
        let user_apps = xdg_data_home.join("applications");
        if user_apps.is_dir() {
            dirs.push(user_apps);
        }

        // 2. XDG_DATA_DIRS/applications (in order - /usr/local/share before /usr/share)
        let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
        for data_dir in xdg_data_dirs.split(':') {
            if data_dir.is_empty() {
                continue;
            }
            let apps_dir = PathBuf::from(data_dir).join("applications");
            if apps_dir.is_dir() && !dirs.contains(&apps_dir) {
                dirs.push(apps_dir);
            }
        }

        // 3. Flatpak user directory (user flatpak apps override system flatpak)
        let flatpak_user = home.join(".local/share/flatpak/exports/share/applications");
        if flatpak_user.is_dir() && !dirs.contains(&flatpak_user) {
            dirs.push(flatpak_user);
        }

        // 4. Flatpak system directory
        let flatpak_system = PathBuf::from("/var/lib/flatpak/exports/share/applications");
        if flatpak_system.is_dir() && !dirs.contains(&flatpak_system) {
            dirs.push(flatpak_system);
        }

        // 5. Snap directory
        let snap_apps = PathBuf::from("/var/lib/snapd/desktop/applications");
        if snap_apps.is_dir() && !dirs.contains(&snap_apps) {
            dirs.push(snap_apps);
        }

        // 6. Extra directories from config (lowest priority)
        for dir in extra_dirs {
            if dir.is_dir() && !dirs.contains(dir) {
                dirs.push(dir.clone());
            }
        }

        dirs
    }

    /// Get the priority of a directory (lower number = higher priority)
    /// Returns None if the path is not in a known applications directory
    fn get_directory_priority(path: &Path, extra_dirs: &[PathBuf]) -> Option<usize> {
        let parent = path.parent()?;
        let ordered_dirs = Self::get_directories_in_precedence_order(extra_dirs);
        ordered_dirs.iter().position(|d| d == parent)
    }

    /// Get all directories that should be watched for .desktop files
    /// Returns (existing_dirs, potential_dirs) where potential_dirs are parent
    /// directories that should be watched for new application directories to appear
    fn get_watch_directories(extra_dirs: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut dirs = HashSet::new();
        let mut parent_dirs = HashSet::new();

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

        // Flatpak directories - watch even if they don't exist yet
        let flatpak_system = PathBuf::from("/var/lib/flatpak/exports/share/applications");
        let flatpak_system_parent = PathBuf::from("/var/lib/flatpak/exports/share");
        if flatpak_system.is_dir() {
            dirs.insert(flatpak_system);
        } else if flatpak_system_parent.is_dir() {
            // Parent exists but applications dir doesn't - watch parent for it to be created
            parent_dirs.insert(flatpak_system_parent);
        }

        let flatpak_user = home.join(".local/share/flatpak/exports/share/applications");
        let flatpak_user_parent = home.join(".local/share/flatpak/exports/share");
        if flatpak_user.is_dir() {
            dirs.insert(flatpak_user);
        } else if flatpak_user_parent.is_dir() {
            parent_dirs.insert(flatpak_user_parent);
        }

        // Snap directory
        let snap_apps = PathBuf::from("/var/lib/snapd/desktop/applications");
        let snap_parent = PathBuf::from("/var/lib/snapd/desktop");
        if snap_apps.is_dir() {
            dirs.insert(snap_apps);
        } else if snap_parent.is_dir() {
            parent_dirs.insert(snap_parent);
        }

        // Extra directories from config
        for dir in extra_dirs {
            if dir.is_dir() {
                dirs.insert(dir.clone());
            }
        }

        (dirs.into_iter().collect(), parent_dirs.into_iter().collect())
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

        // Explicit flatpak icon fallback - in case XDG_DATA_DIRS isn't set correctly
        // This ensures flatpak icons are found even without proper environment
        if let Some(path) = Self::resolve_flatpak_icon(icon) {
            return Some(path);
        }

        None
    }

    /// Explicitly check flatpak icon directories
    /// Fallback for when XDG_DATA_DIRS doesn't include flatpak paths
    fn resolve_flatpak_icon(icon: &str) -> Option<String> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        // Flatpak icon directories (system and user)
        let flatpak_icon_bases = [
            PathBuf::from("/var/lib/flatpak/exports/share/icons"),
            home.join(".local/share/flatpak/exports/share/icons"),
        ];

        // Check each flatpak icon location
        for base in &flatpak_icon_bases {
            // Try scalable SVG first (preferred)
            let svg_path = base.join("hicolor/scalable/apps").join(format!("{}.svg", icon));
            if svg_path.exists() {
                return Some(svg_path.to_string_lossy().to_string());
            }

            // Try each size from largest to smallest
            for &size in ICON_SIZES {
                let size_dir = format!("hicolor/{}x{}/apps", size, size);
                for ext in ["svg", "png"] {
                    let path = base.join(&size_dir).join(format!("{}.{}", icon, ext));
                    if path.exists() {
                        return Some(path.to_string_lossy().to_string());
                    }
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
        let source = AppSource::from_path(path);

        Some(AppEntry {
            id,
            path: path.to_path_buf(),
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
            source,
        })
    }

    /// Add a single desktop entry to the cache, respecting XDG override policy
    /// Only adds if no higher-priority entry with the same ID exists
    fn add_entry(
        apps: &Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: &Arc<RwLock<HashMap<PathBuf, String>>>,
        path: &Path,
        extra_dirs: &[PathBuf],
    ) {
        if let Some(entry) = Self::parse_desktop_file(path) {
            let id = entry.id.clone();
            let new_priority = Self::get_directory_priority(path, extra_dirs);

            if let (Ok(mut apps_guard), Ok(mut path_guard)) = (apps.write(), path_to_id.write()) {
                // Check if an entry with this ID already exists
                if let Some(existing) = apps_guard.get(&id) {
                    let existing_priority = Self::get_directory_priority(&existing.path, extra_dirs);

                    // Only replace if new entry has higher priority (lower number)
                    match (new_priority, existing_priority) {
                        (Some(new_p), Some(existing_p)) if new_p < existing_p => {
                            debug!(
                                "Overriding {} from {:?} (priority {}) with {:?} (priority {})",
                                entry.name, existing.path, existing_p, path, new_p
                            );
                            // Remove old path mapping
                            path_guard.remove(&existing.path);
                        }
                        (Some(new_p), Some(existing_p)) => {
                            debug!(
                                "Skipping {} from {:?} (priority {}), higher priority entry exists at {:?} (priority {})",
                                entry.name, path, new_p, existing.path, existing_p
                            );
                            // Still track this path for file watcher purposes
                            path_guard.insert(path.to_path_buf(), id);
                            return;
                        }
                        _ => {
                            // If we can't determine priority, use existing behavior (first wins)
                            debug!(
                                "Skipping {} from {:?}, entry already exists from {:?}",
                                entry.name, path, existing.path
                            );
                            path_guard.insert(path.to_path_buf(), id);
                            return;
                        }
                    }
                }

                debug!("Adding application: {} from {:?}", entry.name, path);
                path_guard.insert(path.to_path_buf(), id.clone());
                apps_guard.insert(id, entry);
            }
        }
    }

    /// Remove a single entry from the cache by path
    /// If a lower-priority entry exists with the same ID, it will be promoted
    fn remove_entry(
        apps: &Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: &Arc<RwLock<HashMap<PathBuf, String>>>,
        path: &Path,
        extra_dirs: &[PathBuf],
    ) {
        if let (Ok(mut apps_guard), Ok(mut path_guard)) = (apps.write(), path_to_id.write()) {
            if let Some(id) = path_guard.remove(path) {
                // Only remove from apps if this was the active entry for this ID
                if let Some(entry) = apps_guard.get(&id) {
                    if entry.path == path {
                        let entry_name = entry.name.clone();
                        apps_guard.remove(&id);
                        debug!("Removed application: {} from {:?}", entry_name, path);

                        // Look for a lower-priority entry to promote
                        // This handles the case where a user override is removed and
                        // the system entry should become active again
                        let candidates: Vec<_> = path_guard
                            .iter()
                            .filter(|(_, entry_id)| *entry_id == &id)
                            .map(|(p, _)| p.clone())
                            .collect();

                        if !candidates.is_empty() {
                            // Find the highest priority candidate
                            let mut best_path: Option<PathBuf> = None;
                            let mut best_priority: Option<usize> = None;

                            for candidate_path in candidates {
                                let priority = Self::get_directory_priority(&candidate_path, extra_dirs);
                                match (priority, best_priority) {
                                    (Some(p), None) => {
                                        best_path = Some(candidate_path);
                                        best_priority = Some(p);
                                    }
                                    (Some(p), Some(bp)) if p < bp => {
                                        best_path = Some(candidate_path);
                                        best_priority = Some(p);
                                    }
                                    _ => {}
                                }
                            }

                            if let Some(promote_path) = best_path {
                                if let Some(entry) = Self::parse_desktop_file(&promote_path) {
                                    debug!(
                                        "Promoting {} from {:?} after removal of higher-priority entry",
                                        entry.name, promote_path
                                    );
                                    apps_guard.insert(id, entry);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Update an existing entry (re-parse and potentially update)
    fn update_entry(
        apps: &Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: &Arc<RwLock<HashMap<PathBuf, String>>>,
        path: &Path,
        extra_dirs: &[PathBuf],
    ) {
        if let Some(entry) = Self::parse_desktop_file(path) {
            let id = entry.id.clone();

            if let (Ok(mut apps_guard), Ok(mut path_guard)) = (apps.write(), path_to_id.write()) {
                // Check if this path is the active entry for this ID
                if let Some(existing) = apps_guard.get(&id) {
                    if existing.path == path {
                        debug!("Updated application: {} from {:?}", entry.name, path);
                        apps_guard.insert(id.clone(), entry);
                        path_guard.insert(path.to_path_buf(), id);
                        return;
                    }
                }

                // Path is not the active entry - just update path_to_id mapping
                // and check if we should override the existing entry
                let new_priority = Self::get_directory_priority(path, extra_dirs);
                let existing_priority = apps_guard
                    .get(&id)
                    .and_then(|e| Self::get_directory_priority(&e.path, extra_dirs));

                match (new_priority, existing_priority) {
                    (Some(new_p), Some(existing_p)) if new_p < existing_p => {
                        debug!("Updated entry now has higher priority, promoting: {} from {:?}", entry.name, path);
                        apps_guard.insert(id.clone(), entry);
                    }
                    _ => {}
                }
                path_guard.insert(path.to_path_buf(), id);
            }
        } else {
            // If parsing fails (e.g., now hidden), remove it
            Self::remove_entry(apps, path_to_id, path, extra_dirs);
        }
    }

    /// Check if a path is a .desktop file
    fn is_desktop_file(path: &Path) -> bool {
        path.extension().map(|e| e == "desktop").unwrap_or(false)
    }

    /// Check if a path is an "applications" directory we care about
    fn is_applications_dir(path: &Path) -> bool {
        path.file_name().map(|n| n == "applications").unwrap_or(false) && path.is_dir()
    }

    /// Scan a directory for .desktop files and add them to the cache
    fn scan_directory(
        apps: &Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: &Arc<RwLock<HashMap<PathBuf, String>>>,
        dir: &Path,
        extra_dirs: &[PathBuf],
    ) {
        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if Self::is_desktop_file(&path) {
                    Self::add_entry(apps, path_to_id, &path, extra_dirs);
                }
            }
        }
    }

    /// Start watching application directories for changes with incremental updates
    fn start_watching(
        apps: Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: Arc<RwLock<HashMap<PathBuf, String>>>,
        extra_dirs: &[PathBuf],
    ) -> Option<RecommendedWatcher> {
        let (watch_dirs, parent_dirs) = Self::get_watch_directories(extra_dirs);

        if watch_dirs.is_empty() && parent_dirs.is_empty() {
            warn!("No application directories found to watch");
            return None;
        }

        // Clone extra_dirs for use in closure
        let extra_dirs_owned: Vec<PathBuf> = extra_dirs.to_vec();

        // Create watcher with event handler for incremental updates
        let watcher_result = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Check if a new "applications" directory was created (e.g., first flatpak install)
                    for path in &event.paths {
                        if Self::is_applications_dir(path) {
                            match event.kind {
                                EventKind::Create(_) => {
                                    info!("New applications directory detected: {:?}", path);
                                    Self::scan_directory(&apps, &path_to_id, path, &extra_dirs_owned);
                                }
                                _ => {}
                            }
                        }
                    }

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
                    // Note: Flatpak .desktop files are symlinks, so we need to handle
                    // both file and symlink events, and check existence for ambiguous cases
                    match event.kind {
                        EventKind::Create(CreateKind::File) |
                        EventKind::Create(CreateKind::Any) => {
                            // File or symlink created - add if it exists and is readable
                            for path in desktop_paths {
                                // Check if path exists (follows symlinks)
                                if path.exists() || path.is_symlink() {
                                    debug!("Desktop file created: {:?}", path);
                                    Self::add_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                                }
                            }
                        }
                        EventKind::Remove(RemoveKind::File) |
                        EventKind::Remove(RemoveKind::Any) => {
                            // File or symlink removed
                            for path in desktop_paths {
                                debug!("Desktop file removed: {:?}", path);
                                Self::remove_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                            }
                        }
                        EventKind::Modify(ModifyKind::Data(_)) |
                        EventKind::Modify(ModifyKind::Any) => {
                            for path in desktop_paths {
                                debug!("Desktop file modified: {:?}", path);
                                Self::update_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                            }
                        }
                        // Handle rename as remove old + add new
                        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                            for path in desktop_paths {
                                debug!("Desktop file renamed from: {:?}", path);
                                Self::remove_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                            for path in desktop_paths {
                                debug!("Desktop file renamed to: {:?}", path);
                                Self::add_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                            // Both paths in event.paths: [old, new]
                            if event.paths.len() >= 2 {
                                let old_path = &event.paths[0];
                                let new_path = &event.paths[1];
                                if Self::is_desktop_file(old_path) {
                                    debug!("Desktop file renamed from: {:?}", old_path);
                                    Self::remove_entry(&apps, &path_to_id, old_path, &extra_dirs_owned);
                                }
                                if Self::is_desktop_file(new_path) {
                                    debug!("Desktop file renamed to: {:?}", new_path);
                                    Self::add_entry(&apps, &path_to_id, new_path, &extra_dirs_owned);
                                }
                            }
                        }
                        // Catch-all for other create events
                        EventKind::Create(_) => {
                            for path in desktop_paths {
                                if path.exists() || path.is_symlink() {
                                    debug!("Desktop file created (generic): {:?}", path);
                                    Self::add_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                                }
                            }
                        }
                        // Catch-all for other remove events
                        EventKind::Remove(_) => {
                            for path in desktop_paths {
                                debug!("Desktop file removed (generic): {:?}", path);
                                Self::remove_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                            }
                        }
                        // Catch-all for other modify events - check existence to determine action
                        EventKind::Modify(_) => {
                            for path in desktop_paths {
                                if path.exists() {
                                    debug!("Desktop file modified (generic): {:?}", path);
                                    Self::update_entry(&apps, &path_to_id, path, &extra_dirs_owned);
                                } else {
                                    debug!("Desktop file no longer exists: {:?}", path);
                                    Self::remove_entry(&apps, &path_to_id, path, &extra_dirs_owned);
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
                // Watch existing application directories
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

                // Watch parent directories for new application directories to appear
                for dir in &parent_dirs {
                    match watcher.watch(dir, notify::RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            info!("Watching for new application directories in: {:?}", dir);
                        }
                        Err(e) => {
                            warn!("Failed to watch parent {:?}: {}", dir, e);
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
    /// Processes directories in XDG precedence order so higher-priority entries override lower ones
    fn load_applications_into(
        apps: &Arc<RwLock<HashMap<String, AppEntry>>>,
        path_to_id: &Arc<RwLock<HashMap<PathBuf, String>>>,
        extra_dirs: &[PathBuf],
    ) {
        let mut entries: HashMap<String, AppEntry> = HashMap::new();
        let mut path_map: HashMap<PathBuf, String> = HashMap::new();

        // Get directories in precedence order (highest priority first)
        let ordered_dirs = Self::get_directories_in_precedence_order(extra_dirs);

        info!(
            "Loading applications from {} directories in XDG precedence order",
            ordered_dirs.len()
        );
        for (priority, dir) in ordered_dirs.iter().enumerate() {
            debug!("  Priority {}: {:?}", priority, dir);
        }

        // Process directories in order - first directory wins for each ID
        for dir in &ordered_dirs {
            if let Ok(read_dir) = std::fs::read_dir(dir) {
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    if Self::is_desktop_file(&path) {
                        if let Some(app) = Self::parse_desktop_file(&path) {
                            let id = app.id.clone();

                            // Always track the path -> id mapping for file watcher
                            path_map.insert(path.clone(), id.clone());

                            // Only insert if no higher-priority entry exists
                            if !entries.contains_key(&id) {
                                debug!("Adding {} from {:?}", app.name, path);
                                entries.insert(id, app);
                            } else {
                                debug!(
                                    "Skipping {} from {:?} - higher priority entry already exists",
                                    app.name, path
                                );
                            }
                        }
                    }
                }
            }
        }

        info!(
            "Loaded {} unique applications (from {} total desktop files)",
            entries.len(),
            path_map.len()
        );

        if let (Ok(mut apps_guard), Ok(mut path_guard)) = (apps.write(), path_to_id.write()) {
            *apps_guard = entries;
            *path_guard = path_map;
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
                        .with_source(app.source.as_str())
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
                    .with_source(app.source.as_str())
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
