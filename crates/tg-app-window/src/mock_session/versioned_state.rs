//! The mock session's shared state behind a lock that counts its changes (TGB-2949).
//!
//! The chat list keeps its rows while [`DialogsBackend::dialogs_version`] stays the same. The
//! mock session had no version, so the trait default gave a new number on every call and the
//! demo rebuilt every chat row on every frame. [`VersionedState::lock`] returns a guard whose
//! mutable access marks the state changed; the version moves when such a guard is dropped, while
//! the lock is still held. Read-only access leaves the version alone.
//!
//! The version is conservative: any mutable access counts as a change, even one that turns out
//! not to change anything, so a cached reader can only reload too often, never miss a change.
//!
//! [`DialogsBackend::dialogs_version`]: tg_app_core::dialogs::backend::DialogsBackend::dialogs_version

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

/// A [`Mutex`] that counts the guards that had mutable access to its value.
#[derive(Debug, Default)]
pub(super) struct VersionedState<T> {
    value: Mutex<T>,
    version: AtomicU64,
}

impl<T> VersionedState<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            version: AtomicU64::new(1),
        }
    }

    /// Locks the state like [`Mutex::lock`].
    pub(super) fn lock(&self) -> LockResult<VersionedGuard<'_, T>> {
        let wrap = |guard| VersionedGuard {
            guard,
            version: &self.version,
            changed: false,
        };
        match self.value.lock() {
            Ok(guard) => Ok(wrap(guard)),
            Err(poisoned) => Err(PoisonError::new(wrap(poisoned.into_inner()))),
        }
    }

    /// Number of changes made so far. Read it before the data it guards.
    pub(super) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

/// Guard of a [`VersionedState`]; mutable access through it moves the version when it drops.
pub(super) struct VersionedGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    version: &'a AtomicU64,
    changed: bool,
}

impl<T> Deref for VersionedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for VersionedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.changed = true;
        &mut self.guard
    }
}

impl<T> Drop for VersionedGuard<'_, T> {
    fn drop(&mut self) {
        // Runs before the inner `MutexGuard` is dropped, so the change and the new version are
        // published under the same lock.
        if self.changed {
            self.version.fetch_add(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VersionedState;

    #[test]
    fn reads_keep_the_version_and_writes_move_it() {
        let state = VersionedState::new(vec![1_u32, 2, 3]);
        let v0 = state.version();
        {
            let guard = state.lock().expect("lock");
            assert_eq!(guard.len(), 3);
            assert_eq!(guard.iter().sum::<u32>(), 6);
        }
        assert_eq!(state.version(), v0, "a read-only lock is not a change");

        if let Ok(mut guard) = state.lock() {
            guard.push(4);
        }
        let v1 = state.version();
        assert!(v1 > v0, "a mutable access moves the version");

        // `mut` binding alone, without mutable access, is not a change.
        #[allow(unused_mut)]
        if let Ok(mut guard) = state.lock() {
            assert_eq!(guard.len(), 4);
        }
        assert_eq!(state.version(), v1);
    }

    #[test]
    fn a_poisoned_lock_still_counts_changes() {
        let state = std::sync::Arc::new(VersionedState::new(0_u32));
        let poisoner = std::sync::Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poison the lock");
        })
        .join();
        let v0 = state.version();
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        *guard += 1;
        drop(guard);
        assert!(state.version() > v0);
    }
}
