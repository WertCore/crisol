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

use crate::value::Address;

/// A property name, interned, with its hash computed once.
///
/// Shape lookup compares names on every step of a chain walk, so the cost that matters is
/// comparison rather than construction: two keys for the same name are almost always the same
/// allocation, which makes the common case a pointer compare.
///
/// # A symbol is a name too
///
/// A key is **either** a string or a symbol, and the difference is identity rather than
/// spelling: two symbols with the same description are different properties, and a symbol is
/// never equal to the string that describes it. So a symbol key carries its address and
/// compares on that, while keeping the description for [`PropertyKey::as_str`] — which exists
/// for display and for the compiler, neither of which ever sees a symbol.
///
/// **The text is not the identity.** Anything deciding behaviour from `as_str` has to ask
/// [`PropertyKey::is_symbol`] first, or a symbol described `"length"` will be mistaken for the
/// property of that name.
#[derive(Clone)]
pub struct PropertyKey {
    text: Arc<str>,
    /// Where the symbol lives, when this names one.
    symbol: Option<Address>,
    hash: u64,
}

impl PropertyKey {
    /// Interns `name`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            text: Arc::from(name),
            symbol: None,
            hash: fnv1a(name.as_bytes()),
        }
    }

    /// A key naming the symbol at `address`, described by `description`.
    ///
    /// The hash comes from the address, not the description: two symbols described alike are
    /// different keys, and hashing them together would put every `Symbol("id")` in one bucket.
    #[must_use]
    pub fn symbol(address: Address, description: &str) -> Self {
        Self {
            text: Arc::from(description),
            symbol: Some(address),
            hash: fnv1a(&address.get().to_le_bytes()),
        }
    }

    /// The name, or a symbol's description.
    ///
    /// **Not an identity.** See the type's documentation: check [`PropertyKey::is_symbol`]
    /// before deciding anything from this.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether this names a symbol rather than a string.
    #[must_use]
    pub const fn is_symbol(&self) -> bool {
        self.symbol.is_some()
    }

    /// Where the symbol lives, when this names one.
    #[must_use]
    pub const fn symbol_address(&self) -> Option<Address> {
        self.symbol
    }

    /// The cached hash.
    #[must_use]
    pub const fn hash_value(&self) -> u64 {
        self.hash
    }
}

impl PartialEq for PropertyKey {
    fn eq(&self, other: &Self) -> bool {
        // **A symbol compares by address and nothing else**, so two symbols described alike
        // stay different properties and neither is the string that describes them.
        if self.symbol.is_some() || other.symbol.is_some() {
            return self.symbol == other.symbol;
        }
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
        if self.symbol.is_some() {
            // Distinguishable from a string key of the same spelling, which is the one thing
            // a reader of this needs.
            return write!(f, "Symbol({})", self.text);
        }
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
