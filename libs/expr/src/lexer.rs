//! Tokens of the binding language.
//!
//! The token set is Mix's expression surface: the same number spellings,
//! the same two string forms with the same escapes and `${...}`
//! interpolation, the same operators and keywords. Statement keywords are
//! lexed too, so the parser can name the construct it refuses.

use crate::{Error, ErrorKind};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    Number(f64),
    /// A quoted string without interpolation, or a bare identifier (Mix
    /// treats both as one token kind).
    Str(String),
    /// A double-quoted string with at least one non-literal part.
    Interp(Vec<RawPart>),
    Var(String),
    /// `$(...)`: refused by the parser.
    CommandSub,
    /// A reserved word with no meaning inside an expression (`for`,
    /// `print`, ...). The parser refuses it by name.
    Keyword(&'static str),
    If,
    Then,
    Else,
    Elif,
    End,
    And,
    Or,
    Not,
    True,
    False,
    Nil,
    StrEq,
    StrNe,
    Send,
    Sh,
    Function,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Power,
    EqEq,
    NotEq,
    Gt,
    Lt,
    GtEq,
    LtEq,
    DotDot,
    Coalesce,
    Question,
    Pipe,
    AndAnd,
    OrOr,
    Assign,
    Bang,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Tilde,
    Semicolon,
    Newline,
    Eof,
}

/// A piece of a double-quoted string as the lexer sees it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RawPart {
    Literal(String),
    /// The text between `${` and `}`, uninterpreted.
    Var(String),
    /// A leading `~`: an environment read, refused by the parser.
    EnvVar,
}

impl Token {
    /// The source spelling of a keyword-like token, for use as a field name
    /// or map key (Mix allows every reserved word there).
    pub(crate) fn keyword_lexeme(&self) -> Option<&'static str> {
        Some(match self {
            Token::Keyword(name) => name,
            Token::If => "if",
            Token::Then => "then",
            Token::Else => "else",
            Token::Elif => "elif",
            Token::End => "end",
            Token::And => "and",
            Token::Or => "or",
            Token::Not => "not",
            Token::True => "true",
            Token::False => "false",
            Token::Nil => "nil",
            Token::StrEq => "eq",
            Token::StrNe => "ne",
            Token::Send => "send",
            Token::Sh => "sh",
            _ => return None,
        })
    }

    /// Short text for error messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            Token::Number(n) => format!("number {n}"),
            Token::Str(s) => format!("'{s}'"),
            Token::Interp(_) => "string".into(),
            Token::Var(name) => format!("${name}"),
            Token::CommandSub => "$(...)".into(),
            Token::Function => "function".into(),
            Token::Plus => "+".into(),
            Token::Minus => "-".into(),
            Token::Star => "*".into(),
            Token::Slash => "/".into(),
            Token::Percent => "%".into(),
            Token::Power => "**".into(),
            Token::EqEq => "==".into(),
            Token::NotEq => "!=".into(),
            Token::Gt => ">".into(),
            Token::Lt => "<".into(),
            Token::GtEq => ">=".into(),
            Token::LtEq => "<=".into(),
            Token::DotDot => "..".into(),
            Token::Coalesce => "??".into(),
            Token::Question => "?".into(),
            Token::Pipe => "|".into(),
            Token::AndAnd => "&&".into(),
            Token::OrOr => "||".into(),
            Token::Assign => "=".into(),
            Token::Bang => "!".into(),
            Token::LParen => "(".into(),
            Token::RParen => ")".into(),
            Token::LBracket => "[".into(),
            Token::RBracket => "]".into(),
            Token::LBrace => "{".into(),
            Token::RBrace => "}".into(),
            Token::Colon => ":".into(),
            Token::Comma => ",".into(),
            Token::Dot => ".".into(),
            Token::Tilde => "~".into(),
            Token::Semicolon => ";".into(),
            Token::Newline => "newline".into(),
            Token::Eof => "end of input".into(),
            other => other.keyword_lexeme().unwrap_or("token").into(),
        }
    }
}

/// Reserved words that are not expression tokens. Each is refused by the
/// parser under the name Mix gives the construct.
const STATEMENT_KEYWORDS: &[&str] = &[
    "for", "each", "in", "to", "step", "next", "while", "done", "loop", "break", "continue",
    "return", "select", "when", "otherwise", "parse", "with", "address", "emit", "on", "try",
    "catch", "finally", "die", "export", "alias", "print", "eprint", "source", "include",
    "label", "do",
];

