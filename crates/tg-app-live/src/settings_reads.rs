//! Settings reads answered in the background (TGB-3070).
//!
//! The first time a Settings screen asks for its active sessions, blocked users, account TTL,
//! cloud password state or scope notification defaults, the getter returns the cached value
//! (or a placeholder) at once and the request runs off the UI thread, like tdesktop's
//! `request(...).done(...)`; the views are woken when the answer is stored.
//! [`SettingsBackend::settings_read_pending`](tg_app_core::settings::backend::SettingsBackend)
//! tells the screen to show tdesktop's loading state meanwhile.
//!
//! An answer is stored only while its read is still current: a local change made after the
//! request went out ([`SettingsReads::invalidate`]) or a reset of every settings cache
//! ([`SettingsReads::reset`]) makes it stale, so a late answer never overwrites newer state.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tg_api::convert::apply_tl_users_and_chats;
use tg_tl::{enums as tl_enums, functions as tl_fns};
use tracing::debug;

use crate::convert::{authorization_from_tl, now_unix_i32, peer_to_settings_ref};
use crate::session::LiveSession;
use tg_app_core::settings::backend::{ActiveSession, PeerRef, SettingsRead};

/// The reads in flight and what made their answers stale.
#[derive(Debug, Default)]
pub(crate) struct SettingsReads {
    in_flight: HashSet<SettingsRead>,
    /// Reads answered at least once since the last [`Self::reset`].
    answered: HashSet<SettingsRead>,
    generations: HashMap<SettingsRead, u64>,
    epoch: u64,
}

/// One started read; its answer may be stored while [`SettingsReads::is_current`] holds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadTicket {
    read: SettingsRead,
    generation: (u64, u64),
}

impl SettingsReads {
    fn generation(&self, read: SettingsRead) -> (u64, u64) {
        (
            self.epoch,
            self.generations.get(&read).copied().unwrap_or(0),
        )
    }

    /// Starts `read`, or `None` while it is already on its way.
    fn begin(&mut self, read: SettingsRead) -> Option<ReadTicket> {
        self.in_flight.insert(read).then(|| ReadTicket {
            read,
            generation: self.generation(read),
        })
    }

    fn finish(&mut self, ticket: ReadTicket) {
        self.in_flight.remove(&ticket.read);
        if self.is_current(ticket) {
            self.answered.insert(ticket.read);
        }
    }

    /// Whether the answer of `ticket` may still be stored.
    pub(crate) fn is_current(&self, ticket: ReadTicket) -> bool {
        self.generation(ticket.read) == ticket.generation
    }

    /// A local change made answers already on their way for `read` stale; the screen keeps
    /// showing the last value while the next read runs.
    pub(crate) fn invalidate(&mut self, read: SettingsRead) {
        *self.generations.entry(read).or_default() += 1;
    }

    /// Every settings cache was dropped: answers on their way are stale and each screen shows
    /// its loading state again until its next answer.
    pub(crate) fn reset(&mut self) {
        self.epoch += 1;
        self.answered.clear();
    }

    /// `true` while `read` has never been answered and is on its way.
    pub(crate) fn pending(&self, read: SettingsRead) -> bool {
        self.in_flight.contains(&read) && !self.answered.contains(&read)
    }

    fn is_idle(&self) -> bool {
        self.in_flight.is_empty()
    }
}

/// Ends a read when its loader returns or unwinds, so a read is never stuck "in flight".
struct FinishOnDrop {
    live: LiveSession,
    ticket: ReadTicket,
}

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        if let Ok(mut c) = self.live.cache.lock() {
            c.settings_reads.finish(self.ticket);
        }
        let bridge = self.live.bridge.state();
        bridge.mark_views_changed();
        bridge.notify_ui();
    }
}

impl LiveSession {
    /// Runs `load` off the UI thread unless `read` is already on its way. `load` stores its
    /// answer only while [`SettingsReads::is_current`] holds for the ticket it gets.
    pub(crate) fn spawn_settings_read(
        &self,
        read: SettingsRead,
        load: impl FnOnce(&Self, ReadTicket) + Send + 'static,
    ) {
        let Some(ticket) = self
            .cache
            .lock()
            .ok()
            .and_then(|mut c| c.settings_reads.begin(read))
        else {
            return;
        };
        let live = self.clone();
        // The loaders reuse the blocking request helpers, so they run on the blocking pool.
        drop(self.runtime.tokio_handle().spawn_blocking(move || {
            let guard = FinishOnDrop { live, ticket };
            load(&guard.live, ticket);
        }));
    }

    /// `true` while `read` has never been answered and is on its way.
    pub(crate) fn settings_read_pending_now(&self, read: SettingsRead) -> bool {
        self.cache
            .lock()
            .is_ok_and(|c| c.settings_reads.pending(read))
    }

