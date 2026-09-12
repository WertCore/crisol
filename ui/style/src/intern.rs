//! Sharing one allocation between identically-styled nodes.
//!
//! ROADMAP §M3 is explicit that this is not an optimisation to add later: per-node
//! `ComputedStyle` at document-editor scale is hundreds of megabytes, which contradicts the
//! product thesis. A four-hundred-page document is mostly paragraphs that compute to the
//! same style, and they should cost one allocation between them.

use std::collections::HashMap;
use std::sync::Arc;

use crate::computed::ComputedStyle;

/// Deduplicates computed styles.
///
/// Keyed on the style itself, which is why every type in [`crate::values`] is `Eq + Hash`.
#[derive(Debug, Default)]
pub struct StyleInterner {
    /// The key and the value are the same style; the map exists to find the existing `Arc`.
    styles: HashMap<Arc<ComputedStyle>, ()>,
    hits: u64,
    misses: u64,
}

impl StyleInterner {
    /// An empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a shared handle to `style`, reusing an existing allocation when one matches.
    #[must_use]
    pub fn intern(&mut self, style: ComputedStyle) -> Arc<ComputedStyle> {
        if let Some((existing, ())) = self.styles.get_key_value(&style) {
            self.hits += 1;
            return Arc::clone(existing);
        }
        self.misses += 1;
        let shared = Arc::new(style);
        self.styles.insert(Arc::clone(&shared), ());
        shared
    }

    /// Number of distinct styles held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// True when nothing has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// How many `intern` calls reused an existing allocation.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// How many `intern` calls allocated.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Drops styles nothing outside the interner still holds.
    ///
    /// The interner keeps one reference itself, so a style is unused when its strong count
    /// is one. Restyling a document churns styles, and without this the map grows for the
    /// lifetime of the process.
    pub fn collect_unused(&mut self) -> usize {
        let before = self.styles.len();
        self.styles.retain(|style, ()| Arc::strong_count(style) > 1);
        before - self.styles.len()
    }
}