fn keyword(name: &str) -> Option<Token> {
    Some(match name {
        "if" => Token::If,
        "then" => Token::Then,
        "else" => Token::Else,
        "elif" => Token::Elif,
        "end" => Token::End,
        "and" => Token::And,
        "or" => Token::Or,
        "not" => Token::Not,
        "true" => Token::True,
        "false" => Token::False,
        "nil" => Token::Nil,
        "eq" => Token::StrEq,
        "ne" => Token::StrNe,
        "send" => Token::Send,
        "sh" => Token::Sh,
        "function" | "fn" => Token::Function,
        _ => {
            let word = STATEMENT_KEYWORDS.iter().find(|w| **w == name)?;
            Token::Keyword(word)
        }
    })
}

pub(crate) struct Lexer {
    chars: Vec<char>,
    pos: usize,
    /// Newlines inside `(`, `[` or `{` are whitespace, as in Mix.
    group_depth: usize,
}

fn syntax(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Syntax, message)
}

impl Lexer {
    pub(crate) fn new(source: &str) -> Self {
        Lexer { chars: source.chars().collect(), pos: 0, group_depth: 0 }
    }

    pub(crate) fn tokenize(mut self) -> Result<Vec<Token>, Error> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            let done = token == Token::Eof;
            tokens.push(token);
            if done {
                return Ok(tokens);
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        Some(ch)
    }