    /// Waits until every Settings read started so far has been answered. Returns `false` after
    /// `limit`. For tests only.
    #[doc(hidden)]
    pub fn wait_settings_reads(&self, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            if self.cache.lock().is_ok_and(|c| c.settings_reads.is_idle()) {
                return true;
            }
            if started.elapsed() >= limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// `account.getAuthorizations` (`Api::Authorizations::reload`). A failure is stored as an
    /// empty list, so the screen does not ask again on every frame.
    pub(crate) fn load_active_sessions(&self, ticket: ReadTicket) {
        let answer = self.invoke_rpc(tl_fns::account::GetAuthorizations {});
        let Ok(mut c) = self.cache.lock() else {
            return;
        };
        if !c.settings_reads.is_current(ticket) {
            return;
        }
        c.sessions_loaded = true;
        match answer {
            Ok(tl_enums::account::Authorizations::Authorizations(resp)) => {
                c.active_sessions = resp
                    .authorizations
                    .iter()
                    .map(|a| {
                        let tl_enums::Authorization::Authorization(inner) = a;
                        authorization_from_tl(inner)
                    })
                    .collect::<Vec<ActiveSession>>();
                if resp.authorization_ttl_days > 0 {
                    c.session_ttl_days = resp.authorization_ttl_days;
                }
            }
            Err(err) => debug!(error = %err, "account.getAuthorizations failed"),
        }
    }

    /// `account.getAccountTTL` (`Api::SelfDestruct::reload`); 180 days when it fails.
    pub(crate) fn load_account_ttl(&self, ticket: ReadTicket) {
        let days = match self.invoke_rpc(tl_fns::account::GetAccountTTL {}) {
            Ok(tl_enums::AccountDaysTTL::AccountDaysTTL(ttl)) => ttl.days,
            Err(err) => {
                debug!(error = %err, "account.getAccountTTL failed");
                180
            }
        };
        if let Ok(mut c) = self.cache.lock()
            && c.settings_reads.is_current(ticket)
            && !c.account_ttl_loaded
        {
            c.account_ttl_loaded = true;
            c.account_ttl_days = days;
        }
    }

    /// `contacts.getBlocked` (`Api::BlockedPeers::reload`); an empty list when it fails.
    pub(crate) fn load_blocked_users(&self, ticket: ReadTicket) {
        let req = tl_fns::contacts::GetBlocked {
            my_stories_from: false,
            offset: 0,
            limit: 100,
        };
        let now = now_unix_i32();
        let list: Vec<PeerRef> = match self.invoke_rpc(req) {
            Ok(resp) => {
                let (blocked, users, chats) = match resp {
                    tl_enums::contacts::Blocked::Blocked(b) => (b.blocked, b.users, b.chats),
                    tl_enums::contacts::Blocked::Slice(b) => (b.blocked, b.users, b.chats),
                };
                self.write_session(|s| {
                    apply_tl_users_and_chats(s, &users, &chats);
                    blocked
                        .iter()
                        .filter_map(|pb| {
                            let tl_enums::PeerBlocked::PeerBlocked(inner) = pb;
                            let pid = tg_api::convert::peer_id_from_tl(&inner.peer_id);
                            s.peer(pid).map(|p| peer_to_settings_ref(p, now))
                        })
                        .collect()
                })
            }
            Err(err) => {
                debug!(error = %err, "contacts.getBlocked failed");
                Vec::new()
            }
        };
        if let Ok(mut c) = self.cache.lock()
            && c.settings_reads.is_current(ticket)
        {
            c.blocked_loaded = true;
            c.blocked_peers = list;
        }
    }

    /// `account.getPassword` (`Core::CloudPasswordState` reload); the default state when it
    /// fails.
    pub(crate) fn load_cloud_password(&self, ticket: ReadTicket) {
        let state = match self.invoke_rpc(tl_fns::account::GetPassword {}) {
            Ok(tl_enums::account::Password::Password(pwd)) => {
                crate::admin::cloud_password_state_from_tl(&pwd)
            }
            Err(err) => {
                debug!(error = %err, "account.getPassword failed");
                Default::default()
            }
        };
        if let Ok(mut c) = self.cache.lock()
            && c.settings_reads.is_current(ticket)
            && !c.cloud_password_loaded
        {
            c.cloud_password_loaded = true;
            c.cloud_password = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_is_pending_until_its_first_answer() {
        let mut reads = SettingsReads::default();
        let read = SettingsRead::AccountTtl;
        let ticket = reads.begin(read).expect("first start");
        assert!(reads.begin(read).is_none(), "one request at a time");
        assert!(reads.pending(read));
        reads.finish(ticket);
        assert!(!reads.pending(read));
        assert!(reads.is_idle());

        // A reload after a local change keeps showing the last value.
        reads.invalidate(read);
        let again = reads.begin(read).expect("second start");
        assert!(!reads.pending(read));
        reads.finish(again);
    }

    #[test]
    fn a_change_made_after_the_request_makes_its_answer_stale() {
        let mut reads = SettingsReads::default();
        let read = SettingsRead::BlockedUsers;
        let ticket = reads.begin(read).expect("start");
        reads.invalidate(read);
        assert!(!reads.is_current(ticket));
        reads.finish(ticket);
        assert!(reads.is_idle());
        assert!(!reads.answered.contains(&read));

        let other = reads.begin(SettingsRead::CloudPassword).expect("start");
        reads.reset();
        assert!(!reads.is_current(other));
    }
}
