//! `String`, stored as UTF-16 because that is what a JavaScript string *is*.
//!
//! # Why not Rust's `String`
//!
//! A JavaScript string is a sequence of 16-bit code units, and it is **not required to be
//! well-formed**: `"\uD800"` is a perfectly ordinary one-element string holding a lone
//! surrogate. Rust's `String` cannot hold that at all, so storing one there would mean either
//! rejecting legal input or silently replacing it with U+FFFD — corrupting data at the boundary
//! and losing the ability to round-trip anything that came in over a network.
//!
//! So this is a `Vec<u16>`. It costs a conversion at every Rust boundary and buys correctness on
//! the cases that actually appear: half of an emoji arriving in one chunk of a stream, a
//! filename from a Windows API, a `JSON.parse` of a document written by something careless.
//!
//! # Length counts code units, iteration yields code points
//!
//! This is the single most consequential split in the whole type:
//!
//! ```text
//! "😀".length          // 2  — two UTF-16 code units
//! [..."😀"].length     // 1  — one code point
//! "😀".split("")       // ["\uD83D", "\uDE00"] — two broken halves
//! ```
//!
//! Every index-taking method — `charAt`, `slice`, `indexOf`, `substring` — works in **code
//! units**, so slicing at an odd boundary splits an emoji in half and produces a lone
//! surrogate. That is specified behaviour, not a bug to route around: an implementation that
//! "helpfully" snapped indices to code-point boundaries would return different strings than
//! every engine, and `slice` would stop composing with `indexOf`.

/// A JavaScript string: a sequence of UTF-16 code units, well-formed or not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct JsString {
    units: Vec<u16>,
}

/// From Rust text, which is always well-formed.
///
/// A `From` rather than a `from_str`: the inherent name would shadow `FromStr::from_str`,
/// which is fallible, and this conversion cannot fail — every Rust `str` is valid UTF-16.
impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        Self {
            units: text.encode_utf16().collect(),
        }
    }
}

impl JsString {
    /// From raw code units, which may include lone surrogates.
    #[must_use]
    pub fn from_units(units: Vec<u16>) -> Self {
        Self { units }
    }

    /// The code units.
    #[must_use]
    pub fn units(&self) -> &[u16] {
        &self.units
    }

    /// `string.length` — **code units**, not characters.
    #[must_use]
    pub fn length(&self) -> usize {
        self.units.len()
    }