    fn next_token(&mut self) -> Result<Token, Error> {
        loop {
            while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
                self.pos += 1;
            }
            let Some(ch) = self.peek() else {
                return Ok(Token::Eof);
            };
            // Comments run to the end of the line.
            if ch == '#' || (ch == '-' && self.peek_at(1) == Some('-')) {
                while !matches!(self.peek(), None | Some('\n')) {
                    self.pos += 1;
                }
                continue;
            }
            if ch == '\n' {
                while self.peek() == Some('\n') {
                    self.pos += 1;
                }
                if self.group_depth > 0 {
                    continue;
                }
                return Ok(Token::Newline);
            }
            return self.token_at(ch);
        }
    }

    fn token_at(&mut self, ch: char) -> Result<Token, Error> {
        if ch.is_ascii_digit() || (ch == '.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())) {
            return self.number();
        }
        if ch == '"' {
            return self.double_string();
        }
        if ch == '\'' {
            return self.single_string();
        }
        if ch == '$' {
            return self.variable();
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let name = self.word();
            return Ok(keyword(&name).unwrap_or(Token::Str(name)));
        }
        self.pos += 1;
        let two = |this: &mut Self, next: char, yes: Token, no: Token| {
            if this.peek() == Some(next) {
                this.pos += 1;
                yes
            } else {
                no
            }
        };
        Ok(match ch {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => two(self, '*', Token::Power, Token::Star),
            '/' => Token::Slash,
            '%' => Token::Percent,
            '=' => two(self, '=', Token::EqEq, Token::Assign),
            '!' => two(self, '=', Token::NotEq, Token::Bang),
            '>' => two(self, '=', Token::GtEq, Token::Gt),
            '<' => {
                if self.peek() == Some('<') {
                    return Err(syntax("heredoc strings are not supported in an expression"));
                }
                two(self, '=', Token::LtEq, Token::Lt)
            }
            '.' => two(self, '.', Token::DotDot, Token::Dot),
            '?' => two(self, '?', Token::Coalesce, Token::Question),
            '|' => two(self, '|', Token::OrOr, Token::Pipe),
            '&' => {
                if self.peek() == Some('&') {
                    self.pos += 1;
                    Token::AndAnd
                } else {
                    return Err(syntax("unexpected '&', did you mean '&&'?"));
                }
            }
            '(' => {
                self.group_depth += 1;
                Token::LParen
            }
            ')' => {
                self.group_depth = self.group_depth.saturating_sub(1);
                Token::RParen
            }
            '[' => {
                self.group_depth += 1;
                Token::LBracket
            }
            ']' => {
                self.group_depth = self.group_depth.saturating_sub(1);
                Token::RBracket
            }
            '{' => {
                self.group_depth += 1;
                Token::LBrace
            }
            '}' => {
                self.group_depth = self.group_depth.saturating_sub(1);
                Token::RBrace
            }
            ':' => Token::Colon,
            ',' => Token::Comma,
            ';' => Token::Semicolon,
            '~' => Token::Tilde,
            other => return Err(syntax(format!("unexpected character '{other}'"))),
        })
    }

    fn word(&mut self) -> String {
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                name.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        name
    }

    fn variable(&mut self) -> Result<Token, Error> {
        self.pos += 1; // $
        if self.peek() == Some('(') {
            // Skip the balanced body so the parser can refuse the whole form.
            self.pos += 1;
            let mut depth = 1usize;
            loop {
                match self.advance() {
                    Some('(') => depth += 1,
                    Some(')') => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(Token::CommandSub);
                        }
                    }
                    Some(_) => {}
                    None => return Err(syntax("unterminated command substitution")),
                }
            }
        }
        let name = self.word();
        if name.is_empty() {
            return Err(syntax("expected variable name after '$'"));
        }
        Ok(Token::Var(name))
    }

    /// A number: decimal digits with `_` separators, an optional fraction
    /// and exponent, or a `0x`/`0o`/`0b` radix integer. A leading zero on a
    /// multi-digit integer part is refused, as is any literal that cannot
    /// be represented exactly as an f64 integer or that overflows.
    fn number(&mut self) -> Result<Token, Error> {
        if self.peek() == Some('0') {
            let radix = match self.peek_at(1) {
                Some('x' | 'X') => Some(16),
                Some('o' | 'O') => Some(8),
                Some('b' | 'B') => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                return self.radix_number(radix);
            }
        }
        let mut text = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '.' {
                text.push(ch);
            } else if ch != '_' {
                break;
            }
            self.pos += 1;
        }
        let int_part = text.split('.').next().unwrap_or("");
        if int_part.len() > 1 && int_part.starts_with('0') {
            return Err(syntax(format!(
                "ambiguous leading-zero number '{text}'; use a 0o (octal), 0x (hex) or 0b (binary) prefix, or drop the leading zero"
            )));
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let sign = usize::from(matches!(self.peek_at(1), Some('+' | '-')));
            if self.peek_at(1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                text.push(self.advance().unwrap_or('e'));
                if sign == 1 {
                    text.push(self.advance().unwrap_or('+'));
                }
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() {
                        text.push(ch);
                    } else if ch != '_' {
                        break;
                    }
                    self.pos += 1;
                }
            }
        }
        let value: f64 = text
            .parse()
            .map_err(|_| syntax(format!("invalid number '{text}'")))?;
        if !value.is_finite() {
            return Err(syntax(format!("number '{text}' is out of range (infinity)")));
        }
        if !text.contains(['.', 'e', 'E']) {
            let exact = text.parse::<u128>().ok().filter(|v| (*v as f64) as u128 == *v);
            if exact.is_none() {
                return Err(syntax(format!(
                    "integer literal '{text}' exceeds the exact range (2^53); a number would silently round; use a string for ids and digests"
                )));
            }
        }
        Ok(Token::Number(value))
    }

    fn radix_number(&mut self, radix: u32) -> Result<Token, Error> {
        let prefix: String = [self.advance().unwrap_or('0'), self.advance().unwrap_or('?')]
            .into_iter()
            .collect();
        let name = match radix {
            16 => "hex",
            8 => "octal",
            _ => "binary",
        };
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch == '_' {
                self.pos += 1;
            } else if ch.is_digit(radix) {
                digits.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        if let Some(c) = self.peek()
            && c.is_alphanumeric()
        {
            return Err(syntax(format!("'{c}' is not a valid {name} digit in a {prefix} literal")));
        }
        if digits.is_empty() {
            return Err(syntax(format!("{prefix} literal has no digits")));
        }
        let value = u64::from_str_radix(&digits, radix)
            .map_err(|_| syntax(format!("{prefix}{digits} is out of range for a 64-bit integer")))?;
        if value > (1u64 << 53) {
            return Err(syntax(format!(
                "{prefix}{digits} exceeds the exact range (2^53); a number would silently round"
            )));
        }
        Ok(Token::Number(value as f64))
    }

    /// `'...'`: only `\'` and `\\` are escapes; every other backslash is
    /// kept literally.
    fn single_string(&mut self) -> Result<Token, Error> {
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.advance() {
                None => return Err(syntax("unterminated string")),
                Some('\'') => return Ok(Token::Str(text)),
                Some('\\') => match self.advance() {
                    Some('\'') => text.push('\''),
                    Some('\\') => text.push('\\'),
                    Some(c) => {
                        text.push('\\');
                        text.push(c);
                    }
                    None => return Err(syntax("unterminated string")),
                },
                Some(c) => text.push(c),
            }
        }
    }

    /// `"..."`: escapes, `${...}` interpolation, a leading `~` that means
    /// the home directory (refused later), and a literal bare `$`.
    fn double_string(&mut self) -> Result<Token, Error> {
        self.pos += 1;
        let mut parts: Vec<RawPart> = Vec::new();
        let mut current = String::new();
        if self.peek() == Some('~') && matches!(self.peek_at(1), Some('/') | Some('"')) {
            self.pos += 1;
            parts.push(RawPart::EnvVar);
        }
        loop {
            match self.advance() {
                None => return Err(syntax("unterminated string")),
                Some('"') => break,
                Some('\\') => match self.advance() {
                    None => return Err(syntax("unterminated string")),
                    Some('n') => current.push('\n'),
                    Some('t') => current.push('\t'),
                    Some('r') => current.push('\r'),
                    Some('e') => current.push('\x1b'),
                    Some('"') => current.push('"'),
                    Some('\\') => current.push('\\'),
                    Some('$') => current.push('$'),
                    Some('~') => current.push('~'),
                    Some('0') => current.push('\0'),
                    Some('a') => current.push('\u{07}'),
                    Some('b') => current.push('\u{08}'),
                    Some('f') => current.push('\u{0C}'),
                    Some('v') => current.push('\u{0B}'),
                    Some('u') if self.peek() == Some('{') => {
                        current.push(self.unicode_escape()?);
                    }
                    Some('x') => {
                        let pair = (self.peek(), self.peek_at(1));
                        if let (Some(h1), Some(h2)) = pair
                            && let (Some(d1), Some(d2)) = (h1.to_digit(16), h2.to_digit(16))
                        {
                            self.pos += 2;
                            current.push(char::from_u32(d1 * 16 + d2).unwrap_or('\u{FFFD}'));
                        } else {
                            current.push_str("\\x");
                        }
                    }
                    // Any other escape keeps its backslash, as in Mix.
                    Some(c) => {
                        current.push('\\');
                        current.push(c);
                    }
                },
                Some('$') if self.peek() == Some('{') => {
                    self.pos += 1;
                    if !current.is_empty() {
                        parts.push(RawPart::Literal(std::mem::take(&mut current)));
                    }
                    let mut spec = String::new();
                    loop {
                        match self.advance() {
                            Some('}') => break,
                            Some(c) => spec.push(c),
                            None => return Err(syntax("unterminated interpolation")),
                        }
                    }
                    parts.push(RawPart::Var(spec));
                }
                Some(c) => current.push(c),
            }
        }
        if !current.is_empty() {
            parts.push(RawPart::Literal(current));
        }
        match parts.as_slice() {
            [] => Ok(Token::Str(String::new())),
            [RawPart::Literal(s)] => Ok(Token::Str(s.clone())),
            _ => Ok(Token::Interp(parts)),
        }
    }

    /// `\u{XXXX}` with the `\u` already consumed: one to six hex digits.
    fn unicode_escape(&mut self) -> Result<char, Error> {
        self.pos += 1; // {
        let mut hex = String::new();
        loop {
            match self.advance() {
                Some('}') => break,
                Some(c) if c.is_ascii_hexdigit() && hex.len() < 6 => hex.push(c),
                Some(c) => {
                    return Err(syntax(format!("invalid character '{c}' in \\u{{...}} escape")));
                }
                None => return Err(syntax("unterminated \\u{...} escape")),
            }
        }
        if hex.is_empty() {
            return Err(syntax("empty \\u{...} escape"));
        }
        let code = u32::from_str_radix(&hex, 16).map_err(|_| syntax("invalid \\u{...} escape"))?;
        char::from_u32(code)
            .ok_or_else(|| syntax(format!("\\u{{{hex}}} is not a valid unicode codepoint")))
    }
}
