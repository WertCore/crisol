//! `RegExp`, over [`regress`] for JavaScript-compatible matching.
//!
//! §M12 names `regress` specifically, and the reason is worth stating: Rust's `regex` crate
//! deliberately omits backreferences and lookaround to guarantee linear time. JavaScript has
//! both, and real code uses them, so a "close enough" engine would reject patterns that work in
//! every browser. The trade is that a pathological pattern can backtrack — which is a real
//! denial-of-service surface and belongs in the same conversation as any other untrusted input.
//!
//! # `lastIndex` is the whole difficulty
//!
//! `regress` matches. What it does not do — because it is not a JavaScript engine — is carry
//! the **mutable cursor** that `g` and `y` put on a `RegExp` object. That cursor is the single
//! most surprising thing about `RegExp`:
//!
//! ```js
//! const r = /a/g;
//! r.test("a");   // true   — lastIndex is now 1
//! r.test("a");   // false  — searching from index 1 finds nothing
//! r.test("a");   // true   — the failure reset lastIndex to 0
//! ```
//!
//! A regex literal with `g` reused across calls alternates. This is why
//! `if (/x/g.test(a) && /x/g.test(b))` behaves differently from the same regex hoisted into a
//! variable, and why linters warn about `g` on a shared literal.
//!
//! `test` is not a separate, simpler operation — **it is `exec` with the result thrown away**,
//! so it mutates exactly as much. Implementing `test` as a stateless search would make it
//! disagree with `exec` on the same object, which is worse than either behaviour alone.

use regress::{Flags as RegressFlags, Regex};

/// The flags a `RegExp` can carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// `g` — search from `lastIndex` and advance it.
    pub global: bool,
    /// `i` — case-insensitive.
    pub ignore_case: bool,
    /// `m` — `^` and `$` match at line breaks.
    pub multiline: bool,
    /// `s` — `.` matches line terminators.
    pub dot_all: bool,
    /// `u` — Unicode mode.
    pub unicode: bool,
    /// `y` — anchor the match **at** `lastIndex` rather than searching from it.
    pub sticky: bool,
    /// `d` — report capture group indices.
    pub has_indices: bool,
}

impl Flags {
    /// Parses a flag string, rejecting unknown or repeated letters.
    ///
    /// A repeat is a `SyntaxError` in the specification, not something to ignore: `/x/gg` is
    /// malformed, and accepting it would let a typo through silently.
    ///
    /// # Errors
    ///
    /// The offending character.
    pub fn parse(text: &str) -> Result<Self, char> {
        let mut flags = Self::default();
        for letter in text.chars() {
            let slot = match letter {
                'g' => &mut flags.global,
                'i' => &mut flags.ignore_case,
                'm' => &mut flags.multiline,
                's' => &mut flags.dot_all,
                'u' => &mut flags.unicode,
                'y' => &mut flags.sticky,
                'd' => &mut flags.has_indices,
                _ => return Err(letter),
            };
            if *slot {
                return Err(letter);
            }
            *slot = true;
        }
        Ok(flags)
    }

    /// The flag string, in the specification's order.
    ///
    /// `regexp.flags` is defined to report a fixed order regardless of how they were written,
    /// so `/x/yg.flags` is `"gy"`. Echoing the source order instead would make two equivalent
    /// regexes compare unequal as strings.
    #[must_use]
    pub fn to_text(self) -> String {
        let mut out = String::new();
        for (set, letter) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode, 'u'),
            (self.sticky, 'y'),
        ] {
            if set {
                out.push(letter);
            }
        }
        out
    }

    /// Whether `lastIndex` is consulted and updated at all.
    ///
    /// Only `g` and `y` do. Without either, `lastIndex` is inert — it can be assigned and it
    /// changes nothing, which is itself a source of confusion.
    #[must_use]
    pub const fn uses_last_index(self) -> bool {
        self.global || self.sticky
    }
}

/// One match: where it landed and what its groups captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    /// Byte offset the match starts at.
    pub start: usize,
    /// Byte offset one past its end.
    pub end: usize,
    /// One entry per capturing group. `None` is a group that did not participate — which is
    /// **not** the same as one that matched empty, and the difference is visible in `exec`'s
    /// result as `undefined` versus `""`.
    pub groups: Vec<Option<(usize, usize)>>,
}

/// A compiled `RegExp`, with its cursor.
#[derive(Debug)]
pub struct JsRegExp {
    regex: Regex,
    source: String,
    flags: Flags,
    /// `regexp.lastIndex` — mutable, and public in JavaScript, so a program may set it.
    last_index: usize,
}

