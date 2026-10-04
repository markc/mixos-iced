//! The one error type for parsing, encoding and the serde bridge.

use std::fmt;

/// What went wrong. Callers that classify failures (an editor diagnostic,
/// a scene linter) match on this instead of on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Characters that form no token: an unterminated string, a bad escape,
    /// a stray byte.
    Lex,
    /// Tokens in an order the grammar does not allow: a missing `}`, a key
    /// without a value.
    Syntax,
    /// An executable construct in a data file: a variable, a call, a
    /// command substitution, interpolation, a semicolon.
    Violation,
    /// The same key twice in one map.
    DuplicateKey,
    /// Lists or maps nested deeper than [`crate::MAX_DEPTH`].
    Depth,
    /// The file could not be read.
    Io,
    /// A value with no strict-data spelling (a non-finite number).
    Encode,
    /// A Rust value could not be turned into data.
    Serialize,
    /// Data did not fit the requested Rust type.
    Deserialize,
}

/// A strict-data error with its kind, source position and, for a
/// violation, the hint that tells the author what to write instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    position: Option<(usize, usize)>,
    message: String,
    hint: Option<String>,
}

/// `std::result::Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            position: None,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn at(mut self, line: usize, column: usize) -> Self {
        self.position = Some((line, column));
        self
    }

    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub(crate) fn lex(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Lex, message).at(line, column)
    }

    pub(crate) fn syntax(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Syntax, message).at(line, column)
    }

    /// `construct` names the refused production; `hint` says what is allowed.
    pub(crate) fn violation(
        line: usize,
        column: usize,
        construct: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self::new(ErrorKind::Violation, construct)
            .at(line, column)
            .with_hint(hint)
    }

    pub(crate) fn duplicate_key(line: usize, column: usize, key: &str) -> Self {
        Self::new(ErrorKind::DuplicateKey, format!("duplicate map key `{key}`"))
            .at(line, column)
            .with_hint("data files have one source of truth per key; remove or rename one")
    }

    pub(crate) fn depth(line: usize, column: usize, limit: usize) -> Self {
        Self::new(ErrorKind::Depth, format!("nesting too deep (limit {limit})")).at(line, column)
    }

    pub(crate) fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub(crate) fn encode(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Encode, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// 1-based source line, when the error has a position.
    pub fn line(&self) -> Option<usize> {
        self.position.map(|(line, _)| line)
    }

    /// 1-based source column (in characters), when the error has a position.
    pub fn column(&self) -> Option<usize> {
        self.position.map(|(_, column)| column)
    }

    /// The message without the position prefix. For a violation this names
    /// the refused construct.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// What to write instead; set for violations and duplicate keys.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Violation | ErrorKind::DuplicateKey => {
                let line = self.line().unwrap_or(0);
                write!(
                    f,
                    "Strict-data violation at line {line}: {} not allowed in data files.",
                    self.message
                )?;
                if let Some(hint) = &self.hint {
                    write!(f, " {hint}")?;
                }
                Ok(())
            }
            ErrorKind::Lex | ErrorKind::Syntax | ErrorKind::Depth => match self.position {
                Some((line, column)) => {
                    write!(f, "Parse error at line {line}:{column}: {}", self.message)
                }
                None => write!(f, "Parse error: {}", self.message),
            },
            ErrorKind::Encode => write!(f, "Data encode error: {}", self.message),
            ErrorKind::Io | ErrorKind::Serialize | ErrorKind::Deserialize => {
                f.write_str(&self.message)
            }
        }
    }
}

impl std::error::Error for Error {}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::new(ErrorKind::Deserialize, msg.to_string())
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::new(ErrorKind::Serialize, msg.to_string())
    }
}
