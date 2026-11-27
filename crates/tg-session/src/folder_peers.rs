//! Chats a folder includes by name, for folders created or edited while the app runs
//! (TGB-2898).
//!
//! The folders loaded at start hand their include/pinned peers to the dialogs loader
//! (`SessionCommand::LoadFilterExceptions`). A folder that changes later (`updateDialogFilter`,
//! a reload after `updateDialogFilters`, or an edit made here) only changes the session; its
//! new chats that no page brought would stay missing until restart. tdesktop queues every
//! changed filter in `ChatFilters::apply` (`_exceptionsToLoad`,
//! `data/data_chat_filters.cpp:775`) and requests the chats whose folder is unknown with
//! `messages.getPeerDialogs` (`loadNextExceptions`). [`FolderPeerWatch`] does the same: it
//! compares the session's folders with the ones it saw last and queues every newly included
//! chat whose folder is still unknown; the runtime sends them with its dialog-entry request.

use std::collections::{BTreeSet, HashMap};

use tg_data::{FilterId, PeerId, Session};

/// The folders' included chats as last seen, and the chats still to request.
#[derive(Debug, Default)]
pub(crate) struct FolderPeerWatch {
    /// `None` until the dialogs loader got the folders: the loader requests the chats of the
    /// folders it starts with itself.
    seen: Option<HashMap<FilterId, BTreeSet<PeerId>>>,
    requests: BTreeSet<PeerId>,
}

impl FolderPeerWatch {
    /// The dialogs loader got the session's current folders; later changes are new.
    pub(crate) fn folders_loaded(&mut self, session: &Session) {
        self.seen = Some(included_peers(session));
    }

    /// Queues the chats the session's folders include now but did not include when last seen,
    /// unless their folder is already known.
    pub(crate) fn collect(&mut self, session: &Session) {
        let Some(seen) = &self.seen else {
            return;
        };
        let filters = session.filters().list();
        let unchanged = filters.len() == seen.len()
            && filters
                .iter()
                .all(|f| seen.get(&f.id).is_some_and(|peers| *peers == f.always));
        if unchanged {
            return;
        }
        for filter in filters {
            let before = seen.get(&filter.id);
            for &peer in &filter.always {
                if before.is_none_or(|peers| !peers.contains(&peer))
                    && folder_unknown(session, peer)
                {
                    self.requests.insert(peer);
                }
            }
        }
        self.seen = Some(included_peers(session));
    }

    /// `true` when chats wait to be requested.
    pub(crate) fn has_requests(&self) -> bool {
        !self.requests.is_empty()
    }

    /// Takes the queued chats whose folder is still unknown.
    pub(crate) fn take_requests(&mut self, session: &Session) -> Vec<PeerId> {
        std::mem::take(&mut self.requests)
            .into_iter()
            .filter(|&peer| folder_unknown(session, peer))
            .collect()
    }
}

fn included_peers(session: &Session) -> HashMap<FilterId, BTreeSet<PeerId>> {
    session
        .filters()
        .list()
        .iter()
        .map(|f| (f.id, f.always.clone()))
        .collect()
}

/// `!history->folderKnown()`; a community channel never gets a dialog entry.
fn folder_unknown(session: &Session, peer: PeerId) -> bool {
    let community = session
        .peer(peer)
        .and_then(tg_data::Peer::as_channel)
        .is_some_and(tg_data::Channel::is_community);
    !community && session.history(peer).is_none_or(|h| !h.folder_known())
}

/// `ChatFilters::loadNextExceptions` waits until the main list is loaded or holds
/// `kLoadExceptionsAfter` chats before it asks for folder chats.
pub(crate) fn lists_ready_for_folder_peers(session: &Session) -> bool {
    session.main_list().loaded() || session.main_dialogs().len() >= tg_api::LOAD_EXCEPTIONS_AFTER
}

#[cfg(test)]
mod tests {
    use tg_data::{ChatFilter, UserId};

    use super::*;

    fn filter(id: FilterId, peers: &[PeerId]) -> ChatFilter {
        ChatFilter {
            id,
            always: peers.iter().copied().collect(),
            ..ChatFilter::default()
        }
    }

    fn user(id: u64) -> PeerId {
        PeerId::from_user(UserId(id))
    }

    #[test]
    fn only_chats_a_folder_gains_after_the_loader_got_the_folders_are_queued() {
        let mut session = Session::new(UserId(1));
        session.set_filter(filter(2, &[user(10)]));
        let mut watch = FolderPeerWatch::default();

        // Before the loader has the folders, nothing is queued: it requests them itself.
        watch.collect(&session);
        assert!(!watch.has_requests());

        watch.folders_loaded(&session);
        watch.collect(&session);
        assert!(!watch.has_requests(), "nothing changed");

        session.set_filter(filter(2, &[user(10), user(11)]));
        session.set_filter(filter(3, &[user(12)]));
        watch.collect(&session);
        assert_eq!(watch.take_requests(&session), vec![user(11), user(12)]);
        assert!(!watch.has_requests());

        // Removing a chat queues nothing; the same state twice queues nothing.
        session.set_filter(filter(2, &[user(10)]));
        watch.collect(&session);
        watch.collect(&session);
        assert!(!watch.has_requests());
    }
}
