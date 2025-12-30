//! Command provider - run arbitrary shell commands

use super::{Action, Item, Provider};
use std::future::Future;
use std::pin::Pin;
use tracing::debug;

/// Provider for running shell commands
pub struct CommandProvider;

impl CommandProvider {
    pub fn new() -> Self {
        Self
    }

    fn query_impl(&self, query: &str, _max_results: usize) -> Vec<Item> {
        // Remove the prefix if present
        let cmd = query.strip_prefix('/').unwrap_or(query).trim();

        if cmd.is_empty() {
            return vec![Item::new("Enter a command to run", "command")
                .with_subtext("e.g., /htop, /systemctl status")
                .with_icon("utilities-terminal")
                .with_score(1.0)];
        }

        // Provide the command as a runnable item
        vec![Item::new(format!("Run: {}", cmd), "command")
            .with_subtext("Execute in terminal")
            .with_icon("utilities-terminal")
            .with_score(1.0)
            .with_exec(cmd.to_string())
            .with_metadata("command", cmd)
            .with_action(Action {
                id: "run".to_string(),
                name: "Run".to_string(),
                icon: "system-run".to_string(),
            })
            .with_action(Action {
                id: "run_terminal".to_string(),
                name: "Run in Terminal".to_string(),
                icon: "utilities-terminal".to_string(),
            })
            .with_action(Action {
                id: "copy".to_string(),
                name: "Copy Command".to_string(),
                icon: "edit-copy".to_string(),
            })]
    }

    fn activate_impl(&self, item: &Item) -> anyhow::Result<()> {
        let cmd = item
            .metadata
            .get("command")
            .map(|s| s.as_str())
            .unwrap_or(&item.exec);

        debug!("Executing command: {}", cmd);

        // Run in terminal using foot
        std::process::Command::new("foot")
            .arg("-e")
            .arg("sh")
            .arg("-c")
            .arg(format!(
                "{}; echo; echo 'Press Enter to close...'; read",
                cmd
            ))
            .spawn()?;

        Ok(())
    }
}

impl Default for CommandProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for CommandProvider {
    fn name(&self) -> &str {
        "command"
    }

    fn description(&self) -> &str {
        "Run shell commands"
    }

    fn prefix(&self) -> Option<&str> {
        Some("/")
    }

    fn query(
        &self,
        query: &str,
        max_results: usize,
    ) -> Pin<Box<dyn Future<Output = Vec<Item>> + Send + '_>> {
        let result = self.query_impl(query, max_results);
        Box::pin(async move { result })
    }

    fn activate(
        &self,
        item: &Item,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let result = self.activate_impl(item);
        Box::pin(async move { result })
    }
}
