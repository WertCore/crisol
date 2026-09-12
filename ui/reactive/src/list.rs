//! A keyed list reconciler.
//!
//! This is the piece ROADMAP §M7's acceptance actually tests. Rebuilding a list on every
//! change is the easy implementation and the one the milestone rules out; keeping nodes
//! alive across a reorder means matching them by key and moving only what has to move.

use std::collections::HashMap;
use std::hash::Hash;

use crisol_tree::NodeId;

use crate::{Cx, Scope};

/// What a reconcile pass did to the tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListStats {
    /// Items built for keys that were not there before.
    pub created: usize,
    /// Items whose key disappeared, disposed and removed.
    pub removed: usize,
    /// Surviving items that had to change position.
    pub moved: usize,
    /// Surviving items that did not move. The number the milestone cares about.
    pub kept: usize,
}

struct Entry<K> {
    key: K,
    node: NodeId,
    scope: Scope,
}

/// A list of keyed children, reconciled in place.
///
/// The reconciler owns **every child of `parent`**. Mixing reconciled and hand-placed
/// children under one node would make "insert at the end" ambiguous, and the ambiguity would
/// show up as items drifting past their neighbours rather than as an error.
pub struct Keyed<K> {
    parent: NodeId,
    entries: Vec<Entry<K>>,
}

impl<K> std::fmt::Debug for Keyed<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyed")
            .field("parent", &self.parent)
            .field("len", &self.entries.len())
            .finish()
    }
}

impl<K: Eq + Hash + Clone> Keyed<K> {
    /// An empty list under `parent`.
    #[must_use]
    pub fn new(parent: NodeId) -> Self {
        Self {
            parent,
            entries: Vec::new(),
        }
    }

    /// The nodes currently in the list, in document order.
    #[must_use]
    pub fn nodes(&self) -> Vec<NodeId> {
        self.entries.iter().map(|entry| entry.node).collect()
    }

    /// How many items the list holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Makes the children of `parent` match `keys`, building what is new and disposing what
    /// is gone.
    ///
    /// `build` runs inside a fresh [`Scope`] per item, so everything an item creates —
    /// signals, effects, nested lists — is disposed with it. An item that survives is not
    /// rebuilt and its effects are not re-run.
    pub fn reconcile(
        &mut self,
        cx: &mut Cx<'_, '_>,
        keys: &[K],
        mut build: impl FnMut(&mut Cx<'_, '_>, &K) -> NodeId,
    ) -> ListStats {
        let mut stats = ListStats::default();
        let runtime = cx.runtime();

        let mut previous: HashMap<&K, usize> = HashMap::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            previous.insert(&entry.key, index);
        }

        // Which old entry each new position reuses; `usize::MAX` for a fresh build.
        let mut sources = Vec::with_capacity(keys.len());
        let mut built: Vec<Option<(NodeId, Scope)>> = Vec::with_capacity(keys.len());
        let mut reused = vec![false; self.entries.len()];
        for key in keys {
            match previous.get(key) {
                Some(&index) if !reused[index] => {
                    reused[index] = true;
                    sources.push(index);
                    built.push(None);
                }
                // A duplicate key reuses nothing: two items cannot be the same node. The
                // second one is built fresh rather than silently dropped.
                _ => {
                    sources.push(usize::MAX);
                    built.push(None);
                }
            }
        }

        // Dispose the departed before building, so a list that swaps its whole contents
        // does not hold both generations of nodes at once.
        let old = std::mem::take(&mut self.entries);
        let mut survivors: Vec<Option<Entry<K>>> = Vec::with_capacity(old.len());
        for (index, entry) in old.into_iter().enumerate() {
            if reused[index] {
                survivors.push(Some(entry));
            } else {
                // Dispose first: cleanups registered by the item run while its nodes are
                // still alive and can still be read.
                runtime.dispose(entry.scope, cx.dom);
                cx.dom.remove_subtree(entry.node);
                stats.removed += 1;
                survivors.push(None);
            }
        }

