//! `expr`: the pure expression language of Mix Scenes bindings.
//!
//! A scene document binds a port to an expression (`= $model.rows`,
//! `= not $model.volume.has_icon`, `= $model.prefix .. $item.cells[0]`).
//! This crate compiles such an expression once, reports which globals it
//! reads, and evaluates it over JSON values under size and time limits.
//!
//! The language is the expression subset of Mix with every builtin
//! denied, and it has no dependency on the Mix interpreter:
//!
//! - literals: numbers (`42`, `2.5`, `1e3`, `0xFF`, `0o755`, `0b101`),
//!   `'raw'` and `"escaped ${interpolated}"` strings, `true`, `false`,
//!   `nil`, `[lists]` and `{maps: 1}`; a bare word is a string;
//! - `$name` globals with `.field` and `[index]` access; a missing field or
//!   out-of-range index is nil, and a map key `"*"` is the fallback for a
//!   missing key;
//! - unary `-`, `not` and `!`; arithmetic `+ - * / % **` over f64 with
//!   numeric-string and bool coercion; `..` concatenation with Mix's text
//!   forms (`1 .. 2` is `"12"`, nil is `"nil"`); comparisons `== != < > <=
//!   >= eq ne`; short-circuit `and`, `or` and `??`, which return the
//!   deciding operand; `cond ? a : b`; `if c then a elif d then b else e
//!   end`.
//!
//! Anything that could call out, loop, assign or run a statement is
//! refused when compiling, with [`ErrorKind::NotAllowed`]: function calls
//! of any form (`time()`, `$m.f()`, `$f(1)`), function literals, `send`,
//! `sh`, `$(...)`, assignment, `&&`/`||` chaining, pipes, statement
//! keywords (`for`, `print`, `export`, ...), a leading `~` in a string,
//! and more than one statement in an `if` branch. Heredoc strings are not
//! supported (a syntax error).
//!
//! Evaluation errors are [`ErrorKind::Runtime`] (`division by zero`,
//! `modulo by zero`, `cannot use 'ab' as number`, `cannot compare 'abc'
//! as number`, `undefined variable '$x'`, `cannot access field 'f' on
//! number`, `cannot index nil with number`, and the `+`/`==` type errors
//! on collections). Exceeding a [`Limits`] value is [`ErrorKind::Budget`].

mod analysis;
mod ast;
mod eval;
mod lexer;
mod parser;
mod value;

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

pub use serde_json::Value;

/// What went wrong, for the caller's own classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The source does not parse as one expression.
    Syntax,
    /// The source parses, but uses a construct a binding may not use.
    NotAllowed,
    /// Evaluation failed on the values it met.
    Runtime,
    /// A [`Limits`] bound was exceeded.
    Budget,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorKind::Syntax => "syntax",
            ErrorKind::NotAllowed => "not allowed",
            ErrorKind::Runtime => "runtime",
            ErrorKind::Budget => "budget",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error { kind, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Bounds on one evaluation. Every field is optional; the default has no
/// bounds at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Longest string (in bytes) a concatenation or interpolation may build.
    pub max_string_len: Option<usize>,
    /// Longest list a list literal may build, nested values included.
    pub max_list_len: Option<usize>,
    /// Largest map a map literal may build, nested values included.
    pub max_map_len: Option<usize>,
    /// Wall-clock budget. Checked when each node is entered and once more
    /// when the result is ready, so a result finished late is refused.
    pub time_limit: Option<Duration>,
    /// Most nodes one evaluation may enter.
    pub max_steps: Option<usize>,
}

/// A compiled expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    source: String,
    root: ast::Node,
    reads: BTreeSet<String>,
    roots: BTreeSet<String>,
}

/// Compile one expression. The result is reusable across evaluations.
pub fn compile(source: &str) -> Result<Expr, Error> {
    let root = parser::parse(source)?;
    let depth = analysis::depth(&root);
    if depth > analysis::MAX_DEPTH {
        return Err(Error::new(
            ErrorKind::Syntax,
            format!("expression nesting exceeds {} levels", analysis::MAX_DEPTH),
        ));
    }
    let reads = analysis::reads(&root);
    let roots = reads
        .iter()
        .map(|path| path.split('.').next().unwrap_or(path).to_owned())
        .collect();
    Ok(Expr { source: source.to_owned(), root, reads, roots })
}

/// Compile and evaluate in one step.
pub fn eval(source: &str, globals: &[(&str, &Value)], limits: &Limits) -> Result<Value, Error> {
    compile(source)?.eval(globals, limits)
}

impl Expr {
    /// The text this expression was compiled from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The names of the globals the expression reads (`model`, `item`, ...).
    pub fn roots(&self) -> &BTreeSet<String> {
        &self.roots
    }

    /// The dotted paths the expression reads, one per access chain:
    /// `$model.a.b .. $model.c` reads `model.a.b` and `model.c`; a chain
    /// stops at its first index, so `$model.m[$model.k].x` reads `model.m`
    /// and `model.k`. A host uses these to decide which bindings a model
    /// patch dirties.
    pub fn reads(&self) -> &BTreeSet<String> {
        &self.reads
    }

    /// Evaluate over `globals`, each `$name` bound to its value. A global
    /// not in the list is an undefined variable (positional names such as
    /// `$1` read as nil).
    pub fn eval(&self, globals: &[(&str, &Value)], limits: &Limits) -> Result<Value, Error> {
        eval::Evaluator::run(&self.root, globals, limits)
    }
}
