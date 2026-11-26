//! Byte-budgeted least-recently-used map for decoded chat media (textures, compositions).
//!
//! Every paint touches what it shows, so the entries on screen are the most recently used ones
//! and are evicted last; an evicted entry is rebuilt through the same path as a first load.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

use egui::TextureHandle;

struct Slot<V> {
    value: V,
    size: usize,
    tick: u64,
}

pub(super) struct AssetLru<K, V> {
    slots: HashMap<K, Slot<V>>,
    order: BTreeMap<u64, K>,
    tick: u64,
    bytes: usize,
    budget: usize,
    evicted: u64,
}

impl<K: Eq + Hash + Clone, V> AssetLru<K, V> {
    pub(super) fn new(budget: usize) -> Self {
        Self {
            slots: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            budget,
            evicted: 0,
        }
    }

    /// Returns the value and marks it most recently used.
    pub(super) fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        let slot = self.slots.get_mut(key)?;
        if let Some(owner) = self.order.remove(&slot.tick) {
            self.order.insert(tick, owner);
        }
        slot.tick = tick;
        Some(&slot.value)
    }

    /// Returns the value mutably and marks it most recently used.
    pub(super) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.tick += 1;
        let tick = self.tick;
        let slot = self.slots.get_mut(key)?;
        if let Some(owner) = self.order.remove(&slot.tick) {
            self.order.insert(tick, owner);
        }
        slot.tick = tick;
        Some(&mut slot.value)
    }

    /// Returns the value without changing its recency.
    pub(super) fn peek(&self, key: &K) -> Option<&V> {
        self.slots.get(key).map(|slot| &slot.value)
    }

    pub(super) fn contains(&self, key: &K) -> bool {
        self.slots.contains_key(key)
    }

    /// Inserts (or replaces) `key` and evicts least recently used entries over the budget (the
    /// new entry itself is kept even when it alone is over the budget). Returns what was evicted.
    pub(super) fn insert(&mut self, key: K, value: V, size: usize) -> Vec<V> {
        let mut evicted = Vec::new();
        self.remove(&key);
        self.tick += 1;
        let tick = self.tick;
        self.order.insert(tick, key.clone());
        self.slots.insert(key, Slot { value, size, tick });
        self.bytes += size;
        while self.bytes > self.budget && self.slots.len() > 1 {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(slot) = self.slots.remove(&oldest) {
                self.bytes -= slot.size;
                self.evicted += 1;
                evicted.push(slot.value);
            }
        }
        evicted
    }

    pub(super) fn remove(&mut self, key: &K) -> Option<V> {
        let slot = self.slots.remove(key)?;
        self.order.remove(&slot.tick);
        self.bytes -= slot.size;
        Some(slot.value)
    }

    pub(super) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(super) const fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) const fn evicted(&self) -> u64 {
        self.evicted
    }
}

/// Textures evicted from a cache, kept for reuse by a new image of the same size: refilling an
/// existing texture (`TextureHandle::set`) skips creating a GPU texture object, which the GL
/// backend does with `glGenTextures` (a 76 ms stall was measured there once).
///
/// A released texture may still be referenced by quads painted in the current frame, so it is
/// only handed out again from the next frame on.
pub(super) struct TexturePool {
    free: HashMap<[usize; 2], Vec<TextureHandle>>,
    free_count: usize,
    cooling: Vec<TextureHandle>,
    pass: u64,
    cap: usize,
    reused: u64,
}

impl TexturePool {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            free: HashMap::new(),
            free_count: 0,
            cooling: Vec::new(),
            pass: u64::MAX,
            cap,
            reused: 0,
        }
    }

    /// Starts egui pass `pass`: textures released in an earlier pass become reusable.
    pub(super) fn begin_pass(&mut self, pass: u64) {
        if pass == self.pass {
            return;
        }
        self.pass = pass;
        for texture in self.cooling.drain(..) {
            if self.free_count >= self.cap {
                break;
            }
            self.free.entry(texture.size()).or_default().push(texture);
            self.free_count += 1;
        }
    }

    pub(super) fn release(&mut self, texture: TextureHandle) {
        self.cooling.push(texture);
    }

    /// A free texture of exactly `size`, if one is pooled.
    pub(super) fn take(&mut self, size: [usize; 2]) -> Option<TextureHandle> {
        let texture = self.free.get_mut(&size)?.pop()?;
        self.free_count -= 1;
        self.reused += 1;
        Some(texture)
    }

    /// How many textures were reused instead of created.
    pub(super) const fn reused(&self) -> u64 {
        self.reused
    }
}

/// GPU bytes of an RGBA8 texture of `size`.
pub(super) fn texture_bytes(size: [usize; 2]) -> usize {
    size[0].saturating_mul(size[1]).saturating_mul(4)
}
