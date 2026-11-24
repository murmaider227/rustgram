//! Pure compose-stash DTOs and in-memory store (`history/compose_stash.rs` model).

use std::collections::HashMap;

use crate::core_types::dto::rich_compose::RichThreadDraft;
use crate::history::sections_model::ForwardOptions;
use crate::stand_in::types::{MsgId, PeerId};

/// Minimum cut/cleared text length before remembering it for the Ctrl+S stash hint (`kMinLength = 20`).
pub const STASH_HINT_MIN_LENGTH: usize = 20;

/// Memory lifetime for cut-then-pasted text detection (`kMemoryTtl = 120s`).
pub const STASH_HINT_MEMORY_TTL_MS: u64 = 120_000;

/// Maximum number of times the Ctrl+S stash hint toast is shown (`kStashHintMaxShowsCount = 2`).
pub const STASH_HINT_MAX_SHOWS: u32 = 2;

/// Maximum number of prepared files allowed in a single compose stash (`kStashFilesLimit = 100`
/// in `Telegram/SourceFiles/boxes/send_files_box.cpp`).
pub const STASH_FILES_LIMIT: usize = 100;

/// Maximum combined memory/size in bytes of stashed files (`kStashFilesMemoryLimit = 256 MiB`
/// in `Telegram/SourceFiles/boxes/send_files_box.cpp`).
pub const STASH_FILES_MEMORY_LIMIT: u64 = 256 * 1024 * 1024;

/// Key identifying a per-chat / per-topic / per-monoforum compose stash (`Data::DraftKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComposeStashKey {
    /// Owning chat peer id.
    pub peer_id: PeerId,
    /// Optional forum topic or replies thread root message id.
    pub topic_root_id: Option<MsgId>,
    /// Optional monoforum sublist peer id.
    pub monoforum_peer_id: Option<PeerId>,
}

impl ComposeStashKey {
    /// Builds a main-timeline stash key for `peer_id`.
    #[must_use]
    pub const fn main(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            topic_root_id: None,
            monoforum_peer_id: None,
        }
    }

    /// Builds a topic/thread stash key for `(peer_id, topic_root_id)`.
    #[must_use]
    pub const fn topic(peer_id: PeerId, topic_root_id: MsgId) -> Self {
        Self {
            peer_id,
            topic_root_id: Some(topic_root_id),
            monoforum_peer_id: None,
        }
    }
}

/// Prepared attachment stashed alongside a compose draft (`Ui::PreparedFile` summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashedFile {
    /// Display file name.
    pub name: String,
    /// Optional local filesystem path.
    pub path: Option<String>,
    /// File size in bytes.
    pub size_bytes: u64,
    /// MIME type (e.g. `"image/jpeg"`).
    pub mime_type: String,
}

/// Forward draft stashed alongside compose text (`Data::ForwardDraft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashedForward {
    /// Source peer id of the forwarded messages.
    pub from_peer: PeerId,
    /// Message ids queued for forwarding.
    pub msg_ids: Vec<MsgId>,
    /// Forward presentation flags (`drop_author`, `drop_media_captions`).
    pub options: ForwardOptions,
    /// Compact UI summary (e.g. `"2 messages from Alice"`).
    pub summary: String,
}

/// Stashed compose state (`Data::ComposeStash`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComposeStash {
    /// Stashed text, entities, reply target, quote, and webpage toggle.
    pub draft: RichThreadDraft,
    /// Optional reply author label for restoring the composer reply preview bar.
    pub reply_author: Option<String>,
    /// Optional reply snippet for restoring the composer reply preview bar.
    pub reply_preview: Option<String>,
    /// Optional stashed forward draft.
    pub forward: Option<StashedForward>,
    /// Optional stashed prepared files (e.g. from `SendFilesBox`).
    pub files: Vec<StashedFile>,
}

impl ComposeStash {
    /// Returns `true` if any prepared files are stashed (`ComposeStash::hasFiles`).
    #[must_use]
    pub fn has_files(&self) -> bool {
        !self.files.is_empty()
    }

    /// Returns `true` if non-empty text is stashed (`ComposeStash::hasText`).
    #[must_use]
    pub fn has_text(&self) -> bool {
        !self.draft.text.text.is_empty()
    }

    /// Returns `true` if the stash contains text, a reply, forwarded messages, or files.
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.has_text()
            || self.draft.reply_to.is_some()
            || self.forward.as_ref().is_some_and(|f| !f.msg_ids.is_empty())
            || self.has_files()
    }

    /// Returns a compact human-readable preview suitable for tooltips and status toasts.
    #[must_use]
    pub fn preview_label(&self) -> String {
        if self.has_files() {
            let count = self.files.len();
            if self.has_text() {
                format!(
                    "{count} file{} + \"{}\"",
                    if count == 1 { "" } else { "s" },
                    truncate_preview(&self.draft.text.text, 28)
                )
            } else {
                format!("{count} stashed file{}", if count == 1 { "" } else { "s" })
            }
        } else if let Some(ref fwd) = self.forward
            && !fwd.msg_ids.is_empty()
        {
            if self.has_text() {
                format!(
                    "{} + \"{}\"",
                    fwd.summary,
                    truncate_preview(&self.draft.text.text, 24)
                )
            } else {
                fwd.summary.clone()
            }
        } else if self.has_text() {
            format!("\"{}\"", truncate_preview(&self.draft.text.text, 36))
        } else {
            "Stashed draft".to_owned()
        }
    }
}

/// Per-session store of compose stashes keyed by [`ComposeStashKey`].
#[derive(Debug, Clone, Default)]
pub struct ComposeStashStore {
    stashes: HashMap<ComposeStashKey, ComposeStash>,
}

impl ComposeStashStore {
    /// Returns the stashed draft for `key`, if any.
    #[must_use]
    pub fn get(&self, key: ComposeStashKey) -> Option<&ComposeStash> {
        self.stashes.get(&key)
    }

    /// Returns `true` if a non-empty stash exists for `key`.
    #[must_use]
    pub fn has(&self, key: ComposeStashKey) -> bool {
        self.stashes
            .get(&key)
            .is_some_and(ComposeStash::has_content)
    }

    /// Sets or clears the stash for `key`. Empty stashes are removed automatically.
    pub fn set(&mut self, key: ComposeStashKey, stash: Option<ComposeStash>) {
        match stash {
            Some(s) if s.has_content() => {
                self.stashes.insert(key, s);
            }
            _ => {
                self.stashes.remove(&key);
            }
        }
    }

    /// Takes and removes the stash for `key`.
    pub fn take(&mut self, key: ComposeStashKey) -> Option<ComposeStash> {
        self.stashes.remove(&key)
    }

    /// Exchanges `incoming` (current composer content) with any existing stash for `key`.
    ///
    /// Returns the previously stashed draft (to apply to the composer), or `None` if nothing
    /// was stashed before.
    pub fn exchange(
        &mut self,
        key: ComposeStashKey,
        incoming: Option<ComposeStash>,
    ) -> Option<ComposeStash> {
        let previous = self.stashes.remove(&key);
        self.set(key, incoming);
        previous
    }
}

fn truncate_preview(s: &str, max_chars: usize) -> String {
    let single_line = s.replace('\n', " ");
    let mut chars = single_line.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}
