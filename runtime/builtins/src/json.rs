//! `JSON.parse` and `JSON.stringify`.
//!
//! # Strict on the way in
//!
//! JSON is not a relaxed JavaScript literal, and the differences are the ones people trip on:
//! no trailing commas, no comments, no single quotes, no unquoted keys, no leading `+`, no
//! leading zeros, no hexadecimal, no `NaN` or `Infinity`. Accepting any of them would make
//! `JSON.parse` succeed on input that every other parser rejects, which turns a clear error at
//! the boundary into corrupt data further in.
//!
//! # Exact on the way out
//!
//! The escaping rules are the part that looks arbitrary and is not. `JSON.stringify` must
//! escape the control characters, and since ES2019 it must also escape **lone surrogates** as
//! `\uXXXX` so that its output is always well-formed UTF-16 — otherwise the result cannot be
//! transmitted, and round-tripping through a file silently replaces the character.
//!
//! Rust's `String` cannot hold a lone surrogate at all, so that case is unreachable from this
//! type and is noted rather than implemented; it becomes real when strings arrive from
//! JavaScript rather than from Rust.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A parsed JSON document.
///
/// `Object` keeps insertion order, because `JSON.stringify(JSON.parse(text))` should not
/// reorder a document — a `BTreeMap` would sort the keys and quietly rewrite every file it
/// round-tripped.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number. JSON has no `NaN` or infinities.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, in insertion order.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Looks a key up, for callers that do not want to scan.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// An index into an array.
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&Self> {
        match self {
            Self::Array(items) => items.get(index),
            _ => None,
        }
    }
}

/// Why a document could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong.
    pub message: String,
    /// Byte offset where it went wrong.
    pub at: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.at)
    }
}

impl std::error::Error for ParseError {}

/// `JSON.parse`.
///
/// # Errors
///
/// [`ParseError`] naming what was wrong and where.
pub fn parse(text: &str) -> Result<Json, ParseError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.at < parser.bytes.len() {
        return Err(parser.error("unexpected trailing characters"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> ParseError {
        ParseError {
            message: message.to_owned(),
            at: self.at,
        }
    }

    /// JSON's whitespace is exactly these four bytes — not Unicode whitespace, and not a
    /// comment.
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.peek() == Some(byte) {
            self.at += 1;
            return Ok(());
        }
        Err(self.error(&format!("expected {:?}", byte as char)))
    }

    fn literal(&mut self, word: &str) -> Result<(), ParseError> {
        if self.text[self.at..].starts_with(word) {
            self.at += word.len();
            return Ok(());
        }
        Err(self.error(&format!("expected {word}")))
    }

    fn value(&mut self) -> Result<Json, ParseError> {
        match self.peek() {
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn array(&mut self) -> Result<Json, ParseError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                // A trailing comma lands here as `]` after `,` — rejected, because JSON says
                // so even though every JavaScript literal allows it.
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self) -> Result<Json, ParseError> {
        self.expect(b'{')?;
        let mut entries: Vec<(String, Json)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("object keys must be quoted"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value()?;
            // A repeated key keeps the *last* value, as assignment would, and keeps the
            // first position.
            match entries.iter_mut().find(|(name, _)| *name == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key, value)),
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        // Leading zeros are rejected: `01` is not JSON, and accepting it would parse as 1 and
        // lose the fact that the document was malformed.
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit after '.'"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit in the exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        let text = &self.text[start..self.at];
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| self.error("not a number"))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let Some(escape) = self.peek() else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.error("unknown escape")),
                    }
                }
                // Raw control characters are not allowed in a JSON string: a literal newline
                // between the quotes is an error, not a newline.
                0x00..=0x1F => return Err(self.error("control character in string")),
                _ => {
                    let rest = &self.text[self.at..];
                    let character = rest.chars().next().ok_or_else(|| self.error("bad utf-8"))?;
                    self.at += character.len_utf8();
                    out.push(character);
                }
            }
        }
    }

    /// `\uXXXX`, joining a surrogate pair when one follows.
    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let high = self.hex4()?;
        if (0xD800..0xDC00).contains(&high) {
            // A high surrogate must be followed by its low half. Without joining them, the
            // two halves become replacement characters and the text is silently corrupted.
            if self.text[self.at..].starts_with("\\u") {
                let save = self.at;
                self.at += 2;
                let low = self.hex4()?;
                if (0xDC00..0xE000).contains(&low) {
                    let combined =
                        0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    return char::from_u32(combined)
                        .ok_or_else(|| self.error("bad surrogate pair"));
                }
                self.at = save;
            }
            return Err(self.error("lone high surrogate"));
        }
        if (0xDC00..0xE000).contains(&high) {
            return Err(self.error("lone low surrogate"));
        }
        char::from_u32(u32::from(high)).ok_or_else(|| self.error("bad escape"))
    }

    fn hex4(&mut self) -> Result<u16, ParseError> {
        let end = self.at + 4;
        if end > self.bytes.len() {
            return Err(self.error("truncated \\u escape"));
        }
        let digits = &self.text[self.at..end];
        let value = u16::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))?;
        self.at = end;
        Ok(value)
    }
}

/// `JSON.stringify`, with optional indentation.
///
/// `indent` of zero is the compact form.
#[must_use]
pub fn stringify(value: &Json, indent: usize) -> String {
    let mut out = String::new();
    write_value(&mut out, value, indent, 0);
    out
}

fn write_value(out: &mut String, value: &Json, indent: usize, depth: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Number(number) => out.push_str(&format_number(*number)),
        Json::String(text) => write_string(out, text),
        Json::Array(items) if items.is_empty() => out.push_str("[]"),
        Json::Array(items) => {
            out.push('[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        Json::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Json::Object(entries) => {
            out.push('{');
            for (at, (key, item)) in entries.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_string(out, key);
                out.push(':');
                if indent > 0 {
                    out.push(' ');
                }
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: usize, depth: usize) {
    if indent == 0 {
        return;
    }
    out.push('\n');
    for _ in 0..indent * depth {
        out.push(' ');
    }
}

/// How `JSON.stringify` writes a number.
///
/// **Not-a-number and the infinities become `null`**, because JSON has no way to write them —
/// the alternative would be emitting `NaN`, which no other parser accepts. And `-0` becomes
/// `0`, because JSON has no negative zero: a round trip therefore loses the sign, which is
/// the specified behaviour and a genuine information loss worth knowing about.
fn format_number(number: f64) -> String {
    if !number.is_finite() {
        return "null".to_owned();
    }
    if number == 0.0 {
        return "0".to_owned();
    }
    if number.fract() == 0.0 && number.abs() < 1e21 {
        let mut out = String::new();
        let _ = write!(out, "{number:.0}");
        return out;
    }
    let mut out = String::new();
    let _ = write!(out, "{number}");
    out
}

/// Escapes a string the way `JSON.stringify` does.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // Every other control character gets the long form. `/` is deliberately *not*
            // escaped: it is legal either way, and escaping it makes output differ from every
            // other implementation for no benefit.
            character if (character as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

/// So `Json::Object`'s ordering choice is not quietly undone by someone reaching for a map.
const _: fn() = || {
    fn assert_not_used<T>() {}
    assert_not_used::<BTreeMap<String, Json>>();
};
