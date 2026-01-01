//! Calculator provider - evaluates mathematical expressions

use super::{Item, Provider};
use meval::eval_str;
use std::future::Future;
use std::pin::Pin;
use tracing::debug;

/// Provider for mathematical calculations
pub struct CalculatorProvider;

impl CalculatorProvider {
    pub fn new() -> Self {
        Self
    }

    fn query_impl(&self, query: &str, _max_results: usize) -> Vec<Item> {
        // Remove the prefix if present
        let expr = query.strip_prefix('=').unwrap_or(query).trim();

        if expr.is_empty() {
            return vec![Item::new("Enter an expression (e.g., 2+2)", "calculator")
                .with_subtext("Supports: +, -, *, /, ^, sqrt(), sin(), cos(), tan(), log(), etc.")
                .with_icon("accessories-calculator")
                .with_score(1.0)];
        }

        // Try to evaluate the expression
        match eval_str(expr) {
            Ok(result) => {
                let result_str = format_result(result);
                debug!("Calculator: {} = {}", expr, result_str);

                vec![Item::new(&result_str, "calculator")
                    .with_subtext(format!("{} =", expr))
                    .with_icon("accessories-calculator")
                    .with_score(1.0)
                    .with_metadata("expression", expr)
                    .with_metadata("result", &result_str)]
            }
            Err(e) => {
                debug!("Calculator error for '{}': {}", expr, e);
                vec![Item::new("Invalid expression", "calculator")
                    .with_subtext(format!("Error: {}", e))
                    .with_icon("dialog-error")
                    .with_score(0.5)]
            }
        }
    }
}

impl Default for CalculatorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for CalculatorProvider {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Evaluate mathematical expressions"
    }

    fn prefix(&self) -> Option<&str> {
        Some("=")
    }

    fn query(
        &self,
        query: &str,
        max_results: usize,
    ) -> Pin<Box<dyn Future<Output = Vec<Item>> + Send + '_>> {
        let result = self.query_impl(query, max_results);
        Box::pin(async move { result })
    }
}

/// Format a floating point result nicely
fn format_result(value: f64) -> String {
    if value.is_infinite() {
        if value.is_sign_positive() {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else if value.is_nan() {
        "NaN".to_string()
    } else if value.fract() == 0.0 && value.abs() < 1e15 {
        // Display as integer if it's a whole number
        format!("{}", value as i64)
    } else {
        // Display with reasonable precision
        let formatted = format!("{:.10}", value);
        // Remove trailing zeros
        let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_result() {
        assert_eq!(format_result(42.0), "42");
        assert_eq!(format_result(3.14159), "3.14159");
        assert_eq!(format_result(f64::INFINITY), "Infinity");
        assert_eq!(format_result(f64::NEG_INFINITY), "-Infinity");
    }
}