impl JsRegExp {
    /// Compiles a pattern.
    ///
    /// # Errors
    ///
    /// The message from the underlying engine, which is a `SyntaxError` in JavaScript terms.
    pub fn new(pattern: &str, flags: Flags) -> Result<Self, String> {
        let regress_flags = RegressFlags {
            icase: flags.ignore_case,
            multiline: flags.multiline,
            dot_all: flags.dot_all,
            unicode: flags.unicode,
            ..RegressFlags::default()
        };
        let regex = Regex::with_flags(pattern, regress_flags).map_err(|error| error.to_string())?;
        Ok(Self {
            regex,
            source: pattern.to_owned(),
            flags,
            last_index: 0,
        })
    }

    /// `regexp.source`.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// `regexp.flags`.
    #[must_use]
    pub const fn flags(&self) -> Flags {
        self.flags
    }

    /// `regexp.lastIndex`.
    #[must_use]
    pub const fn last_index(&self) -> usize {
        self.last_index
    }

    /// Assigns `regexp.lastIndex`, which a program is allowed to do.
    pub const fn set_last_index(&mut self, index: usize) {
        self.last_index = index;
    }

    /// `regexp.exec(text)` — and the only search primitive here.
    ///
    /// Without `g` or `y`, `lastIndex` is ignored and left alone. With either, the search
    /// starts at `lastIndex`; **on success it advances to the end of the match, and on failure
    /// it resets to zero.** That reset is what makes a repeated `test` alternate rather than
    /// staying false forever.
    ///
    /// With `y` the match must begin exactly at `lastIndex`; a match found later in the string
    /// is not a match at all.
    pub fn exec(&mut self, text: &str) -> Option<Captured> {
        let start = if self.flags.uses_last_index() {
            self.last_index
        } else {
            0
        };
        if start > text.len() {
            // Past the end. Only meaningful when the cursor is live, but resetting
            // unconditionally would clobber a `lastIndex` the program set on a non-global regex,
            // where the specification says it is inert.
            if self.flags.uses_last_index() {
                self.last_index = 0;
            }
            return None;
        }

        let found = self.regex.find_from(text, start).next();
        let Some(found) = found else {
            if self.flags.uses_last_index() {
                self.last_index = 0;
            }
            return None;
        };

        if self.flags.sticky && found.range.start != start {
            // Sticky means anchored, not "search from here". A match further along is a miss.
            self.last_index = 0;
            return None;
        }

        if self.flags.uses_last_index() {
            self.last_index = found.range.end;
        }
        Some(Captured {
            start: found.range.start,
            end: found.range.end,
            groups: found
                .captures
                .iter()
                .map(|capture| capture.as_ref().map(|range| (range.start, range.end)))
                .collect(),
        })
    }

    /// `regexp.test(text)`.
    ///
    /// **`exec` with the result discarded**, so it mutates `lastIndex` exactly as much.
    /// Implementing it as a stateless search would make it disagree with `exec` on the same
    /// object, which is worse than either behaviour on its own.
    pub fn test(&mut self, text: &str) -> bool {
        self.exec(text).is_some()
    }

    /// Every match, as `String.prototype.matchAll` produces them.
    ///
    /// **An empty match advances the cursor by one**, or this never terminates: `/(?:)/g` has
    /// an empty match at every position, and without the bump the cursor stays put forever.
    /// The bump is one *character*, not one byte, so it cannot land inside a multi-byte
    /// sequence and produce a panic.
    pub fn all_matches(&mut self, text: &str) -> Vec<Captured> {
        let mut out = Vec::new();
        let mut at = 0;
        while at <= text.len() {
            let Some(found) = self.regex.find_from(text, at).next() else {
                break;
            };
            let empty = found.range.start == found.range.end;
            at = if empty {
                next_boundary(text, found.range.end)
            } else {
                found.range.end
            };
            out.push(Captured {
                start: found.range.start,
                end: found.range.end,
                groups: found
                    .captures
                    .iter()
                    .map(|capture| capture.as_ref().map(|range| (range.start, range.end)))
                    .collect(),
            });
            if empty && at > text.len() {
                break;
            }
        }
        if self.flags.uses_last_index() {
            self.last_index = 0;
        }
        out
    }
}

/// The next character boundary at or after `from`, so advancing never splits a code point.
fn next_boundary(text: &str, from: usize) -> usize {
    let mut at = from + 1;
    while at <= text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}
