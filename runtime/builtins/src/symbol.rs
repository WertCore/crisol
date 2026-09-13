//! `Symbol`.
//!
//! A symbol is a property key that is not a string and is unique by construction. Three kinds
//! exist and they behave differently in ways that are easy to conflate:
//!
//! | | equal to another with the same description? | in the registry? |
//! |---|---|---|
//! | `Symbol("x")` | **no** | no |
//! | `Symbol.for("x")` | **yes** | yes |
//! | `Symbol.iterator` | n/a — there is one | **no** |
//!
//! The third row is the one that catches people: well-known symbols are shared across every
//! realm, which *looks* like registry behaviour, but `Symbol.keyFor(Symbol.iterator)` is
//! `undefined`. They are shared by being well-known, not by being registered, and an
//! implementation that put them in the registry would make `Symbol.for("Symbol.iterator")`
//! return the real one — which is exactly the collision the registry's separate namespace
//! exists to prevent.

use std::collections::HashMap;

/// Identifies a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId(u32);

impl SymbolId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// The well-known symbols, which every realm shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WellKnown {
    /// `Symbol.asyncIterator`.
    AsyncIterator,
    /// `Symbol.hasInstance`.
    HasInstance,
    /// `Symbol.isConcatSpreadable`.
    IsConcatSpreadable,
    /// `Symbol.iterator`.
    Iterator,
    /// `Symbol.match`.
    Match,
    /// `Symbol.matchAll`.
    MatchAll,
    /// `Symbol.replace`.
    Replace,
    /// `Symbol.search`.
    Search,
    /// `Symbol.species`.
    Species,
    /// `Symbol.split`.
    Split,
    /// `Symbol.toPrimitive`.
    ToPrimitive,
    /// `Symbol.toStringTag`.
    ToStringTag,
    /// `Symbol.unscopables`.
    Unscopables,
}

impl WellKnown {
    /// All of them, in specification order.
    pub const ALL: [Self; 13] = [
        Self::AsyncIterator,
        Self::HasInstance,
        Self::IsConcatSpreadable,
        Self::Iterator,
        Self::Match,
        Self::MatchAll,
        Self::Replace,
        Self::Search,
        Self::Species,
        Self::Split,
        Self::ToPrimitive,
        Self::ToStringTag,
        Self::Unscopables,
    ];

    /// The description, which is what `String(Symbol.iterator)` shows.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AsyncIterator => "Symbol.asyncIterator",
            Self::HasInstance => "Symbol.hasInstance",
            Self::IsConcatSpreadable => "Symbol.isConcatSpreadable",
            Self::Iterator => "Symbol.iterator",
            Self::Match => "Symbol.match",
            Self::MatchAll => "Symbol.matchAll",
            Self::Replace => "Symbol.replace",
            Self::Search => "Symbol.search",
            Self::Species => "Symbol.species",
            Self::Split => "Symbol.split",
            Self::ToPrimitive => "Symbol.toPrimitive",
            Self::ToStringTag => "Symbol.toStringTag",
            Self::Unscopables => "Symbol.unscopables",
        }
    }
}

/// Every symbol in a runtime, and the registry.
#[derive(Debug)]
pub struct Symbols {
    /// Description per symbol. `None` is `Symbol()` with no description, which is *not* the
    /// same as `Symbol("")` — `description` reports `undefined` for one and `""` for the other.
    descriptions: Vec<Option<String>>,
    /// `Symbol.for`: key to symbol.
    registry: HashMap<String, SymbolId>,
    /// `Symbol.keyFor`: symbol back to key. Only registered symbols are in here.
    keys: HashMap<SymbolId, String>,
    /// The well-known ones, created up front so they are stable for the realm's life.
    well_known: HashMap<WellKnown, SymbolId>,
}

impl Default for Symbols {
    fn default() -> Self {
        Self::new()
    }
}

impl Symbols {
    /// A fresh set, with the well-known symbols already in it.
    #[must_use]
    pub fn new() -> Self {
        let mut symbols = Self {
            descriptions: Vec::new(),
            registry: HashMap::new(),
            keys: HashMap::new(),
            well_known: HashMap::new(),
        };
        for known in WellKnown::ALL {
            let id = symbols.create(Some(known.description().to_owned()));
            symbols.well_known.insert(known, id);
        }
        symbols
    }

    /// `Symbol(description)` — a new one every time.
    pub fn create(&mut self, description: Option<String>) -> SymbolId {
        let id = SymbolId(u32::try_from(self.descriptions.len()).expect("symbols fit in u32"));
        self.descriptions.push(description);
        id
    }

    /// `symbol.description`.
    ///
    /// `None` for `Symbol()` and `Some("")` for `Symbol("")`, which are different symbols with
    /// different descriptions and are frequently conflated.
    #[must_use]
    pub fn description(&self, symbol: SymbolId) -> Option<&str> {
        self.descriptions.get(symbol.0 as usize)?.as_deref()
    }

    /// `String(symbol)` — `Symbol(x)`, or `Symbol()` when there is no description.
    #[must_use]
    pub fn to_display(&self, symbol: SymbolId) -> String {
        match self.description(symbol) {
            Some(description) => format!("Symbol({description})"),
            None => "Symbol()".to_owned(),
        }
    }

    /// `Symbol.for(key)` — the same symbol for the same key, forever.
    pub fn for_key(&mut self, key: &str) -> SymbolId {
        if let Some(existing) = self.registry.get(key) {
            return *existing;
        }
        let id = self.create(Some(key.to_owned()));
        self.registry.insert(key.to_owned(), id);
        self.keys.insert(id, key.to_owned());
        id
    }

    /// `Symbol.keyFor(symbol)`.
    ///
    /// `None` for anything not made by [`Symbols::for_key`] — including the well-known
    /// symbols, which are shared without being registered.
    #[must_use]
    pub fn key_for(&self, symbol: SymbolId) -> Option<&str> {
        self.keys.get(&symbol).map(String::as_str)
    }

    /// `Symbol.iterator` and friends.
    #[must_use]
    pub fn well_known(&self, known: WellKnown) -> SymbolId {
        *self
            .well_known
            .get(&known)
            .expect("every well-known symbol is created up front")
    }

    /// How many symbols exist.
    #[must_use]
    pub fn count(&self) -> usize {
        self.descriptions.len()
    }
}
