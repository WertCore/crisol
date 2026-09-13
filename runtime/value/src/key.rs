//! Property names.
//!
//! Deliberately not `crisol_tree::Atom`, which is the same idea for a different vocabulary.
//! The two tracks do not converge until M16 (ROADMAP §4) and a runtime crate reaching into
//! the UI tree for a string type would couple them years early. The requirements differ too:
//! `Atom` has a `lowercase` constructor because HTML names are case-insensitive, and applying
//! that to a JavaScript property name would be a bug rather than a convenience.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A property name, interned, with its hash computed once.
///
/// Shape lookup compares names on every step of a chain walk, so the cost that matters is
/// comparison rather than construction: two keys for the same name are almost always the same
/// allocation, which makes the common case a pointer compare.
#[derive(Clone)]
pub struct PropertyKey {
    text: Arc<str>,
    hash: u64,
}

impl PropertyKey {
    /// Interns `name`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            text: Arc::from(name),
            hash: fnv1a(name.as_bytes()),
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The cached hash.
    #[must_use]
    pub const fn hash_value(&self) -> u64 {
        self.hash
    }
}

impl PartialEq for PropertyKey {
    fn eq(&self, other: &Self) -> bool {
        // Pointer first: two keys for the same name usually share an allocation, and when
        // they do this is the whole comparison. The hash second, which rejects almost every
        // mismatch without touching the bytes.
        Arc::ptr_eq(&self.text, &other.text) || (self.hash == other.hash && self.text == other.text)
    }
}

impl Eq for PropertyKey {}

impl Hash for PropertyKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl fmt::Debug for PropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.text)
    }
}

impl fmt::Display for PropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<&str> for PropertyKey {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// FNV-1a, 64-bit.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}
