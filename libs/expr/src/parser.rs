//! Parser: tokens to a [`Node`] tree, refusing everything that is not a
//! pure expression.
//!
//! Precedence, lowest first: `? :` (right-associative), `or`, `and`,
//! comparisons (`== != < > <= >= eq ne`), `??`, `..`, `+ -`, `* / %`, `**`.
//! All binary operators are left-associative, `**` included. Unary `-`,
//! `not` and `!` bind tighter than any binary operator.

use crate::ast::{BinOp, Coalesce, IfBranch, InterpVar, Node, Part, Segment, UnaryOp};
use crate::lexer::{Lexer, RawPart, Token};
use crate::{Error, ErrorKind};

/// Nesting the parser follows recursively before it refuses the input.
pub(crate) const MAX_NESTING: usize = 200;

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

fn syntax(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Syntax, message)
}

fn not_allowed(what: &str) -> Error {
    Error::new(ErrorKind::NotAllowed, format!("{what} is not allowed in an expression"))
}

/// Mix's name for a statement keyword, for the refusal message.
fn statement_construct(keyword: &str) -> Option<&'static str> {
    Some(match keyword {
        "for" => "for loop",
        "while" => "while loop",
        "loop" => "loop statement",
        "break" => "break statement",
        "continue" => "continue statement",
        "return" => "return statement",
        "select" => "select statement",
        "print" | "eprint" => "print statement",
        "parse" => "parse statement",
        "die" => "die statement",
        "try" => "try statement",
        "export" => "export statement",
        "alias" => "alias statement",
        "address" => "address block",
        "emit" => "emit statement",
        "on" => "on handler registration",
        "source" => "source statement",
        "include" => "include statement",
        _ => return None,
    })
}

/// Parse `source` as exactly one expression.
pub(crate) fn parse(source: &str) -> Result<Node, Error> {
    parse_at_depth(source, 0)
}

