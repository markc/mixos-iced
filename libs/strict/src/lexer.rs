//! Tokens of the strict-data grammar.
//!
//! The lexer knows the full surface of a data file: `#` and `--` comments,
//! newlines (dropped inside `[ ]` and `{ }`), numbers, the three string
//! forms (`"…"`, `'…'`, `<<TAG` heredocs), bare words, and the punctuation
//! `[ ] { } : , -`. Anything executable that the surface can spell (`$name`,
//! `$(cmd)`, `${…}` inside a string, a leading `~` that would expand to the
//! home directory, `(`, `;`) still lexes to a token, so the parser can refuse
//! it with a message that names the construct.

use std::fmt;

use crate::error::{Error, Result};

/// Why an otherwise literal string is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// `${name}` interpolation.
    Variable,
    /// `$(command)` substitution inside a heredoc.
    Command,
    /// A leading `~` or `~/` in a double-quoted string, which the shell
    /// expands to the home directory.
    Home,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    Number(f64),
    /// A quoted string, decoded.
    Str(String),
    /// A bare identifier: `[A-Za-z_][A-Za-z0-9_]*`.
    Word(String),
    /// A string that is not plain text; always refused.
    Interpolated(Refused),
    /// `$name`.
    Variable(String),
    /// `$(…)`.
    CommandSub,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    LParen,
    Colon,
    Comma,
    Minus,
    Semicolon,
    Newline,
    /// Any other punctuation; never part of a valid document.
    Other(char),
    Eof,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Number(n) => write!(f, "{n}"),
            Token::Str(s) => write!(f, "{s:?}"),
            Token::Word(w) => f.write_str(w),
            Token::Interpolated(_) => f.write_str("interpolated string"),
            Token::Variable(name) => write!(f, "${name}"),
            Token::CommandSub => f.write_str("$(...)"),
            Token::LBracket => f.write_str("`[`"),
            Token::RBracket => f.write_str("`]`"),
            Token::LBrace => f.write_str("`{`"),
            Token::RBrace => f.write_str("`}`"),
            Token::LParen => f.write_str("`(`"),
            Token::Colon => f.write_str("`:`"),
            Token::Comma => f.write_str("`,`"),
            Token::Minus => f.write_str("`-`"),
            Token::Semicolon => f.write_str("`;`"),
            Token::Newline => f.write_str("newline"),
            Token::Other(c) => write!(f, "`{c}`"),
            Token::Eof => f.write_str("end of input"),
        }
    }
}

/// A token with the 1-based line and column it starts at.
#[derive(Debug, Clone)]
pub(crate) struct Spanned {
    pub token: Token,
    pub line: usize,
    pub column: usize,
}

