//! Background chat-list loading: every `messages.getDialogs` page of the main list, the archive
//! pages chat folders need, and the folder include/pinned peers the pages did not bring
//! (`messages.getPeerDialogs`).
//!
//! Port origin: `ApiWrap::requestMoreDialogsIfNeeded` (`apiwrap.cpp:1048-1066`), which runs after
//! every applied page (`apiwrap.cpp:1022`) and keeps requesting the main list until
//! `messages.dialogs` / an empty `messages.dialogsSlice` ends it, then the archive while
//! `ChatFilters::archiveNeeded` (`data/data_chat_filters.cpp:891-898`), with
//! `ChatFilters::loadNextExceptions` (`data/data_chat_filters.cpp:942-990`) first once at least
//! `kLoadExceptionsAfter` chats are known. `Folder::applyDialog` (`data/data_folder.cpp:354-367`)
//! asks for the archive while it holds fewer than `kLoadedChatsMinCount` chats, and the archive
//! list asks for its next page when scrolled to the end (`dialogs_widget.cpp:548-561`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_data::{ARCHIVE_FOLDER_ID, ChatFilterFlags, FolderId};
use tg_session::SessionRuntime;
use tg_tl::types as tl_types;
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::media::with_rpc_timeout;
use crate::session::{LiveSession, LiveSessionCache};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;

/// `kLoadExceptionsAfter` (`data/data_chat_filters.cpp`).
const LOAD_EXCEPTIONS_AFTER: usize = tg_api::LOAD_EXCEPTIONS_AFTER;

/// `kLoadedChatsMinCount` (`data/data_folder.cpp:32`).
const ARCHIVE_LOADED_CHATS_MIN_COUNT: usize = 20;

/// One `messages.getDialogs` page of 500 chats can take a while on a slow link.
const PAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Wait before a failed page is requested again.
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(5);

const MAIN_FOLDER_ID: FolderId = 0;

/// Chat-list loading state shared by all clones of a [`LiveSession`].
#[derive(Debug, Default)]
pub(crate) struct DialogsPreload {
    /// A loading task is running.
    running: bool,
    /// The task found nothing left to do; cleared by new demand (filters, archive scroll).
    settled: bool,
    /// `messages.getDialogs` for the main list answered nothing new without finishing it.
    main_exhausted: bool,
    /// Same for the archive.
    archive_exhausted: bool,
    /// The archive list was scrolled to its end and wants its next page.
    archive_page_wanted: bool,
    /// Folders were (re)loaded and their include/pinned peers still have to be checked.
    exceptions_pending: bool,
    /// Filters to hand to the runtime's loader with the next exceptions request.
    pending_filters: Option<tl_types::messages::DialogFilters>,
    /// No new request before this moment after a failed one.
    retry_at: Option<Instant>,
}

/// What the main list, the archive and the folders look like in the session right now.
#[derive(Clone, Copy, Debug)]
struct ListsSnapshot {
    main_loaded: bool,
    main_count: usize,
    archive_loaded: bool,
    archive_count: usize,
    archive_needed: bool,
}

#[derive(Debug)]
enum Step {
    Exceptions(Option<tl_types::messages::DialogFilters>),
    Page(FolderId),
    Idle,
}

impl DialogsPreload {
    /// Picks the next request like `ApiWrap::requestMoreDialogsIfNeeded`.
    fn next_step(&mut self, lists: ListsSnapshot) -> Step {
        let main_done = lists.main_loaded || self.main_exhausted;
        if self.exceptions_pending && (main_done || lists.main_count >= LOAD_EXCEPTIONS_AFTER) {
            return Step::Exceptions(self.pending_filters.take());
        }
        if !main_done {
            return Step::Page(MAIN_FOLDER_ID);
        }
        let archive_done = lists.archive_loaded || self.archive_exhausted;
        if !archive_done
            && (lists.archive_needed
                || self.archive_page_wanted
                || lists.archive_count < ARCHIVE_LOADED_CHATS_MIN_COUNT)
        {
            self.archive_page_wanted = false;
            return Step::Page(ARCHIVE_FOLDER_ID);
        }
        Step::Idle
    }
}

impl LiveSession {
    /// Starts (or keeps) the background chat-list loading.
    ///
    /// `archive_page_wanted` asks for the archive's next page, as scrolling the archive list to
    /// its end does in tdesktop. Cheap to call every frame: it only takes a lock unless there is
    /// new work.
    pub(crate) fn request_more_dialogs(&self, archive_page_wanted: bool) {
        let Ok(mut c) = self.cache.lock() else {
            return;
        };
        let state = &mut c.dialogs_preload;
        if archive_page_wanted && !state.archive_page_wanted && !state.archive_exhausted {
            state.archive_page_wanted = true;
            state.settled = false;
        }
        if state.running || state.settled {
            return;
        }
        if state.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        state.retry_at = None;
        state.running = true;
        drop(c);
        spawn_loading_task(
            Arc::clone(&self.runtime),
            Arc::clone(&self.cache),
            Arc::clone(self.bridge.state()),
        );
    }

    /// Hands freshly loaded folders to the loader so the chats they include but no page brought
    /// are requested with `messages.getPeerDialogs`, and the archive is loaded when a folder
    /// shows archived chats (`ChatFilters::received` -> `requestMoreDialogsIfNeeded`).
    pub(crate) fn dialog_filters_loaded(&self, filters: tl_types::messages::DialogFilters) {
        if let Ok(mut c) = self.cache.lock() {
            let state = &mut c.dialogs_preload;
            state.pending_filters = Some(filters);
            state.exceptions_pending = true;
            state.settled = false;
        }
        self.request_more_dialogs(false);
    }

