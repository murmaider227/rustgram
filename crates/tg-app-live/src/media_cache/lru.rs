//! Byte-budgeted least-recently-used map for the media cache's in-memory layers.

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

struct Slot<V> {
    value: V,
    size: usize,
    tick: u64,
}

/// A map that keeps at most `budget` bytes of values (as reported on insert), evicting the
/// least recently used entries first. An entry larger than the whole budget is not kept.
pub(crate) struct ByteLru<K, V> {
    slots: HashMap<K, Slot<V>>,
    order: BTreeMap<u64, K>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> ByteLru<K, V> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            slots: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            budget,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Returns a clone of the value and marks it most recently used.
    pub(crate) fn get<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let tick = self.next_tick();
        let slot = self.slots.get_mut(key)?;
        if let Some(owner) = self.order.remove(&slot.tick) {
            self.order.insert(tick, owner);
        }
        slot.tick = tick;
        Some(slot.value.clone())
    }

    pub(crate) fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.slots.contains_key(key)
    }

    /// Inserts (or replaces) `key`, then evicts least recently used entries over the budget.
    pub(crate) fn insert(&mut self, key: K, value: V, size: usize) {
        self.remove(&key);
        if size > self.budget {
            return;
        }
        let tick = self.next_tick();
        self.order.insert(tick, key.clone());
        self.slots.insert(key, Slot { value, size, tick });
        self.bytes += size;
        while self.bytes > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(slot) = self.slots.remove(&oldest) {
                self.bytes -= slot.size;
            }
        }
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let slot = self.slots.remove(key)?;
        self.order.remove(&slot.tick);
        self.bytes -= slot.size;
        Some(slot.value)
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::ByteLru;

    #[test]
    fn evicts_least_recently_used_over_the_byte_budget() {
        let mut lru: ByteLru<String, u32> = ByteLru::new(100);
        lru.insert("a".to_owned(), 1, 40);
        lru.insert("b".to_owned(), 2, 40);
        assert_eq!(lru.get("a"), Some(1)); // `b` is now the oldest
        lru.insert("c".to_owned(), 3, 40);
        assert!(lru.contains("a"));
        assert!(!lru.contains("b"));
        assert!(lru.contains("c"));
        assert_eq!(lru.bytes(), 80);

        // Replacing keeps the byte count exact; an entry over the budget is dropped.
        lru.insert("a".to_owned(), 4, 10);
        assert_eq!(lru.bytes(), 50);
        lru.insert("huge".to_owned(), 5, 101);
        assert!(!lru.contains("huge"));
        assert_eq!(lru.remove("c"), Some(3));
        assert_eq!((lru.len(), lru.bytes()), (1, 10));
        lru.clear();
        assert_eq!((lru.len(), lru.bytes()), (0, 0));
    }
}