    /// Whether it is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Whether every surrogate is paired.
    ///
    /// `JSON.stringify` cares (it escapes lone surrogates since ES2019), and so does anything
    /// about to hand the text to a Rust API.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        char::decode_utf16(self.units.iter().copied()).all(|result| result.is_ok())
    }

    /// Rust text, or `None` when a lone surrogate makes that impossible.
    ///
    /// `None` rather than a lossy replacement: silently turning half an emoji into U+FFFD is
    /// how text gets corrupted somewhere far from where it went wrong.
    #[must_use]
    pub fn to_rust(&self) -> Option<String> {
        String::from_utf16(&self.units).ok()
    }

    /// Rust text with lone surrogates replaced, for diagnostics only.
    #[must_use]
    pub fn to_rust_lossy(&self) -> String {
        String::from_utf16_lossy(&self.units)
    }

    /// `string.charCodeAt(index)` — one code unit.
    #[must_use]
    pub fn code_unit_at(&self, index: usize) -> Option<u16> {
        self.units.get(index).copied()
    }

    /// `string.codePointAt(index)` — joins a surrogate pair when one starts here.
    ///
    /// The difference from [`JsString::code_unit_at`] is the whole reason both exist.
    #[must_use]
    pub fn code_point_at(&self, index: usize) -> Option<u32> {
        let high = u32::from(*self.units.get(index)?);
        if (0xD800..0xDC00).contains(&high)
            && let Some(&low) = self.units.get(index + 1)
            && (0xDC00..0xE000).contains(&u32::from(low))
        {
            return Some(0x1_0000 + ((high - 0xD800) << 10) + (u32::from(low) - 0xDC00));
        }
        Some(high)
    }

    /// Code points, as `[...string]` and `for…of` yield them.
    ///
    /// A lone surrogate comes through as itself rather than being dropped or replaced — the
    /// string is allowed to contain one, so iterating it must be able to report one.
    #[must_use]
    pub fn code_points(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.units.len() {
            let Some(point) = self.code_point_at(at) else {
                break;
            };
            at += if point > 0xFFFF { 2 } else { 1 };
            out.push(point);
        }
        out
    }

    /// `string.slice(start, end)` in code units.
    ///
    /// Negative indices count from the end; a start past the end gives `""`. **Unlike
    /// `substring`, the arguments are not swapped** — `slice(4, 1)` is `""`.
    #[must_use]
    pub fn slice(&self, start: isize, end: isize) -> Self {
        let length = self.units.len();
        let from = relative(start, length);
        let to = relative(end, length);
        if from >= to {
            return Self::default();
        }
        Self {
            units: self.units[from..to].to_vec(),
        }
    }

    /// `string.substring(start, end)`.
    ///
    /// **Swaps its arguments when they are the wrong way round**, and clamps negatives to zero
    /// rather than counting from the end. Two differences from `slice` that make the pair a
    /// reliable source of bugs when one is substituted for the other.
    #[must_use]
    pub fn substring(&self, start: isize, end: isize) -> Self {
        let length = self.units.len();
        let clamp = |value: isize| value.clamp(0, length as isize) as usize;
        let (from, to) = {
            let a = clamp(start);
            let b = clamp(end);
            if a > b { (b, a) } else { (a, b) }
        };
        Self {
            units: self.units[from..to].to_vec(),
        }
    }

    /// `string.at(index)` — one code unit, negative indices from the end.
    #[must_use]
    pub fn at(&self, index: isize) -> Option<u16> {
        let length = self.units.len();
        let resolved = if index < 0 {
            length.checked_sub(index.unsigned_abs())?
        } else {
            index as usize
        };
        self.units.get(resolved).copied()
    }

    /// `string.indexOf(needle)` in code units, or `None` for not found.
    ///
    /// JavaScript reports `-1`; `None` is reported here so a caller cannot use the sentinel as
    /// an index by accident.
    #[must_use]
    pub fn index_of(&self, needle: &Self) -> Option<usize> {
        if needle.units.is_empty() {
            return Some(0);
        }
        self.units
            .windows(needle.units.len())
            .position(|window| window == needle.units.as_slice())
    }

    /// Whether it contains `needle`.
    #[must_use]
    pub fn includes(&self, needle: &Self) -> bool {
        self.index_of(needle).is_some()
    }

    /// `string.startsWith`.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.units.starts_with(&prefix.units)
    }

    /// `string.endsWith`.
    #[must_use]
    pub fn ends_with(&self, suffix: &Self) -> bool {
        self.units.ends_with(&suffix.units)
    }

    /// `string.split("")` — one element per **code unit**, so a surrogate pair is torn in half.
    ///
    /// Specified, and the reason `[...string]` exists as a separate way to take a string apart.
    #[must_use]
    pub fn split_units(&self) -> Vec<Self> {
        self.units
            .iter()
            .map(|unit| Self { units: vec![*unit] })
            .collect()
    }

    /// `string.concat`.
    #[must_use]
    pub fn concat(&self, other: &Self) -> Self {
        let mut units = self.units.clone();
        units.extend_from_slice(&other.units);
        Self { units }
    }

    /// `string.repeat(count)`.
    ///
    /// `None` for a negative count, which is a `RangeError` — not an empty string. Returning
    /// `""` would turn a caller's arithmetic mistake into silently missing output.
    #[must_use]
    pub fn repeat(&self, count: isize) -> Option<Self> {
        if count < 0 {
            return None;
        }
        Some(Self {
            units: self.units.repeat(count as usize),
        })
    }

    /// `string.padStart(target, pad)`.
    ///
    /// The padding is **truncated**, not dropped, when it does not divide evenly — and a target
    /// shorter than the string leaves it alone rather than trimming it.
    #[must_use]
    pub fn pad_start(&self, target: usize, pad: &Self) -> Self {
        if self.units.len() >= target || pad.units.is_empty() {
            return self.clone();
        }
        let needed = target - self.units.len();
        let mut filler: Vec<u16> = Vec::with_capacity(needed);
        while filler.len() < needed {
            filler.extend_from_slice(&pad.units);
        }
        filler.truncate(needed);
        filler.extend_from_slice(&self.units);
        Self { units: filler }
    }

    /// `string.trim`, using the specification's whitespace set.
    ///
    /// That set is `WhiteSpace` plus `LineTerminator`: Unicode space separators, tab, vertical
    /// tab, form feed, no-break space, **and the byte-order mark**. The BOM is the one people
    /// miss — Unicode does not classify it as whitespace and the spec trims it anyway, so a
    /// file that begins with one leaves an invisible character on the front of the first field.
    #[must_use]
    pub fn trim(&self) -> Self {
        let is_space = |unit: u16| {
            matches!(
                unit,
                0x0009
                    | 0x000A
                    | 0x000B
                    | 0x000C
                    | 0x000D
                    | 0x0020
                    | 0x00A0
                    | 0x1680
                    | 0x2028
                    | 0x2029
                    | 0x202F
                    | 0x205F
                    | 0x3000
                    | 0xFEFF
            ) || (0x2000..=0x200A).contains(&unit)
        };
        let start = self
            .units
            .iter()
            .position(|unit| !is_space(*unit))
            .unwrap_or(self.units.len());
        let end = self
            .units
            .iter()
            .rposition(|unit| !is_space(*unit))
            .map_or(start, |at| at + 1);
        Self {
            units: self.units[start..end].to_vec(),
        }
    }
}

/// Resolves a possibly-negative index against a length, the way `slice` does.
fn relative(index: isize, length: usize) -> usize {
    if index < 0 {
        length.saturating_sub(index.unsigned_abs())
    } else {
        (index as usize).min(length)
    }
}