/// Lex a whole document. The last token is always `Eof`.
pub(crate) fn tokenize(source: &str) -> Result<Vec<Spanned>> {
    let mut lexer = Lexer {
        chars: source.chars().collect(),
        pos: 0,
        line: 1,
        column: 1,
        groups: 0,
    };
    let mut tokens = Vec::new();
    loop {
        let spanned = lexer.next_token()?;
        let done = spanned.token == Token::Eof;
        tokens.push(spanned);
        if done {
            return Ok(tokens);
        }
    }
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
    /// Open `[`, `{` and `(` groups; newlines inside them are dropped.
    groups: usize,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn spanned(&self, token: Token, line: usize, column: usize) -> Spanned {
        Spanned {
            token,
            line,
            column,
        }
    }

    /// Spaces, tabs, carriage returns and a backslash that continues the
    /// line onto the next one.
    fn skip_blanks(&mut self) {
        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                '\\' if self.peek_at(1) == Some('\n') => {
                    self.advance();
                    self.advance();
                }
                '\\' if self.peek_at(1) == Some('\r') && self.peek_at(2) == Some('\n') => {
                    self.advance();
                    self.advance();
                    self.advance();
                }
                _ => break,
            }
        }
    }

    fn skip_to_line_end(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.advance();
        }
    }

    fn next_token(&mut self) -> Result<Spanned> {
        loop {
            self.skip_blanks();
            let line = self.line;
            let column = self.column;
            let Some(ch) = self.peek() else {
                return Ok(self.spanned(Token::Eof, line, column));
            };
            if ch == '#' || (ch == '-' && self.peek_at(1) == Some('-')) {
                self.skip_to_line_end();
                continue;
            }
            if ch == '\n' {
                while self.peek() == Some('\n') {
                    self.advance();
                }
                if self.groups > 0 {
                    continue;
                }
                return Ok(self.spanned(Token::Newline, line, column));
            }
            return self.lex_token(ch, line, column);
        }
    }

    fn lex_token(&mut self, ch: char, line: usize, column: usize) -> Result<Spanned> {
        if ch.is_ascii_digit() || (ch == '.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())) {
            return self.lex_number(line, column);
        }
        if ch == '"' {
            return self.lex_double_string(line, column);
        }
        if ch == '\'' {
            return self.lex_single_string(line, column);
        }
        if ch == '<' && self.peek_at(1) == Some('<') {
            return self.lex_heredoc(line, column);
        }
        if ch == '$' {
            return self.lex_dollar(line, column);
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let word = self.take_word();
            return Ok(self.spanned(Token::Word(word), line, column));
        }
        self.advance();
        let token = match ch {
            '[' => {
                self.groups += 1;
                Token::LBracket
            }
            ']' => {
                self.groups = self.groups.saturating_sub(1);
                Token::RBracket
            }
            '{' => {
                self.groups += 1;
                Token::LBrace
            }
            '}' => {
                self.groups = self.groups.saturating_sub(1);
                Token::RBrace
            }
            '(' => {
                self.groups += 1;
                Token::LParen
            }
            ')' => {
                self.groups = self.groups.saturating_sub(1);
                Token::Other(')')
            }
            ':' => Token::Colon,
            ',' => Token::Comma,
            '-' => Token::Minus,
            ';' => Token::Semicolon,
            c if c.is_ascii_punctuation() => Token::Other(c),
            c => {
                return Err(Error::lex(line, column, format!("unexpected character {c:?}")));
            }
        };
        Ok(self.spanned(token, line, column))
    }

    fn take_word(&mut self) -> String {
        let mut word = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                word.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        word
    }

    /// A decimal literal with optional fraction and exponent, or a `0x` /
    /// `0o` / `0b` integer. `_` separates digit groups. A leading zero on a
    /// multi-digit integer part is refused: `0755` would silently read as
    /// 755. Large integral values and `1e999` are accepted as the `f64` they
    /// parse to, so everything the encoder writes reads back.
    fn lex_number(&mut self, line: usize, column: usize) -> Result<Spanned> {
        if self.peek() == Some('0') {
            let radix = match self.peek_at(1) {
                Some('x' | 'X') => Some(16),
                Some('o' | 'O') => Some(8),
                Some('b' | 'B') => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                return self.lex_radix_number(line, column, radix);
            }
        }
        let mut text = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '.' {
                text.push(ch);
                self.advance();
            } else if ch == '_' {
                self.advance();
            } else {
                break;
            }
        }
        let int_part = text.split('.').next().unwrap_or("");
        if int_part.len() > 1 && int_part.starts_with('0') {
            return Err(Error::lex(
                line,
                column,
                format!(
                    "ambiguous leading-zero number '{text}': use a 0o (octal), 0x (hex) or \
                     0b (binary) prefix, or drop the leading zero for decimal"
                ),
            ));
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let sign = usize::from(matches!(self.peek_at(1), Some('+' | '-')));
            if self.peek_at(1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                text.push(self.advance().expect("exponent marker"));
                if sign == 1 {
                    text.push(self.advance().expect("exponent sign"));
                }
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() {
                        text.push(ch);
                        self.advance();
                    } else if ch == '_' {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }
        let n: f64 = text
            .parse()
            .map_err(|_| Error::lex(line, column, format!("invalid number '{text}'")))?;
        Ok(self.spanned(Token::Number(n), line, column))
    }

    fn lex_radix_number(&mut self, line: usize, column: usize, radix: u32) -> Result<Spanned> {
        let zero = self.advance().unwrap_or('0');
        let marker = self.advance().unwrap_or('?');
        let prefix = format!("{zero}{marker}");
        let radix_name = match radix {
            16 => "hex",
            8 => "octal",
            _ => "binary",
        };
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch == '_' {
                self.advance();
            } else if ch.is_digit(radix) {
                digits.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        if let Some(c) = self.peek()
            && c.is_alphanumeric()
        {
            return Err(Error::lex(
                line,
                column,
                format!("'{c}' is not a valid {radix_name} digit in a {prefix} literal"),
            ));
        }
        if digits.is_empty() {
            return Err(Error::lex(line, column, format!("{prefix} literal has no digits")));
        }
        let value = u64::from_str_radix(&digits, radix).map_err(|_| {
            Error::lex(
                line,
                column,
                format!("{prefix}{digits} is out of range for a 64-bit integer"),
            )
        })?;
        if value > (1u64 << 53) {
            return Err(Error::lex(
                line,
                column,
                format!("{prefix}{digits} exceeds the exact-integer range (2^53); numbers are f64"),
            ));
        }
        Ok(self.spanned(Token::Number(value as f64), line, column))
    }

    /// `'…'`: raw text. Only `\'` and `\\` are escapes; any other backslash
    /// is kept. The string may span lines.
    fn lex_single_string(&mut self, line: usize, column: usize) -> Result<Spanned> {
        self.advance();
        let mut text = String::new();
        loop {
            match self.advance() {
                None => return Err(Error::lex(line, column, "unterminated string")),
                Some('\'') => break,
                Some('\\') => match self.advance() {
                    Some('\'') => text.push('\''),
                    Some('\\') => text.push('\\'),
                    Some(c) => {
                        text.push('\\');
                        text.push(c);
                    }
                    None => return Err(Error::lex(line, column, "unterminated string")),
                },
                Some(c) => text.push(c),
            }
        }
        Ok(self.spanned(Token::Str(text), line, column))
    }

    /// `"…"` with escapes. `${…}` makes the string interpolated, and a
    /// leading `~` or `~/` would expand to the home directory; both are
    /// refused by the parser. A bare `$` elsewhere is literal. An escape
    /// the format does not define keeps its backslash.
    fn lex_double_string(&mut self, line: usize, column: usize) -> Result<Spanned> {
        self.advance();
        let mut text = String::new();
        let mut refused = None;
        if self.peek() == Some('~') && matches!(self.peek_at(1), Some('/') | Some('"')) {
            refused = Some(Refused::Home);
        }
        loop {
            match self.peek() {
                None => return Err(Error::lex(line, column, "unterminated string")),
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.advance() {
                        Some('n') => text.push('\n'),
                        Some('t') => text.push('\t'),
                        Some('r') => text.push('\r'),
                        Some('e') => text.push('\x1b'),
                        Some('"') => text.push('"'),
                        Some('\\') => text.push('\\'),
                        Some('$') => text.push('$'),
                        Some('~') => text.push('~'),
                        Some('0') => text.push('\0'),
                        Some('a') => text.push('\u{07}'),
                        Some('b') => text.push('\u{08}'),
                        Some('f') => text.push('\u{0C}'),
                        Some('v') => text.push('\u{0B}'),
                        Some('u') if self.peek() == Some('{') => {
                            text.push(self.lex_braced_unicode(line, column)?);
                        }
                        Some('u') if self.hex4_at(0).is_some() => {
                            text.push(self.lex_json_unicode(line, column)?);
                        }
                        Some('x') => {
                            let pair = (self.peek(), self.peek_at(1));
                            if let (Some(h1), Some(h2)) = pair
                                && h1.is_ascii_hexdigit()
                                && h2.is_ascii_hexdigit()
                            {
                                self.advance();
                                self.advance();
                                let code = h1.to_digit(16).expect("hex") * 16 + h2.to_digit(16).expect("hex");
                                text.push(char::from_u32(code).expect("0x00..=0xFF is a char"));
                            } else {
                                text.push_str("\\x");
                            }
                        }
                        Some(c) => {
                            text.push('\\');
                            text.push(c);
                        }
                        None => return Err(Error::lex(line, column, "unterminated string")),
                    }
                }
                Some('$') if self.peek_at(1) == Some('{') => {
                    refused.get_or_insert(Refused::Variable);
                    self.advance();
                    self.advance();
                    loop {
                        match self.advance() {
                            Some('}') => break,
                            Some(_) => {}
                            None => {
                                return Err(Error::lex(line, column, "unterminated interpolation"));
                            }
                        }
                    }
                }
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }
        let token = match refused {
            Some(why) => Token::Interpolated(why),
            None => Token::Str(text),
        };
        Ok(self.spanned(token, line, column))
    }

    /// `\u{…}` with one to six hex digits; the `\u` is consumed.
    fn lex_braced_unicode(&mut self, line: usize, column: usize) -> Result<char> {
        let err = |msg: String| Error::lex(line, column, msg);
        if self.advance() != Some('{') {
            return Err(err("\\u must be followed by '{': write \\u{FEFF}".into()));
        }
        let mut hex = String::new();
        loop {
            match self.advance() {
                Some('}') => break,
                Some(c) if c.is_ascii_hexdigit() => {
                    hex.push(c);
                    if hex.len() > 6 {
                        return Err(err("\\u{...} takes at most 6 hex digits".into()));
                    }
                }
                Some(c) => return Err(err(format!("invalid hex digit '{c}' in \\u{{...}}"))),
                None => return Err(err("unterminated \\u{...} escape".into())),
            }
        }
        if hex.is_empty() {
            return Err(err("empty \\u{} escape: write \\u{FEFF}".into()));
        }
        let code = u32::from_str_radix(&hex, 16).map_err(|_| err(format!("invalid \\u{{{hex}}} hex")))?;
        char::from_u32(code).ok_or_else(|| err(format!("\\u{{{hex}}} is not a valid unicode codepoint")))
    }

    /// The value of the four hex digits starting `offset` characters ahead.
    fn hex4_at(&self, offset: usize) -> Option<u32> {
        let mut value = 0u32;
        for k in 0..4 {
            value = value * 16 + self.peek_at(offset + k)?.to_digit(16)?;
        }
        Some(value)
    }

    /// JSON's `\uXXXX`; the `\u` is consumed and four hex digits follow. A
    /// surrogate pair joins into one character; a lone surrogate is an
    /// error.
    fn lex_json_unicode(&mut self, line: usize, column: usize) -> Result<char> {
        let err = |msg: String| Error::lex(line, column, msg);
        let high = self.hex4_at(0).expect("caller checked four hex digits");
        for _ in 0..4 {
            self.advance();
        }
        let code = match high {
            0xD800..=0xDBFF => {
                let low = if self.peek() == Some('\\') && self.peek_at(1) == Some('u') {
                    self.hex4_at(2)
                } else {
                    None
                };
                match low {
                    Some(low @ 0xDC00..=0xDFFF) => {
                        for _ in 0..6 {
                            self.advance();
                        }
                        0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                    }
                    _ => {
                        return Err(err(format!(
                            "\\u{high:04x} is a high surrogate with no \\uDC00-\\uDFFF low surrogate after it"
                        )));
                    }
                }
            }
            0xDC00..=0xDFFF => return Err(err(format!("\\u{high:04x} is a lone low surrogate"))),
            code => code,
        };
        Ok(char::from_u32(code).expect("a non-surrogate codepoint is a char"))
    }

    /// `<<TAG` then the body lines up to a line holding only `TAG`. The
    /// body keeps its newlines except the last; `\n \t \r \e \\ \$` decode;
    /// `${…}` and `$(…)` make it refused. The closing line's newline is left
    /// for the main loop.
    fn lex_heredoc(&mut self, line: usize, column: usize) -> Result<Spanned> {
        self.advance();
        self.advance();
        let tag = self.take_word();
        if tag.is_empty() {
            return Err(Error::lex(line, column, "expected heredoc tag after '<<'"));
        }
        let mut trailing = String::new();
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                self.advance();
                break;
            }
            trailing.push(ch);
            self.advance();
        }
        if !trailing.trim().is_empty() && !trailing.trim_start().starts_with("--") {
            return Err(Error::lex(
                line,
                column,
                format!(
                    "unexpected text after heredoc tag '<<{tag}': the opener owns its line; \
                     put '{trailing}' on the next line"
                ),
            ));
        }
        let mut body = String::new();
        loop {
            let mut content = String::new();
            while let Some(ch) = self.peek() {
                if ch == '\n' {
                    break;
                }
                content.push(ch);
                self.advance();
            }
            if content.trim() == tag {
                break;
            }
            body.push_str(&content);
            body.push('\n');
            if self.advance().is_none() {
                return Err(Error::lex(
                    line,
                    column,
                    format!("unterminated heredoc (expected '{tag}')"),
                ));
            }
        }
        if body.ends_with('\n') {
            body.pop();
        }

        let chars: Vec<char> = body.chars().collect();
        let mut text = String::new();
        let mut refused = None;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '$' if chars.get(i + 1) == Some(&'{') => {
                    refused.get_or_insert(Refused::Variable);
                    i += 2;
                    while i < chars.len() && chars[i] != '}' {
                        i += 1;
                    }
                    i += 1;
                }
                '$' if chars.get(i + 1) == Some(&'(') => {
                    refused.get_or_insert(Refused::Command);
                    i += 2;
                    let mut depth = 1;
                    while i < chars.len() {
                        match chars[i] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    i += 1;
                }
                '\\' if i + 1 < chars.len() => {
                    i += 1;
                    match chars[i] {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        'e' => text.push('\x1b'),
                        '\\' => text.push('\\'),
                        '$' => text.push('$'),
                        c => {
                            text.push('\\');
                            text.push(c);
                        }
                    }
                    i += 1;
                }
                c => {
                    text.push(c);
                    i += 1;
                }
            }
        }
        let token = match refused {
            Some(why) => Token::Interpolated(why),
            None => Token::Str(text),
        };
        Ok(self.spanned(token, line, column))
    }

    /// `$name` or `$(…)`; both are refused by the parser, but each gets its
    /// own message.
    fn lex_dollar(&mut self, line: usize, column: usize) -> Result<Spanned> {
        self.advance();
        if self.peek() == Some('(') {
            self.advance();
            let mut depth = 1;
            loop {
                match self.advance() {
                    Some('(') => depth += 1,
                    Some(')') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => return Err(Error::lex(line, column, "unterminated command substitution")),
                }
            }
            return Ok(self.spanned(Token::CommandSub, line, column));
        }
        let name = self.take_word();
        if name.is_empty() {
            return Err(Error::lex(line, column, "expected variable name after '$'"));
        }
        Ok(self.spanned(Token::Variable(name), line, column))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(source: &str) -> Vec<Token> {
        tokenize(source).expect("lexes").into_iter().map(|t| t.token).collect()
    }

    #[test]
    fn comments_are_dropped_and_newline_runs_collapse() {
        // A comment ends at its newline, which is still a token (the parser
        // skips newlines between entries); a run of newlines is one token.
        assert_eq!(
            tokens("# head\n\n-- dash\na: 1 # tail\n\n"),
            vec![
                Token::Newline,
                Token::Newline,
                Token::Word("a".into()),
                Token::Colon,
                Token::Number(1.0),
                Token::Newline,
                Token::Eof
            ]
        );
    }

    #[test]
    fn newlines_vanish_inside_groups() {
        assert_eq!(
            tokens("[\n1,\n2\n]"),
            vec![
                Token::LBracket,
                Token::Number(1.0),
                Token::Comma,
                Token::Number(2.0),
                Token::RBracket,
                Token::Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(tokens("1_000 .5 2.5e3 0x1F 0o755 0b101 1e999"), vec![
            Token::Number(1000.0),
            Token::Number(0.5),
            Token::Number(2500.0),
            Token::Number(31.0),
            Token::Number(493.0),
            Token::Number(5.0),
            Token::Number(f64::INFINITY),
            Token::Eof,
        ]);
        assert!(tokenize("0755").is_err());
        assert!(tokenize("0x").is_err());
        assert!(tokenize("0o78").is_err());
        assert!(tokenize("1.2.3").is_err());
    }

    #[test]
    fn string_forms() {
        assert_eq!(
            tokens(r#""a\tb\$c\~d\u{41}B\x43\q" 'raw\n\'x' "#),
            vec![
                Token::Str("a\tb$c~dABC\\q".into()),
                Token::Str("raw\\n'x".into()),
                Token::Eof
            ]
        );
        assert_eq!(tokens(r#""${x}""#), vec![Token::Interpolated(Refused::Variable), Token::Eof]);
        assert_eq!(tokens(r#""~/x""#), vec![Token::Interpolated(Refused::Home), Token::Eof]);
        assert_eq!(tokens(r#""~x" "a~/b" "$x""#), vec![
            Token::Str("~x".into()),
            Token::Str("a~/b".into()),
            Token::Str("$x".into()),
            Token::Eof
        ]);
        assert!(tokenize("\"open").is_err());
        assert!(tokenize("'open").is_err());
    }

    #[test]
    fn heredocs() {
        assert_eq!(
            tokens("<<E\nline\\n1\n  E\n"),
            vec![Token::Str("line\n1".into()), Token::Newline, Token::Eof]
        );
        assert_eq!(tokens("<<E\n$(ls)\nE"), vec![Token::Interpolated(Refused::Command), Token::Eof]);
        assert!(tokenize("<<E\nbody\n").is_err());
        assert!(tokenize("<<E extra\nbody\nE\n").is_err());
        assert!(tokenize("<<\nbody\n").is_err());
    }

    #[test]
    fn dollar_forms() {
        assert_eq!(tokens("$x $(ls -l)"), vec![
            Token::Variable("x".into()),
            Token::CommandSub,
            Token::Eof
        ]);
        assert!(tokenize("$").is_err());
        assert!(tokenize("$(ls").is_err());
    }

    #[test]
    fn line_continuation_and_crlf() {
        assert_eq!(tokens("a: \\\n 1\r\nb: 2\r\n"), vec![
            Token::Word("a".into()),
            Token::Colon,
            Token::Number(1.0),
            Token::Newline,
            Token::Word("b".into()),
            Token::Colon,
            Token::Number(2.0),
            Token::Newline,
            Token::Eof
        ]);
    }

    #[test]
    fn stray_characters() {
        assert_eq!(tokens("+ ."), vec![Token::Other('+'), Token::Other('.'), Token::Eof]);
        let err = tokenize("é").unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::Lex);
        assert_eq!((err.line(), err.column()), (Some(1), Some(1)));
    }
}
