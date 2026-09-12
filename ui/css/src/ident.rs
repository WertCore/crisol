//! Identifiers that appear in selectors.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A string that appears in a selector: a tag name, a class, an id, an attribute name or
/// an attribute value.
///
/// Refcounted rather than owned because the same handful of strings — `div`, `span`,
/// `class` — appear in nearly every selector in a stylesheet, and a `String` per occurrence
/// is exactly the kind of per-node allocation ROADMAP §M3 says contradicts the product
/// thesis.
///
/// Carries a hash computed once at construction. The selector matcher's bloom filter asks
/// for a hash on every candidate ancestor, and recomputing it from the bytes each time is
/// measurable on a document-sized tree.
#[derive(Clone, Default)]
pub struct CssIdent {
    text: Arc<str>,
    /// Cached for [`selectors::bloom`]'s benefit. Deliberately *not* what [`Hash`] uses —
    /// see the `Hash` impl.
    precomputed: u32,
}

impl CssIdent {
    /// Interns `text` as written.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self {
            text: Arc::from(text),
            precomputed: fnv1a(text.as_bytes()),
        }
    }

    /// Interns `text` lowercased, for names that are ASCII case-insensitive.
    ///
    /// HTML tag and attribute names are case-insensitive, and `html5ever` already hands us
    /// lowercase ones at M4. Normalising here too means a hand-built tree and a parsed one
    /// match the same selectors, which is otherwise a very confusing difference.
    #[must_use]
    pub fn lowercase(text: &str) -> Self {
        if text.bytes().any(|b| b.is_ascii_uppercase()) {
            Self::new(&text.to_ascii_lowercase())
        } else {
            Self::new(text)
        }
    }

    /// The string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// True when this is the empty identifier, which is what [`Default`] produces and what
    /// "no namespace" is represented by.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl PartialEq for CssIdent {
    fn eq(&self, other: &Self) -> bool {
        // The hash is a cheap reject; the string is the answer.
        self.precomputed == other.precomputed && self.text == other.text
    }
}

impl Eq for CssIdent {}

impl Hash for CssIdent {
    /// Hashes the string, *not* the precomputed value.
    ///
    /// `Borrow<str>` promises that a borrowed and an owned key hash the same, and
    /// `HashMap<CssIdent, _>::get(&str)` relies on it. `precomputed_hash` is a separate
    /// contract, used only by the selector bloom filter, and is free to differ.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl AsRef<str> for CssIdent {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl Borrow<str> for CssIdent {
    fn borrow(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for CssIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

impl fmt::Display for CssIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<&str> for CssIdent {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl precomputed_hash::PrecomputedHash for CssIdent {
    fn precomputed_hash(&self) -> u32 {
        self.precomputed
    }
}

impl cssparser::ToCss for CssIdent {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_identifier(&self.text, dest)
    }
}

/// FNV-1a, 32-bit.
///
/// Chosen over the default hasher because it is a `const`-shaped loop with no setup cost,
/// and these strings are short — a tag name is three or four bytes, where a stronger hash
/// spends more time on the ceremony than on the data.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use precomputed_hash::PrecomputedHash;

    #[test]
    fn equality_ignores_the_allocation() {
        assert_eq!(CssIdent::new("div"), CssIdent::new("div"));
        assert_ne!(CssIdent::new("div"), CssIdent::new("span"));
    }

    #[test]
    fn lowercase_normalises_ascii_only() {
        assert_eq!(CssIdent::lowercase("DIV").as_str(), "div");
        // Non-ASCII is left alone: CSS case-insensitivity is ASCII-only.
        assert_eq!(CssIdent::lowercase("Ä").as_str(), "Ä");
    }

    #[test]
    fn hash_matches_the_borrowed_string() {
        use std::collections::HashMap;
        let mut map: HashMap<CssIdent, u32> = HashMap::new();
        map.insert(CssIdent::new("div"), 1);
        assert_eq!(
            map.get("div"),
            Some(&1),
            "Hash must agree with Borrow<str> or HashMap lookups by &str silently miss"
        );
    }

    #[test]
    fn precomputed_hash_is_stable_and_not_the_std_hash() {
        let a = CssIdent::new("div");
        assert_eq!(
            a.precomputed_hash(),
            CssIdent::new("div").precomputed_hash()
        );
        assert_ne!(
            a.precomputed_hash(),
            CssIdent::new("span").precomputed_hash()
        );
    }
}
