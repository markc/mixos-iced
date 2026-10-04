//! How a binding's expression is compiled and evaluated.
//!
//! A port value of the form `= expression` is a binding. The scene core
//! does not interpret the expression itself; it asks an [`Evaluator`] to
//! compile it to a [`Compiled`] form, reads which globals (`model`, `item`)
//! and which dotted paths it uses, and later evaluates it against the
//! scene's model and, inside a list template, the current row.
//!
//! [`ExprEvaluator`] is the evaluator every public entry point uses; a
//! host only reaches for the trait to plug in another language.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value as JsonValue;

/// Why compiling or evaluating a binding failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingErrorKind {
    /// The source does not parse as one expression.
    Syntax,
    /// The source parses but uses a construct a binding may not use: a
    /// call, an assignment, a statement, a shell or Bus form.
    NotAllowed,
    /// Evaluation failed on the values it met.
    Runtime,
    /// A [`Limits`] bound was exceeded.
    Budget,
}

/// A compile or evaluation failure with a message for the diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingError {
    pub kind: BindingErrorKind,
    pub message: String,
}

impl BindingError {
    pub fn new(kind: BindingErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BindingError {}

/// Bounds on one evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Longest string (in bytes) the expression may build.
    pub max_string_len: usize,
    /// Longest list the expression may build.
    pub max_list_len: usize,
    /// Largest map the expression may build.
    pub max_map_len: usize,
    /// Wall-clock budget for this one evaluation.
    pub time_limit: Duration,
}

/// Compiles binding sources.
pub trait Evaluator: fmt::Debug + Send + Sync {
    /// Compile one expression. A refusal is [`BindingErrorKind::Syntax`]
    /// or [`BindingErrorKind::NotAllowed`].
    fn compile(&self, source: &str) -> Result<Arc<dyn Compiled>, BindingError>;
}

/// One compiled binding expression.
pub trait Compiled: fmt::Debug + Send + Sync {
    /// The globals the expression reads (`model`, `item`, ...).
    fn roots(&self) -> &BTreeSet<String>;
    /// The dotted paths the expression reads, one per access chain:
    /// `$model.a.b .. $item.cells[0]` reads `model.a.b` and `item.cells`.
    fn reads(&self) -> &BTreeSet<String>;
    /// Evaluate with each global bound to its value. A failure is
    /// [`BindingErrorKind::Runtime`] or [`BindingErrorKind::Budget`].
    fn eval(&self, globals: &[(&str, &JsonValue)], limits: &Limits) -> Result<JsonValue, BindingError>;
}

/// The default evaluator: the `expr` crate, Mix's pure expression subset
/// with every builtin denied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExprEvaluator;

/// The evaluator the crate's own entry points use.
pub const DEFAULT_EVALUATOR: &dyn Evaluator = &ExprEvaluator;

impl Evaluator for ExprEvaluator {
    fn compile(&self, source: &str) -> Result<Arc<dyn Compiled>, BindingError> {
        expr::compile(source).map(|expression| Arc::new(expression) as Arc<dyn Compiled>).map_err(convert)
    }
}

impl Compiled for expr::Expr {
    fn roots(&self) -> &BTreeSet<String> {
        expr::Expr::roots(self)
    }

    fn reads(&self) -> &BTreeSet<String> {
        expr::Expr::reads(self)
    }

    fn eval(&self, globals: &[(&str, &JsonValue)], limits: &Limits) -> Result<JsonValue, BindingError> {
        let limits = expr::Limits {
            max_string_len: Some(limits.max_string_len),
            max_list_len: Some(limits.max_list_len),
            max_map_len: Some(limits.max_map_len),
            time_limit: Some(limits.time_limit),
            max_steps: None,
        };
        expr::Expr::eval(self, globals, &limits).map_err(convert)
    }
}

fn convert(error: expr::Error) -> BindingError {
    let kind = match error.kind {
        expr::ErrorKind::Syntax => BindingErrorKind::Syntax,
        expr::ErrorKind::NotAllowed => BindingErrorKind::NotAllowed,
        expr::ErrorKind::Runtime => BindingErrorKind::Runtime,
        expr::ErrorKind::Budget => BindingErrorKind::Budget,
    };
    BindingError::new(kind, error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> Limits {
        Limits { max_string_len: 1 << 20, max_list_len: 500, max_map_len: 1024, time_limit: Duration::from_millis(50) }
    }

    #[test]
    fn the_default_evaluator_compiles_reads_and_evaluates() {
        let compiled = DEFAULT_EVALUATOR.compile("$model.prefix .. $item.cells[0]").unwrap();
        assert_eq!(compiled.roots().iter().collect::<Vec<_>>(), ["item", "model"]);
        assert_eq!(compiled.reads().iter().collect::<Vec<_>>(), ["item.cells", "model.prefix"]);
        let model = json!({"prefix": "live "});
        let item = json!({"cells": ["one"]});
        let value = compiled.eval(&[("model", &model), ("item", &item)], &limits()).unwrap();
        assert_eq!(value, json!("live one"));
    }

    #[test]
    fn failures_keep_their_kind() {
        assert_eq!(DEFAULT_EVALUATOR.compile("(").unwrap_err().kind, BindingErrorKind::Syntax);
        assert_eq!(DEFAULT_EVALUATOR.compile("time()").unwrap_err().kind, BindingErrorKind::NotAllowed);
        let compiled = DEFAULT_EVALUATOR.compile("1 / 0").unwrap();
        assert_eq!(compiled.eval(&[], &limits()).unwrap_err().kind, BindingErrorKind::Runtime);
        let compiled = DEFAULT_EVALUATOR.compile("'a' .. 'b'").unwrap();
        let tight = Limits { max_string_len: 1, ..limits() };
        assert_eq!(compiled.eval(&[], &tight).unwrap_err().kind, BindingErrorKind::Budget);
    }
}
