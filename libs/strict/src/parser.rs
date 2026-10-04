//! Recursive-descent parser for data literals.
//!
//! A document is one of: an implicit top-level map body (`key: value`
//! pairs separated by newlines or commas), `{ … }`, `[ … ]`, or nothing
//! (an empty map). Inside `{ }` and `[ ]` entries are comma-separated, with
//! an optional trailing comma. Every executable construct the lexer can
//! still produce is refused here with a message naming it.

use crate::MAX_DEPTH;
use crate::error::{Error, Result};
use crate::lexer::{Refused, Spanned, Token};
use crate::value::{Map, Value};

pub(crate) fn parse(tokens: Vec<Spanned>) -> Result<Value> {
    Parser {
        tokens,
        pos: 0,
        depth: 0,
    }
    .document()
}

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).map_or(&Token::Eof, |t| &t.token)
    }

    fn at_end(&self) -> bool {
        matches!(self.peek(), Token::Eof)
    }

    /// Position of the current token.
    fn here(&self) -> (usize, usize) {
        self.tokens
            .get(self.pos)
            .or(self.tokens.last())
            .map_or((0, 0), |t| (t.line, t.column))
    }

    /// Position of the last consumed token: the anchor for "something is
    /// missing after this" messages.
    fn previous(&self) -> (usize, usize) {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map_or((0, 0), |t| (t.line, t.column))
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == token {
            self.advance();
            true
        } else {
            false
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Token::Newline) {
            self.advance();
        }
    }

    fn violation(&self, construct: impl Into<String>, hint: impl Into<String>) -> Error {
        let (line, column) = self.here();
        Error::violation(line, column, construct, hint)
    }

    fn semicolon(&self) -> Error {
        self.violation(
            "semicolon statement separator `;`",
            "strict data uses newline or `,` separators; `;` is executable syntax",
        )
    }

    fn expect(&mut self, token: &Token) -> Result<()> {
        if self.eat(token) {
            return Ok(());
        }
        let (line, column) = self.here();
        Err(Error::syntax(
            line,
            column,
            format!("expected {token}, got {}", self.peek()),
        ))
    }

    fn document(&mut self) -> Result<Value> {
        self.skip_newlines();
        if matches!(self.peek(), Token::Semicolon) {
            return Err(self.semicolon());
        }
        let value = match self.peek() {
            Token::Eof => Value::Map(Map::new()),
            Token::LBrace => self.map()?,
            Token::LBracket => self.list()?,
            _ => self.body()?,
        };
        self.skip_newlines();
        if matches!(self.peek(), Token::Semicolon) {
            return Err(self.semicolon());
        }
        if !self.at_end() {
            return Err(self.violation(
                format!("trailing content ({})", self.peek()),
                "a data file holds a single top-level value (map body, [ list ], or { map })",
            ));
        }
        Ok(value)
    }

    /// One value, depth-guarded so a hostile file cannot overflow the stack.
    fn value(&mut self) -> Result<Value> {
        if self.depth + 1 > MAX_DEPTH {
            let (line, column) = self.here();
            return Err(Error::depth(line, column, MAX_DEPTH));
        }
        self.depth += 1;
        let result = self.value_inner();
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self) -> Result<Value> {
        if matches!(self.peek(), Token::Minus) {
            self.advance();
            return match self.peek().clone() {
                Token::Number(n) => {
                    self.advance();
                    Ok(Value::Number(-n))
                }
                _ => Err(self.violation(
                    "unary minus on non-number",
                    "negation is only valid on a number literal (e.g. -42)",
                )),
            };
        }
        match self.peek().clone() {
            Token::Number(n) => {
                self.advance();
                Ok(Value::Number(n))
            }
            Token::Str(s) => {
                self.advance();
                Ok(Value::String(s))
            }
            Token::Word(word) => self.word_value(word),
            Token::LBracket => self.list(),
            Token::LBrace => self.map(),
            Token::Semicolon => Err(self.semicolon()),
            Token::Interpolated(Refused::Variable) => Err(self.violation(
                "string interpolation",
                "data files use plain strings; precompute the value",
            )),
            Token::Interpolated(Refused::Command) => Err(self.violation(
                "command substitution `$(...)` inside a string",
                "data files cannot run shell commands",
            )),
            Token::Interpolated(Refused::Home) => Err(self.violation(
                "leading `~` home-directory expansion",
                "data files have no home directory; write `\\~/` for a literal tilde",
            )),
            Token::Variable(name) => Err(self.violation(
                format!("variable reference `${name}`"),
                "data files have no variable scope; inline the value",
            )),
            Token::CommandSub => Err(self.violation(
                "command substitution `$(...)`",
                "data files cannot run shell commands",
            )),
            Token::LParen => Err(self.violation(
                "parenthesised expression",
                "data files have no expression precedence to override",
            )),
            other => Err(self.violation(
                format!("unexpected token {other}"),
                "data files only allow scalars (number, string, true, false, nil), [ list ], { map }",
            )),
        }
    }

    /// A bare word in value position: the three literals, a few words that
    /// spell executable constructs and are refused by name, or a plain
    /// string.
    fn word_value(&mut self, word: String) -> Result<Value> {
        match word.as_str() {
            "true" => {
                self.advance();
                Ok(Value::Bool(true))
            }
            "false" => {
                self.advance();
                Ok(Value::Bool(false))
            }
            "nil" => {
                self.advance();
                Ok(Value::Nil)
            }
            "function" | "fn" => Err(self.violation(
                "function literal",
                "data files contain values, not behaviour",
            )),
            "send" => Err(self.violation("send expression", "data files cannot perform Bus IPC")),
            "sh" => Err(self.violation("sh expression", "data files cannot run shell commands")),
            _ => {
                self.advance();
                if matches!(self.peek(), Token::LParen) {
                    self.pos -= 1;
                    return Err(self.violation(
                        format!("function call `{word}(...)`"),
                        "data files cannot invoke functions; use a literal value",
                    ));
                }
                Ok(Value::String(word))
            }
        }
    }

    /// `[ value, value, … ]` with an optional trailing comma.
    fn list(&mut self) -> Result<Value> {
        self.advance();
        let mut items = Vec::new();
        self.skip_newlines();
        if !matches!(self.peek(), Token::RBracket) {
            items.push(self.value()?);
            loop {
                self.skip_newlines();
                if matches!(self.peek(), Token::Semicolon) {
                    return Err(self.semicolon());
                }
                if !self.eat(&Token::Comma) {
                    if !matches!(self.peek(), Token::RBracket) && !self.at_end() {
                        let (line, column) = self.previous();
                        return Err(Error::violation(
                            line,
                            column,
                            format!("missing `,` after this list item (next token: {})", self.peek()),
                            "separate `[ ]` list items with commas; a trailing comma before `]` is fine",
                        ));
                    }
                    break;
                }
                self.skip_newlines();
                if matches!(self.peek(), Token::RBracket) {
                    break;
                }
                items.push(self.value()?);
            }
        }
        self.skip_newlines();
        self.expect(&Token::RBracket)?;
        Ok(Value::List(items))
    }

    /// `{ key: value, … }` with an optional trailing comma. Newlines never
    /// reach the parser inside braces, so a comma is required between
    /// entries: `{ a: 1\n b: 2 }` is a missing comma, not two entries.
    fn map(&mut self) -> Result<Value> {
        self.advance();
        let mut entries = Map::new();
        if !matches!(self.peek(), Token::RBrace) {
            loop {
                let key_at = self.here();
                let key = self.key()?;
                self.expect(&Token::Colon)?;
                let value = self.value()?;
                insert_unique(&mut entries, key, value, key_at)?;
                if matches!(self.peek(), Token::Semicolon) {
                    return Err(self.semicolon());
                }
                if !self.eat(&Token::Comma) {
                    if !matches!(self.peek(), Token::RBrace) && !self.at_end() {
                        let (line, column) = self.previous();
                        return Err(Error::violation(
                            line,
                            column,
                            format!("missing `,` after this map entry (next token: {})", self.peek()),
                            "separate `{ }` map entries with commas; a trailing comma before `}` is fine",
                        ));
                    }
                    break;
                }
                if matches!(self.peek(), Token::RBrace) {
                    break;
                }
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(Value::Map(entries))
    }

    /// The top-level map body: `key: value` pairs separated by newlines or
    /// commas, up to the end of the document.
    fn body(&mut self) -> Result<Value> {
        let mut entries = Map::new();
        loop {
            self.skip_newlines();
            if self.at_end() {
                break;
            }
            let key_at = self.here();
            let key = self.key()?;
            self.expect(&Token::Colon)?;
            self.skip_newlines();
            let value = self.value()?;
            insert_unique(&mut entries, key, value, key_at)?;
            if matches!(self.peek(), Token::Semicolon) {
                return Err(self.semicolon());
            }
            let had_comma = self.eat(&Token::Comma);
            let had_newline = matches!(self.peek(), Token::Newline);
            self.skip_newlines();
            if self.at_end() {
                break;
            }
            if !had_comma && !had_newline {
                return Err(self.violation(
                    format!("missing separator before {}", self.peek()),
                    "top-level map entries are separated by newline or `,`",
                ));
            }
        }
        Ok(Value::Map(entries))
    }

    /// A map key: a bare word or a quoted string.
    fn key(&mut self) -> Result<String> {
        match self.peek().clone() {
            Token::Semicolon => Err(self.semicolon()),
            Token::Word(word) | Token::Str(word) => {
                self.advance();
                Ok(word)
            }
            other => Err(self.violation(
                format!("non-identifier map key {other}"),
                "map keys must be bareword identifiers or quoted strings",
            )),
        }
    }
}

/// Insert, refusing a key that is already present: a data file has one
/// source of truth per key, and silent last-wins would hide a copy-paste
/// mistake.
fn insert_unique(entries: &mut Map, key: String, value: Value, at: (usize, usize)) -> Result<()> {
    if entries.contains_key(&key) {
        return Err(Error::duplicate_key(at.0, at.1, &key));
    }
    entries.insert(key, value);
    Ok(())
}