        for (position, key) in keys.iter().enumerate() {
            if sources[position] == usize::MAX {
                let (scope, node) = runtime.scope(|_| build(cx, key));
                built[position] = Some((node, scope));
                stats.created += 1;
            }
        }

        // Entries whose relative order is already correct do not move. Everything else is
        // spliced in around them, walking backwards so the node to insert before is always
        // one that has already been placed.
        let anchored = longest_increasing(&sources);
        let mut anchor = anchored.len();
        let mut next: Option<NodeId> = None;
        let mut ordered: Vec<Option<Entry<K>>> = (0..keys.len()).map(|_| None).collect();

        for position in (0..keys.len()).rev() {
            let source = sources[position];
            let (node, scope) = if source == usize::MAX {
                built[position].take().expect("built above")
            } else {
                let entry = survivors[source].take().expect("reused exactly once");
                (entry.node, entry.scope)
            };

            let stays = anchor > 0 && anchored[anchor - 1] == position;
            if stays {
                anchor -= 1;
                stats.kept += 1;
            } else {
                match next {
                    Some(reference) => {
                        cx.dom
                            .insert_before(self.parent, node, reference)
                            .expect("reference is a child of parent");
                    }
                    None => {
                        cx.dom
                            .append_child(self.parent, node)
                            .expect("parent is alive");
                    }
                }
                if source != usize::MAX {
                    stats.moved += 1;
                }
            }
            next = Some(node);
            ordered[position] = Some(Entry {
                key: key_at(keys, position),
                node,
                scope,
            });
        }

        self.entries = ordered
            .into_iter()
            .map(|entry| entry.expect("filled"))
            .collect();
        stats
    }

    /// Disposes every item and removes its nodes.
    pub fn clear(&mut self, cx: &mut Cx<'_, '_>) {
        let runtime = cx.runtime();
        for entry in std::mem::take(&mut self.entries) {
            runtime.dispose(entry.scope, cx.dom);
            cx.dom.remove_subtree(entry.node);
        }
    }
}

fn key_at<K: Clone>(keys: &[K], position: usize) -> K {
    keys[position].clone()
}

/// Indices into `values` forming a longest increasing subsequence, ignoring `usize::MAX`.
///
/// The positions this returns are the ones that can stay where they are; reordering a list
/// by moving everything else is the minimum number of moves. Reversing a ten-item list still
/// costs nine moves, but moving one item from the end to the front costs exactly one.
fn longest_increasing(values: &[usize]) -> Vec<usize> {
    let mut piles: Vec<usize> = Vec::new();
    let mut parent = vec![usize::MAX; values.len()];

    for (index, &value) in values.iter().enumerate() {
        if value == usize::MAX {
            continue;
        }
        let mut low = 0;
        let mut high = piles.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if values[piles[middle]] < value {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low > 0 {
            parent[index] = piles[low - 1];
        }
        if low == piles.len() {
            piles.push(index);
        } else {
            piles[low] = index;
        }
    }

    let mut chain = Vec::with_capacity(piles.len());
    let mut current = piles.last().copied().unwrap_or(usize::MAX);
    while current != usize::MAX {
        chain.push(current);
        current = parent[current];
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::longest_increasing;

    #[test]
    fn an_already_ordered_run_never_moves() {
        assert_eq!(longest_increasing(&[0, 1, 2, 3]), vec![0, 1, 2, 3]);
    }

    #[test]
    fn moving_one_item_to_the_front_anchors_the_rest() {
        // Old order 0,1,2,3 reordered to 3,0,1,2: the tail stays, one node moves.
        assert_eq!(longest_increasing(&[3, 0, 1, 2]), vec![1, 2, 3]);
    }

    #[test]
    fn a_reversal_anchors_exactly_one() {
        assert_eq!(longest_increasing(&[3, 2, 1, 0]).len(), 1);
    }

    #[test]
    fn fresh_entries_are_skipped_rather_than_anchoring() {
        // usize::MAX marks a newly built item; it must not join the subsequence.
        assert_eq!(longest_increasing(&[usize::MAX, 0, 1]), vec![1, 2]);
    }
}
