//! Calculator provider - evaluates mathematical expressions

use super::{Item, Provider};
use evalexpr::{
    eval_with_context, ContextWithMutableFunctions, ContextWithMutableVariables, Function,
    HashMapContext, Value,
};
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
                .with_subtext(
                    "Supports: +, -, *, /, ^, %, sqrt(), sin(), cos(), tan(), \
                     log(), ln(), and constants pi, e",
                )
                .with_icon("accessories-calculator")
                .with_score(1.0)];
        }

        // evalexpr uses integer division for integer operands (5/2 == 2), which
        // is surprising for a calculator. Coerce bare integer literals to floats
        // so arithmetic behaves like a calculator (5/2 == 2.5).
        let prepared = floatify_int_literals(expr);
        let context = build_context();

        // Try to evaluate the expression
        match eval_with_context(&prepared, &context) {
            Ok(value) => match format_value(&value) {
                Some(result_str) => {
                    debug!("Calculator: {} = {}", expr, result_str);

                    vec![Item::new(&result_str, "calculator")
                        .with_subtext(format!("{} =", expr))
                        .with_icon("accessories-calculator")
                        .with_score(1.0)
                        .with_metadata("expression", expr)
                        .with_metadata("result", &result_str)]
                }
                None => {
                    debug!("Calculator: unsupported result type for '{}'", expr);
                    vec![Item::new("Invalid expression", "calculator")
                        .with_subtext("Error: unsupported result type")
                        .with_icon("dialog-error")
                        .with_score(0.5)]
                }
            },
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

/// Build an evaluation context exposing common math functions and constants
/// under bare names (e.g. `sqrt`, `pi`) for a familiar calculator experience.
///
/// evalexpr only ships these under a `math::` namespace and provides no math
/// constants, so we register the friendly names ourselves.
fn build_context() -> HashMapContext {
    let mut ctx = HashMapContext::new();

    // Constants
    let _ = ctx.set_value("pi".into(), Value::Float(std::f64::consts::PI));
    let _ = ctx.set_value("e".into(), Value::Float(std::f64::consts::E));
    let _ = ctx.set_value("tau".into(), Value::Float(std::f64::consts::TAU));

    // Unary f64 -> f64 functions
    type UnaryFn = fn(f64) -> f64;
    let unary: &[(&str, UnaryFn)] = &[
        ("sqrt", f64::sqrt),
        ("cbrt", f64::cbrt),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
        ("ln", f64::ln),
        ("log10", f64::log10),
        ("log2", f64::log2),
        ("exp", f64::exp),
        ("abs", f64::abs),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("round", f64::round),
    ];
    for &(name, f) in unary {
        let _ = ctx.set_function(
            name.into(),
            Function::new(move |arg| {
                let x = arg.as_number()?;
                Ok(Value::Float(f(x)))
            }),
        );
    }

    // log(x) is base-10; log(base, x) uses an explicit base.
    let _ = ctx.set_function(
        "log".into(),
        Function::new(|arg| match arg.as_fixed_len_tuple(2) {
            Ok(tuple) => {
                let base: f64 = tuple[0].as_number()?;
                let x: f64 = tuple[1].as_number()?;
                Ok(Value::Float(x.log(base)))
            }
            Err(_) => {
                let x: f64 = arg.as_number()?;
                Ok(Value::Float(x.log10()))
            }
        }),
    );

    // pow(base, exp)
    let _ = ctx.set_function(
        "pow".into(),
        Function::new(|arg| {
            let tuple = arg.as_fixed_len_tuple(2)?;
            let base: f64 = tuple[0].as_number()?;
            let exp: f64 = tuple[1].as_number()?;
            Ok(Value::Float(base.powf(exp)))
        }),
    );

    ctx
}

/// Convert an evaluation result into a display string.
/// Returns `None` for result types that have no meaningful textual form here
/// (empty value, tuples).
fn format_value(value: &Value) -> Option<String> {
    match value {
        Value::Float(f) => Some(format_result(*f)),
        Value::Int(i) => Some(i.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Append `.0` to standalone integer literals so evalexpr performs floating
/// point arithmetic (e.g. `5/2` -> `5.0/2.0` -> `2.5`).
///
/// Digits that are part of an identifier (such as the `10` in `log10`) or that
/// already belong to a decimal literal (such as `3.14`) are left untouched.
fn floatify_int_literals(expr: &str) -> String {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::with_capacity(expr.len());
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if !c.is_ascii_digit() {
            out.push(c);
            i += 1;
            continue;
        }

        // A digit run is a number literal (rather than part of an identifier or
        // a decimal fraction) only if the preceding char is not alphanumeric,
        // `_`, or `.`.
        let is_number_literal = match chars.get(i.wrapping_sub(1)) {
            _ if i == 0 => true,
            Some(p) => !(p.is_alphanumeric() || *p == '_' || *p == '.'),
            None => true,
        };

        // Consume the digit run.
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        out.extend(&chars[start..i]);

        // Don't floatify if it's already a decimal (`3.14`) or is immediately
        // followed by identifier characters (an unusual `2x`-style token).
        let next = chars.get(i).copied();
        let followed_by_dot = next == Some('.');
        let followed_by_ident = next.map(|n| n.is_alphabetic() || n == '_').unwrap_or(false);

        if is_number_literal && !followed_by_dot && !followed_by_ident {
            out.push_str(".0");
        }
    }

    out
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

    fn eval(expr: &str) -> Option<String> {
        let prepared = floatify_int_literals(expr);
        let context = build_context();
        evalexpr::eval_with_context(&prepared, &context)
            .ok()
            .and_then(|v| format_value(&v))
    }

    #[test]
    fn test_format_result() {
        assert_eq!(format_result(42.0), "42");
        assert_eq!(format_result(3.14159), "3.14159");
        assert_eq!(format_result(f64::INFINITY), "Infinity");
        assert_eq!(format_result(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn test_floatify() {
        assert_eq!(floatify_int_literals("5/2"), "5.0/2.0");
        assert_eq!(floatify_int_literals("2^10"), "2.0^10.0");
        assert_eq!(floatify_int_literals("3.14"), "3.14");
        assert_eq!(floatify_int_literals("log10(100)"), "log10(100.0)");
        assert_eq!(floatify_int_literals("abs(-5)"), "abs(-5.0)");
        assert_eq!(floatify_int_literals("pi"), "pi");
    }

    #[test]
    fn test_basic_arithmetic() {
        assert_eq!(eval("2+2").as_deref(), Some("4"));
        assert_eq!(eval("5/2").as_deref(), Some("2.5"));
        assert_eq!(eval("2*3.5").as_deref(), Some("7"));
        assert_eq!(eval("(1+2)*3").as_deref(), Some("9"));
        assert_eq!(eval("2^10").as_deref(), Some("1024"));
        assert_eq!(eval("10%3").as_deref(), Some("1"));
    }

    #[test]
    fn test_functions_and_constants() {
        assert_eq!(eval("sqrt(2)").as_deref(), Some("1.4142135624"));
        assert_eq!(eval("abs(-5)").as_deref(), Some("5"));
        assert_eq!(eval("log(100)").as_deref(), Some("2"));
        assert_eq!(eval("log2(8)").as_deref(), Some("3"));
        assert_eq!(eval("floor(3.7)").as_deref(), Some("3"));
        // sin(pi) is ~0
        assert_eq!(eval("round(sin(pi))").as_deref(), Some("0"));
    }

    #[test]
    fn test_invalid() {
        // Unbound functions / unparseable input yield no result.
        assert_eq!(eval("notafunc(2)"), None);
        assert_eq!(eval("2 +"), None);
        // Float division by zero matches the previous (f64) behaviour.
        assert_eq!(eval("1/0").as_deref(), Some("Infinity"));
    }
}