    /// `true` while the background loading still has pages or folder chats to request.
    #[doc(hidden)]
    pub fn dialogs_preload_running(&self) -> bool {
        self.cache
            .lock()
            .map(|c| c.dialogs_preload.running)
            .unwrap_or(false)
    }
}

fn spawn_loading_task(
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    cache: Arc<Mutex<LiveSessionCache>>,
    bridge: Arc<LiveBridgeState>,
) {
    let handle = runtime.tokio_handle().clone();
    handle.spawn(async move {
        run_loading(&runtime, &cache, &bridge).await;
    });
}

async fn run_loading(
    runtime: &SessionRuntime<ErasedRpcClient>,
    cache: &Mutex<LiveSessionCache>,
    bridge: &LiveBridgeState,
) {
    loop {
        let lists = lists_snapshot(runtime).await;
        let step = match cache.lock() {
            Ok(mut c) => {
                let step = c.dialogs_preload.next_step(lists);
                if matches!(step, Step::Idle) {
                    c.dialogs_preload.running = false;
                    c.dialogs_preload.settled = true;
                }
                step
            }
            Err(_) => return,
        };
        match step {
            Step::Idle => return,
            Step::Exceptions(filters) => {
                let res =
                    with_rpc_timeout(PAGE_TIMEOUT, runtime.load_filter_exceptions(filters)).await;
                match res {
                    Ok(true) => bridge.notify_ui(),
                    Ok(false) => set_state(cache, |s| s.exceptions_pending = false),
                    Err(err) => {
                        // The batch is dropped from the queue like in `loadNextExceptions`'s
                        // fail handler; the chats still show once a page brings them.
                        debug!(error = %err, "messages.getPeerDialogs for folder chats failed");
                        set_state(cache, |s| s.exceptions_pending = false);
                    }
                }
            }
            Step::Page(folder_id) => {
                match with_rpc_timeout(PAGE_TIMEOUT, runtime.load_dialogs_page(folder_id)).await {
                    Ok(Some(page)) => {
                        if page.dialogs.is_empty() && !page.folder_done {
                            mark_exhausted(cache, folder_id);
                        }
                        bridge.notify_ui();
                    }
                    Ok(None) => mark_exhausted(cache, folder_id),
                    Err(err) => {
                        debug!(folder = folder_id, error = %err, "messages.getDialogs page failed");
                        set_state(cache, |s| {
                            s.running = false;
                            s.retry_at = Some(Instant::now() + RETRY_AFTER_ERROR);
                        });
                        return;
                    }
                }
            }
        }
    }
}

fn mark_exhausted(cache: &Mutex<LiveSessionCache>, folder_id: FolderId) {
    set_state(cache, |s| {
        if folder_id == ARCHIVE_FOLDER_ID {
            s.archive_exhausted = true;
        } else {
            s.main_exhausted = true;
        }
    });
}

fn set_state(cache: &Mutex<LiveSessionCache>, f: impl FnOnce(&mut DialogsPreload)) {
    if let Ok(mut c) = cache.lock() {
        f(&mut c.dialogs_preload);
    }
}

async fn lists_snapshot(runtime: &SessionRuntime<ErasedRpcClient>) -> ListsSnapshot {
    let session = runtime.session().read().await;
    ListsSnapshot {
        main_loaded: session.main_list().loaded(),
        main_count: session.main_dialogs().len(),
        archive_loaded: session.archive().chats_list.loaded(),
        archive_count: session.archive_dialogs().len(),
        archive_needed: session
            .filters()
            .list()
            .iter()
            .any(|f| !f.flags.contains(ChatFilterFlags::NO_ARCHIVED)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(main_loaded: bool, main_count: usize) -> ListsSnapshot {
        ListsSnapshot {
            main_loaded,
            main_count,
            archive_loaded: false,
            archive_count: 0,
            archive_needed: false,
        }
    }

    #[test]
    fn main_pages_come_before_archive_and_exceptions_wait_for_a_hundred_chats() {
        let mut state = DialogsPreload {
            exceptions_pending: true,
            ..DialogsPreload::default()
        };
        assert!(matches!(state.next_step(lists(false, 20)), Step::Page(0)));
        assert!(matches!(
            state.next_step(lists(false, LOAD_EXCEPTIONS_AFTER)),
            Step::Exceptions(None)
        ));
        state.exceptions_pending = false;
        assert!(matches!(state.next_step(lists(true, 300)), Step::Page(1)));
    }

    #[test]
    fn archive_loads_past_its_first_chats_only_when_a_folder_or_scroll_needs_it() {
        let mut state = DialogsPreload::default();
        let mut snapshot = ListsSnapshot {
            main_loaded: true,
            main_count: 300,
            archive_loaded: false,
            archive_count: ARCHIVE_LOADED_CHATS_MIN_COUNT,
            archive_needed: false,
        };
        assert!(matches!(state.next_step(snapshot), Step::Idle));
        state.archive_page_wanted = true;
        assert!(matches!(state.next_step(snapshot), Step::Page(1)));
        assert!(!state.archive_page_wanted);
        snapshot.archive_needed = true;
        assert!(matches!(state.next_step(snapshot), Step::Page(1)));
        snapshot.archive_loaded = true;
        assert!(matches!(state.next_step(snapshot), Step::Idle));
    }
}
