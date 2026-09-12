//! Interned names.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A refcounted string with a hash computed once.
///
/// Tag names, ids, classes and attribute names are drawn from a small vocabulary that
/// repeats across every node in a document — `div`, `span`, `class`, `href`. A `String` per
/// occurrence is exactly the per-node allocation ROADMAP §M3 calls out as contradicting the
/// product thesis, and cloning one to compare it is worse.
///
/// The cached hash exists for the selector matcher's bloom filter, which asks for a hash on
/// every candidate ancestor of every element it tests.
#[derive(Clone, Default)]
pub struct Atom {
    text: Arc<str>,
    hash: u32,
}

impl Atom {
    /// Interns `text` as written.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self {
            text: Arc::from(text),
            hash: fnv1a(text.as_bytes()),
        }
    }

    /// Interns `text` lowercased, for names that are ASCII case-insensitive.
    ///
    /// HTML tag and attribute names are case-insensitive and `html5ever` hands us lowercase
    /// ones at M4. Normalising here as well means a hand-built tree and a parsed one match
    /// the same selectors, which is otherwise a very confusing difference to debug.
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

    /// True when this is the empty name.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The cached hash.
    ///
    /// Deliberately not what [`Hash`] produces: `Borrow<str>` promises a borrowed and an
    /// owned key hash alike, and this value does not. It exists for the selector bloom
    /// filter, which has no such contract.
    #[must_use]
    pub fn cached_hash(&self) -> u32 {
        self.hash
    }
}

impl PartialEq for Atom {
    fn eq(&self, other: &Self) -> bool {
        // Same allocation is the common case once a document is built; then the hash is a
        // cheap reject; the bytes are the answer.
        Arc::ptr_eq(&self.text, &other.text) || (self.hash == other.hash && self.text == other.text)
    }
}

impl Eq for Atom {}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl Borrow<str> for Atom {
    fn borrow(&self) -> &str {
        &self.text
    }
}

impl PartialEq<str> for Atom {
    fn eq(&self, other: &str) -> bool {
        &*self.text == other
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<&str> for Atom {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<&Atom> for Atom {
    fn from(atom: &Atom) -> Self {
        atom.clone()
    }
}

/// FNV-1a, 32-bit.
///
/// A short loop with no setup cost, which is what these strings want: a tag name is three
/// or four bytes, and a stronger hash spends more time on ceremony than on data.
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
    use std::collections::HashMap;

    #[test]
    fn equal_atoms_compare_equal_across_allocations() {
        assert_eq!(Atom::new("div"), Atom::new("div"));
        assert_ne!(Atom::new("div"), Atom::new("span"));
    }

    #[test]
    fn lowercase_normalises_ascii_only() {
        assert_eq!(Atom::lowercase("DIV").as_str(), "div");
        assert_eq!(Atom::lowercase("Ä").as_str(), "Ä");
    }

    #[test]
    fn hash_agrees_with_borrowed_str() {
        let mut map: HashMap<Atom, u32> = HashMap::new();
        map.insert(Atom::new("div"), 1);
        assert_eq!(
            map.get("div"),
            Some(&1),
            "Hash must agree with Borrow<str> or lookups by &str silently miss"
        );
    }

    #[test]
    fn cached_hash_is_stable_and_discriminating() {
        assert_eq!(
            Atom::new("div").cached_hash(),
            Atom::new("div").cached_hash()
        );
        assert_ne!(
            Atom::new("div").cached_hash(),
            Atom::new("span").cached_hash()
        );
    }
}