fn parse_at_depth(source: &str, depth: usize) -> Result<Node, Error> {
    let tokens = Lexer::new(source).tokenize()?;
    let mut parser = Parser { tokens, pos: 0, depth };
    parser.program()
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn peek_at(&self, offset: usize) -> &Token {
        self.tokens.get(self.pos + offset).unwrap_or(&Token::Eof)
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
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> Result<(), Error> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(syntax(format!(
                "expected {} but found {}",
                token.describe(),
                self.peek().describe()
            )))
        }
    }

    fn skip_separators(&mut self) {
        while matches!(self.peek(), Token::Newline | Token::Semicolon) {
            self.pos += 1;
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Token::Newline) {
            self.pos += 1;
        }
    }

    fn enter(&mut self) -> Result<(), Error> {
        if self.depth >= MAX_NESTING {
            return Err(syntax(format!("nesting too deep (limit {MAX_NESTING})")));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// The whole input: one expression statement and nothing else.
    fn program(&mut self) -> Result<Node, Error> {
        self.skip_separators();
        if matches!(self.peek(), Token::Eof) {
            return Err(syntax("expected an expression"));
        }
        let node = self.statement()?;
        self.skip_separators();
        if !matches!(self.peek(), Token::Eof) {
            return Err(syntax("expected exactly one expression, found more than one statement"));
        }
        Ok(node)
    }

    /// One statement position. Only an expression statement survives;
    /// every other statement form is refused by name, and a trailing
    /// `=`, `&&`, `||` or `|` turns an expression into a refused statement.
    fn statement(&mut self) -> Result<Node, Error> {
        if let Token::Keyword(word) = self.peek() {
            return Err(match statement_construct(word) {
                Some(construct) => not_allowed(construct),
                None => syntax(format!("unexpected keyword '{word}'")),
            });
        }
        let node = self.expression()?;
        match self.peek() {
            Token::Assign => Err(not_allowed("assignment")),
            Token::AndAnd | Token::OrOr => Err(not_allowed("statement chaining with && or ||")),
            Token::Pipe => Err(not_allowed("a pipe to an external command")),
            _ => Ok(node),
        }
    }

    fn expression(&mut self) -> Result<Node, Error> {
        self.enter()?;
        let result = self.expression_inner();
        self.leave();
        result
    }

    fn expression_inner(&mut self) -> Result<Node, Error> {
        let left = self.unary()?;
        let cond = self.binary_rhs(left, 0)?;
        if self.eat(&Token::Question) {
            let then = self.expression()?;
            self.expect(&Token::Colon)?;
            let otherwise = self.expression()?;
            return Ok(Node::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            });
        }
        Ok(cond)
    }

    fn binary_rhs(&mut self, mut left: Node, min_prec: u8) -> Result<Node, Error> {
        loop {
            let (op, prec) = match self.peek() {
                Token::Or => (BinOp::Or, 1),
                Token::And => (BinOp::And, 2),
                Token::EqEq => (BinOp::Eq, 3),
                Token::NotEq => (BinOp::Ne, 3),
                Token::Gt => (BinOp::Gt, 3),
                Token::Lt => (BinOp::Lt, 3),
                Token::GtEq => (BinOp::Ge, 3),
                Token::LtEq => (BinOp::Le, 3),
                Token::StrEq => (BinOp::StrEq, 3),
                Token::StrNe => (BinOp::StrNe, 3),
                Token::Coalesce => (BinOp::NilCoalesce, 4),
                Token::DotDot => (BinOp::Concat, 5),
                Token::Plus => (BinOp::Add, 6),
                Token::Minus => (BinOp::Sub, 6),
                Token::Star => (BinOp::Mul, 7),
                Token::Slash => (BinOp::Div, 7),
                Token::Percent => (BinOp::Mod, 7),
                Token::Power => (BinOp::Pow, 8),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance();
            if op == BinOp::Concat {
                // `..` at the end of a line continues the expression.
                self.skip_newlines();
                if matches!(self.peek(), Token::Eof) {
                    return Err(syntax("expected expression after `..`"));
                }
            }
            let right = self.unary()?;
            self.enter()?;
            let right = self.binary_rhs(right, prec + 1);
            self.leave();
            left = Node::Binary { op, left: Box::new(left), right: Box::new(right?) };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Node, Error> {
        let op = match self.peek() {
            Token::Minus => UnaryOp::Neg,
            Token::Not | Token::Bang => UnaryOp::Not,
            _ => return self.primary(),
        };
        self.advance();
        self.enter()?;
        let operand = self.unary();
        self.leave();
        Ok(Node::Unary { op, operand: Box::new(operand?) })
    }

    fn primary(&mut self) -> Result<Node, Error> {
        let node = match self.advance() {
            Token::Number(n) => Node::Number(n),
            Token::Str(s) => {
                if matches!(self.peek(), Token::LParen) {
                    return Err(not_allowed(&format!("the function call {s}()")));
                }
                // A bare word is a string, as in Mix.
                Node::Str(s)
            }
            Token::Interp(parts) => Node::Interp(self.interp_parts(parts)?),
            Token::Var(name) => Node::Var(name),
            Token::True => Node::Bool(true),
            Token::False => Node::Bool(false),
            Token::Nil => Node::Nil,
            Token::LParen => {
                let inner = self.expression()?;
                self.expect(&Token::RParen)?;
                inner
            }
            Token::LBracket => self.list()?,
            Token::LBrace => self.map()?,
            Token::If => self.if_expression()?,
            Token::CommandSub => return Err(not_allowed("command substitution $()")),
            Token::Send => return Err(not_allowed("send")),
            Token::Sh => return Err(not_allowed("sh")),
            Token::Function => return Err(not_allowed("a function literal")),
            Token::Keyword(word) => {
                return Err(match statement_construct(word) {
                    Some(construct) => not_allowed(construct),
                    None => syntax(format!("unexpected keyword '{word}'")),
                });
            }
            Token::Semicolon => {
                return Err(syntax("unexpected ';' inside an expression"));
            }
            other => return Err(syntax(format!("unexpected {}", other.describe()))),
        };
        self.postfix(node)
    }

    fn postfix(&mut self, mut node: Node) -> Result<Node, Error> {
        loop {
            match self.peek() {
                Token::Dot => {
                    self.advance();
                    let field = self.field_name()?;
                    if matches!(self.peek(), Token::LParen) {
                        return Err(not_allowed(&format!("the method call .{field}()")));
                    }
                    node = Node::Field { object: Box::new(node), field };
                }
                Token::LBracket => {
                    self.advance();
                    let index = self.expression()?;
                    self.expect(&Token::RBracket)?;
                    node = Node::Index { object: Box::new(node), index: Box::new(index) };
                }
                Token::LParen => return Err(not_allowed("a call on a function value")),
                _ => return Ok(node),
            }
        }
    }

    /// A name after `.`: an identifier, a quoted string, or any keyword.
    fn field_name(&mut self) -> Result<String, Error> {
        let token = self.advance();
        match token {
            Token::Str(name) => Ok(name),
            Token::Function => Ok("function".into()),
            other => other
                .keyword_lexeme()
                .map(str::to_owned)
                .ok_or_else(|| syntax(format!("expected field name but found {}", other.describe()))),
        }
    }

    fn list(&mut self) -> Result<Node, Error> {
        let mut items = Vec::new();
        if !matches!(self.peek(), Token::RBracket) {
            items.push(self.expression()?);
            while self.eat(&Token::Comma) {
                if matches!(self.peek(), Token::RBracket) {
                    break;
                }
                items.push(self.expression()?);
            }
        }
        self.expect(&Token::RBracket)?;
        Ok(Node::List(items))
    }

    fn map(&mut self) -> Result<Node, Error> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        self.skip_newlines();
        if !matches!(self.peek(), Token::RBrace) {
            loop {
                self.skip_newlines();
                let key = match self.advance() {
                    Token::Str(s) => s,
                    other => other.keyword_lexeme().map(str::to_owned).ok_or_else(|| {
                        syntax(format!("expected map key but found {}", other.describe()))
                    })?,
                };
                self.expect(&Token::Colon)?;
                let value = self.expression()?;
                if entries.iter().any(|(k, _)| *k == key) {
                    return Err(syntax(format!(
                        "duplicate map key '{key}'; the second literal would overwrite the first"
                    )));
                }
                entries.push((key, value));
                self.skip_newlines();
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        self.skip_newlines();
        self.expect(&Token::RBrace)?;
        Ok(Node::Map(entries))
    }

    /// `if` already consumed. Bodies are statement blocks in Mix; here a
    /// body is at most one expression statement.
    fn if_expression(&mut self) -> Result<Node, Error> {
        let mut branches = Vec::new();
        let mut otherwise = None;
        let cond = self.expression()?;
        self.expect(&Token::Then)?;
        let body = self.block(&[Token::Else, Token::Elif, Token::End])?;
        branches.push(IfBranch { cond, body });
        loop {
            let chained = if self.eat(&Token::Elif) {
                true
            } else if self.eat(&Token::Else) {
                // `else if` is the two-word spelling of `elif`.
                self.eat(&Token::If)
            } else {
                break;
            };
            if chained {
                let cond = self.expression()?;
                self.expect(&Token::Then)?;
                let body = self.block(&[Token::Else, Token::Elif, Token::End])?;
                branches.push(IfBranch { cond, body });
            } else {
                otherwise = self.block(&[Token::End])?.map(Box::new);
                break;
            }
        }
        self.expect(&Token::End)?;
        Ok(Node::If { branches, otherwise })
    }

    /// A branch body: separators, at most one expression statement, then
    /// one of `stops`.
    fn block(&mut self, stops: &[Token]) -> Result<Option<Node>, Error> {
        self.skip_separators();
        if stops.contains(self.peek()) {
            return Ok(None);
        }
        let node = self.statement()?;
        self.skip_separators();
        if stops.contains(self.peek()) {
            return Ok(Some(node));
        }
        match self.peek() {
            Token::Eof => Err(syntax("expected `end` to close the if expression")),
            token @ (Token::RParen | Token::RBracket | Token::RBrace | Token::Colon | Token::Comma) => {
                Err(syntax(format!("unexpected {} in an if branch", token.describe())))
            }
            _ => Err(not_allowed("more than one statement in an if branch")),
        }
    }

    /// Interpret the pieces of a double-quoted string.
    fn interp_parts(&mut self, raw: Vec<RawPart>) -> Result<Vec<Part>, Error> {
        let mut parts = Vec::with_capacity(raw.len());
        for part in raw {
            parts.push(match part {
                RawPart::Literal(s) => Part::Literal(s),
                RawPart::EnvVar => {
                    return Err(not_allowed("environment-variable interpolation in a string"));
                }
                RawPart::Var(spec) => Part::Var(self.interp_var(&spec)?),
            });
        }
        Ok(parts)
    }

    /// `${path ?? default}`: the first `??` or `?:` splits the spec; the
    /// path is dotted segments, each with optional `[index]` suffixes.
    fn interp_var(&mut self, spec: &str) -> Result<InterpVar, Error> {
        let (path, coalesce) = split_coalesce(spec);
        let path = path.trim();
        if path.is_empty() {
            return Err(syntax("empty interpolation ${}"));
        }
        let mut segments = Vec::new();
        let mut head = None;
        for (i, segment) in split_segments(path).into_iter().enumerate() {
            let (base, suffixes) = split_suffixes(segment)?;
            if i == 0 {
                if base.is_empty() || !base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(syntax(format!("invalid interpolation variable '{base}'")));
                }
                head = Some(base.to_owned());
            } else {
                segments.push(Segment::Field(base.to_owned()));
            }
            for suffix in suffixes {
                match suffix {
                    Suffix::Index(body) => {
                        self.enter()?;
                        let index = parse_at_depth(body, self.depth);
                        self.leave();
                        segments.push(Segment::Index(index?));
                    }
                    Suffix::Call => {
                        return Err(not_allowed("a function call in an interpolation"));
                    }
                }
            }
        }
        let coalesce = match coalesce {
            None => None,
            Some((kind, payload)) => {
                let payload = payload.trim();
                if payload.is_empty() {
                    Some((kind, None))
                } else {
                    self.enter()?;
                    let node = parse_at_depth(payload, self.depth);
                    self.leave();
                    Some((kind, Some(Box::new(node?))))
                }
            }
        };
        Ok(InterpVar { head: head.unwrap_or_default(), segments, coalesce })
    }
}

fn split_coalesce(spec: &str) -> (&str, Option<(Coalesce, &str)>) {
    let bytes = spec.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b'?' {
            let kind = match bytes[i + 1] {
                b'?' => Coalesce::Nil,
                b':' => Coalesce::Falsy,
                _ => continue,
            };
            return (&spec[..i], Some((kind, &spec[i + 2..])));
        }
    }
    (spec, None)
}

/// Split a path on the dots outside `[...]` and `(...)`.
fn split_segments(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, ch) in path.char_indices() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            '.' if depth == 0 => {
                out.push(&path[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&path[start..]);
    out
}

enum Suffix<'a> {
    Index(&'a str),
    Call,
}

/// Split one segment into its base name and its trailing `[...]`/`(...)`.
fn split_suffixes(segment: &str) -> Result<(&str, Vec<Suffix<'_>>), Error> {
    let bytes = segment.as_bytes();
    let base_end = bytes
        .iter()
        .position(|b| matches!(b, b'[' | b'('))
        .unwrap_or(bytes.len());
    let mut suffixes = Vec::new();
    let mut i = base_end;
    while i < bytes.len() {
        let (open, close) = match bytes[i] {
            b'[' => (b'[', b']'),
            b'(' => (b'(', b')'),
            _ => return Err(syntax(format!("unexpected text in interpolation '{segment}'"))),
        };
        let mut depth = 0usize;
        let mut j = i;
        let mut closed = None;
        while j < bytes.len() {
            if bytes[j] == open {
                depth += 1;
            } else if bytes[j] == close {
                depth -= 1;
                if depth == 0 {
                    closed = Some(j);
                    break;
                }
            }
            j += 1;
        }
        let Some(end) = closed else {
            return Err(syntax(format!("unbalanced brackets in interpolation '{segment}'")));
        };
        suffixes.push(if open == b'[' {
            Suffix::Index(&segment[i + 1..end])
        } else {
            Suffix::Call
        });
        i = end + 1;
    }
    Ok((&segment[..base_end], suffixes))
}
