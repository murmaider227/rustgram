//! Server salt and `future_salts` bookkeeping for encrypted MTProto sessions.

use std::collections::VecDeque;

use tg_tl::mtproto::types::FutureSalt;

/// Margin in seconds before a salt expires during which we switch to the next future salt.
const SALT_EXPIRY_MARGIN_SECS: i32 = 60;

/// Manages the active server salt and cached `future_salts` for a session.
#[derive(Debug, Default, Clone)]
pub struct SaltManager {
    current: u64,
    future_salts: VecDeque<FutureSalt>,
}

impl SaltManager {
    /// Creates a new `SaltManager` with an initial salt (e.g. `0` or DH-derived initial salt).
    pub fn new(initial_salt: u64) -> Self {
        Self {
            current: initial_salt,
            future_salts: VecDeque::new(),
        }
    }

    /// Returns the best available salt for `now` (server unix seconds).
    ///
    /// Automatically rotates to a valid `FutureSalt` if one covers `now` and has not entered its
    /// expiry margin, falling back to `self.current`.
    pub fn current_salt(&mut self, now: i32) -> u64 {
        self.prune_expired(now);
        for salt in &self.future_salts {
            if salt.valid_since <= now && now + SALT_EXPIRY_MARGIN_SECS < salt.valid_until {
                self.current = salt.salt as u64;
                return self.current;
            }
        }
        self.current
    }

    /// Returns the raw currently active salt without rotating.
    pub fn active_salt(&self) -> u64 {
        self.current
    }

    /// Directly updates the active server salt (e.g. from `bad_server_salt` or `new_session_created`).
    pub fn set_salt(&mut self, salt: u64) {
        if salt != self.current {
            // Discard stale cached future_salts ahead of the server-assigned salt so
            // current_salt() does not revert back to a salt the server just rejected.
            // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:1708, 2726.
            while let Some(front) = self.future_salts.front() {
                if front.salt as u64 == salt {
                    break;
                }
                self.future_salts.pop_front();
            }
        }
        self.current = salt;
    }

    /// Stores salts received in a `future_salts#ae500895` response, sorted by `valid_since`.
    pub fn apply_future_salts(&mut self, now: i32, mut salts: Vec<FutureSalt>) {
        salts.retain(|s| s.valid_until > now);
        salts.sort_by_key(|s| s.valid_since);
        salts.dedup_by_key(|s| (s.valid_since, s.salt));
        self.future_salts = salts.into();
        let _ = self.current_salt(now);
    }

    /// Removes expired salts whose `valid_until <= now`.
    pub fn prune_expired(&mut self, now: i32) {
        while let Some(front) = self.future_salts.front() {
            if front.valid_until <= now {
                self.future_salts.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns true if fewer than 2 non-expired future salts remain.
    pub fn needs_refresh(&mut self, now: i32) -> bool {
        self.prune_expired(now);
        self.future_salts.len() < 2
    }

    /// Returns the number of cached future salts.
    pub fn future_salt_count(&self) -> usize {
        self.future_salts.len()
    }
}
