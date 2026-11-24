//! Stand-in backend trait and in-memory mock implementation replacing `tg-api` + `tg-data`.
//!
//! Port origin: narrow façade over `Telegram/SourceFiles/apiwrap.h`,
//! `Telegram/SourceFiles/main/main_session.h`, and `Telegram/SourceFiles/intro/`.

use std::collections::HashMap;

use crate::stand_in::types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, MessageItem, MsgId, PeerId, PeerKind, PeerSummary,
};

/// Narrow backend contract consumed by all `tg-app` screens.
///
/// Integration replaces [`MockBackend`] with an adapter over `tg-api` + `tg-data`.
pub trait Backend: Send {
    /// Returns the currently authenticated user, if signed in.
    fn current_user(&self) -> Option<&AuthUser>;

    /// Initiates phone login and returns code delivery metadata.
    fn send_login_code(&mut self, phone: &str) -> Result<CodeSentInfo, String>;

    /// Verifies the login code entered by the user.
    fn verify_login_code(&mut self, code: &str) -> Result<CodeVerifyOutcome, String>;

    /// Verifies the 2FA cloud password and signs in.
    fn verify_cloud_password(&mut self, password: &str) -> Result<AuthUser, String>;

    /// Logs out the current session back to the intro flow.
    fn logout(&mut self);

    /// Returns configured chat folders (always starting with All Chats).
    fn folders(&self) -> Vec<FolderFilter>;

    /// Returns dialogs filtered by `folder` and case-insensitive `query`.
    fn dialogs(&self, folder: FilterId, query: &str) -> Vec<DialogEntry>;

    /// Looks up detailed peer info by `peer_id`.
    fn peer_summary(&self, peer_id: PeerId) -> Option<PeerSummary>;

    /// Returns a history slice for `peer_id` ordered oldest-to-newest.
    fn history_slice(&self, peer_id: PeerId) -> Vec<MessageItem>;

    /// Appends a new outgoing message to `peer_id` and returns the created item.
    fn send_message(&mut self, peer_id: PeerId, text: &str) -> Result<MessageItem, String>;

    /// Marks all messages in `peer_id` as read.
    fn mark_read(&mut self, peer_id: PeerId);

    /// Toggles notification mute state for `peer_id`.
    fn toggle_mute(&mut self, peer_id: PeerId) -> bool;

    /// Returns the current simulated or real MTProto connection state.
    fn connection_status(&self) -> ConnectionStatus;

    /// Returns whether a proxy is active.
    fn proxy_enabled(&self) -> bool;

    /// Sets the connection state (used by mock simulator & reconnect button).
    fn set_connection_status(&mut self, status: ConnectionStatus);

    /// Toggles proxy usage state.
    fn set_proxy_enabled(&mut self, enabled: bool);

    /// Requests an immediate reconnect attempt (used by "Try now").
    fn retry_connection(&mut self);
}

/// Deterministic in-memory mock backend with synthetic chats and messages.
///
/// Uses only synthetic test data (never real accounts or phone numbers).
pub struct MockBackend {
    signed_in: bool,
    user: AuthUser,
    pending_phone: String,
    require_2fa: bool,
    expected_code: String,
    expected_password: String,
    folders: Vec<FolderFilter>,
    dialogs: Vec<DialogEntry>,
    histories: HashMap<PeerId, Vec<MessageItem>>,
    next_msg_id: i32,
    connection_status: ConnectionStatus,
    proxy_enabled: bool,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new(false)
    }
}

impl MockBackend {
    /// Creates a new mock backend. Pass `start_signed_in = true` to bypass intro in tests/dev.
    #[must_use]
    pub fn new(start_signed_in: bool) -> Self {
        let self_id = PeerId::from_user(1000);
        let user = AuthUser {
            id: self_id,
            name: "Ada Lovelace".to_owned(),
            username: "ada_dev".to_owned(),
            phone: "+999 661 0000".to_owned(),
            bio: "Porting Telegram Desktop to Rust.".to_owned(),
            initials: "AL".to_owned(),
            avatar_rgb: [0x40, 0xA7, 0xE3],
        };

        let folder_personal = FilterId(1);
        let folder_work = FilterId(2);
        let folder_channels = FilterId(3);

        let peer_saved = self_id;
        let peer_linus = PeerId::from_user(1001);
        let peer_grace = PeerId::from_user(1002);
        let peer_rust_team = PeerId::from_chat(2001);
        let peer_rustgram_dev = PeerId::from_channel(3001);
        let peer_design = PeerId::from_chat(2002);
        let peer_release = PeerId::from_channel(3002);

        let dialogs = vec![
            DialogEntry {
                peer: PeerSummary {
                    id: peer_saved,
                    kind: PeerKind::User,
                    title: "Saved Messages".to_owned(),
                    status: "cloud storage".to_owned(),
                    username: Some("ada_dev".to_owned()),
                    phone_or_link: None,
                    about: "Your personal cloud notes and bookmarked messages.".to_owned(),
                    initials: "SM".to_owned(),
                    avatar_rgb: [0x4D, 0xA6, 0xE8],
                    muted: false,
                    can_send: true,
                    is_saved_messages: true,
                },
                last_message: "Column thresholds: 260 / 380 / 292 px".to_owned(),
                last_sender: None,
                timestamp: "14:22".to_owned(),
                unread_count: 0,
                pinned: true,
                folders: vec![folder_personal],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_rust_team,
                    kind: PeerKind::Group,
                    title: "Rustgram Core Port".to_owned(),
                    status: "20 members, 7 online".to_owned(),
                    username: Some("rustgram_core".to_owned()),
                    phone_or_link: Some("t.me/rustgram_core".to_owned()),
                    about: "Coordination channel for the 20-crate Rust port of tdesktop."
                        .to_owned(),
                    initials: "TC".to_owned(),
                    avatar_rgb: [0xE1, 0x70, 0x76],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                last_message: "Adaptive ThreeColumn layout matches window_session_controller.cpp!"
                    .to_owned(),
                last_sender: Some("Grace".to_owned()),
                timestamp: "14:18".to_owned(),
                unread_count: 3,
                pinned: true,
                folders: vec![folder_work],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_grace,
                    kind: PeerKind::User,
                    title: "Mira Lane".to_owned(),
                    status: "online".to_owned(),
                    username: Some("grace_h".to_owned()),
                    phone_or_link: Some("+999 661 0042".to_owned()),
                    about: "Compilers, protocols, and zero-copy parsers.".to_owned(),
                    initials: "ML".to_owned(),
                    avatar_rgb: [0x7B, 0xC8, 0x62],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                last_message: "Can you verify back navigation from Info -> History?".to_owned(),
                last_sender: None,
                timestamp: "13:55".to_owned(),
                unread_count: 1,
                pinned: false,
                folders: vec![folder_personal, folder_work],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_linus,
                    kind: PeerKind::User,
                    title: "Theo Park".to_owned(),
                    status: "last seen 12 minutes ago".to_owned(),
                    username: Some("theopark".to_owned()),
                    phone_or_link: Some("+999 661 0101".to_owned()),
                    about: "MTProto crypto primitives and SRP verification.".to_owned(),
                    initials: "TP".to_owned(),
                    avatar_rgb: [0xED, 0xA8, 0x6C],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                last_message: "AES-256-IGE test vectors are all green.".to_owned(),
                last_sender: None,
                timestamp: "12:40".to_owned(),
                unread_count: 0,
                pinned: false,
                folders: vec![folder_personal],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_rustgram_dev,
                    kind: PeerKind::Channel,
                    title: "Rustgram Dev Log".to_owned(),
                    status: "1,420 subscribers".to_owned(),
                    username: Some("rustgram_log".to_owned()),
                    phone_or_link: Some("t.me/rustgram_log".to_owned()),
                    about: "Architecture notes, benchmarks, and milestone updates.".to_owned(),
                    initials: "DL".to_owned(),
                    avatar_rgb: [0xA6, 0x95, 0xE7],
                    muted: true,
                    can_send: false,
                    is_saved_messages: false,
                },
                last_message: "Shell now supports persisted geometry and passcode lock.".to_owned(),
                last_sender: None,
                timestamp: "11:05".to_owned(),
                unread_count: 5,
                pinned: false,
                folders: vec![folder_channels, folder_work],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_design,
                    kind: PeerKind::Group,
                    title: "egui & Style System".to_owned(),
                    status: "8 members, 3 online".to_owned(),
                    username: Some("rustgram_ui".to_owned()),
                    phone_or_link: Some("t.me/rustgram_ui".to_owned()),
                    about: "Matching colors.palette and window.style metrics in egui 0.37."
                        .to_owned(),
                    initials: "ES".to_owned(),
                    avatar_rgb: [0x6E, 0xC9, 0xCB],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                last_message: "Using egui/eframe 0.37.0 across tg-ui and tg-app.".to_owned(),
                last_sender: Some("Mira".to_owned()),
                timestamp: "Yesterday".to_owned(),
                unread_count: 0,
                pinned: false,
                folders: vec![folder_work],
            },
            DialogEntry {
                peer: PeerSummary {
                    id: peer_release,
                    kind: PeerKind::Channel,
                    title: "Nightly Builds".to_owned(),
                    status: "312 subscribers".to_owned(),
                    username: Some("rustgram_builds".to_owned()),
                    phone_or_link: Some("t.me/rustgram_builds".to_owned()),
                    about: "Automated Windows x64 MSVC build status announcements.".to_owned(),
                    initials: "NB".to_owned(),
                    avatar_rgb: [0xEE, 0x7A, 0xAE],
                    muted: true,
                    can_send: false,
                    is_saved_messages: false,
                },
                last_message: "clippy -D warnings and cargo fmt passed.".to_owned(),
                last_sender: None,
                timestamp: "Mon".to_owned(),
                unread_count: 0,
                pinned: false,
                folders: vec![folder_channels],
            },
        ];

        let mut histories = HashMap::new();
        histories.insert(
            peer_saved,
            vec![
                MessageItem {
                    id: MsgId(1),
                    peer_id: peer_saved,
                    sender_name: "Ada Lovelace".to_owned(),
                    outgoing: true,
                    text: "Remember: keep all build outputs under target/tg-app.".to_owned(),
                    timestamp: "10:15".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(2),
                    peer_id: peer_saved,
                    sender_name: "Ada Lovelace".to_owned(),
                    outgoing: true,
                    text: "Column thresholds: 260 / 380 / 292 px".to_owned(),
                    timestamp: "14:22".to_owned(),
                    is_read: true,
                },
            ],
        );

        histories.insert(
            peer_rust_team,
            vec![
                MessageItem {
                    id: MsgId(10),
                    peer_id: peer_rust_team,
                    sender_name: "Theo Park".to_owned(),
                    outgoing: false,
                    text: "Morning everyone! How is the tg-app shell shaping up?".to_owned(),
                    timestamp: "14:02".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(11),
                    peer_id: peer_rust_team,
                    sender_name: "Ada Lovelace".to_owned(),
                    outgoing: true,
                    text: "Ported Window::Adaptive, Window::Columns, section stack mementos, and the resizable separators.".to_owned(),
                    timestamp: "14:07".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(12),
                    peer_id: peer_rust_team,
                    sender_name: "Mira".to_owned(),
                    outgoing: false,
                    text: "Nice! Dragging the dialogs splitter below 130px collapses cleanly into the 66px avatar rail too?".to_owned(),
                    timestamp: "14:12".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(13),
                    peer_id: peer_rust_team,
                    sender_name: "Ada Lovelace".to_owned(),
                    outgoing: true,
                    text: "Yes — matches mainwidget.cpp line 2909 (`st::columnMinimalWidthLeft / 2`).".to_owned(),
                    timestamp: "14:15".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(14),
                    peer_id: peer_rust_team,
                    sender_name: "Mira Lane".to_owned(),
                    outgoing: false,
                    text: "Adaptive ThreeColumn layout matches window_session_controller.cpp!".to_owned(),
                    timestamp: "14:18".to_owned(),
                    is_read: false,
                },
            ],
        );

        histories.insert(
            peer_grace,
            vec![
                MessageItem {
                    id: MsgId(20),
                    peer_id: peer_grace,
                    sender_name: "Mira Lane".to_owned(),
                    outgoing: false,
                    text:
                        "Hey! Click the chat header in wide mode to toggle the third info column."
                            .to_owned(),
                    timestamp: "13:50".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(21),
                    peer_id: peer_grace,
                    sender_name: "Mira Lane".to_owned(),
                    outgoing: false,
                    text: "Can you verify back navigation from Info -> History?".to_owned(),
                    timestamp: "13:55".to_owned(),
                    is_read: false,
                },
            ],
        );

        histories.insert(
            peer_linus,
            vec![MessageItem {
                id: MsgId(30),
                peer_id: peer_linus,
                sender_name: "Theo Park".to_owned(),
                outgoing: false,
                text: "AES-256-IGE test vectors are all green.".to_owned(),
                timestamp: "12:40".to_owned(),
                is_read: true,
            }],
        );

        histories.insert(
            peer_rustgram_dev,
            vec![
                MessageItem {
                    id: MsgId(40),
                    peer_id: peer_rustgram_dev,
                    sender_name: "Rustgram Dev Log".to_owned(),
                    outgoing: false,
                    text: "Milestone 1: Native Windows shell with intro flow, adaptive columns, main menu, and passcode lock.".to_owned(),
                    timestamp: "09:30".to_owned(),
                    is_read: true,
                },
                MessageItem {
                    id: MsgId(41),
                    peer_id: peer_rustgram_dev,
                    sender_name: "Rustgram Dev Log".to_owned(),
                    outgoing: false,
                    text: "Shell now supports persisted geometry and passcode lock.".to_owned(),
                    timestamp: "11:05".to_owned(),
                    is_read: false,
                },
            ],
        );

        histories.insert(
            peer_design,
            vec![MessageItem {
                id: MsgId(50),
                peer_id: peer_design,
                sender_name: "Mira".to_owned(),
                outgoing: false,
                text: "Using egui/eframe 0.37.0 across tg-ui and tg-app.".to_owned(),
                timestamp: "Yesterday".to_owned(),
                is_read: true,
            }],
        );

        histories.insert(
            peer_release,
            vec![MessageItem {
                id: MsgId(60),
                peer_id: peer_release,
                sender_name: "Nightly Builds".to_owned(),
                outgoing: false,
                text: "clippy -D warnings and cargo fmt passed.".to_owned(),
                timestamp: "Mon".to_owned(),
                is_read: true,
            }],
        );

        let folders = vec![
            FolderFilter {
                id: FilterId::ALL,
                title: "All Chats".to_owned(),
                icon_glyph: "All".to_owned(),
                unread_count: 0,
            },
            FolderFilter {
                id: folder_personal,
                title: "Personal".to_owned(),
                icon_glyph: "Per".to_owned(),
                unread_count: 0,
            },
            FolderFilter {
                id: folder_work,
                title: "Work".to_owned(),
                icon_glyph: "Wrk".to_owned(),
                unread_count: 0,
            },
            FolderFilter {
                id: folder_channels,
                title: "Channels".to_owned(),
                icon_glyph: "Chn".to_owned(),
                unread_count: 0,
            },
        ];

        let mut backend = Self {
            signed_in: start_signed_in,
            user,
            pending_phone: String::new(),
            require_2fa: true,
            expected_code: "12345".to_owned(),
            expected_password: "rust".to_owned(),
            folders,
            dialogs,
            histories,
            next_msg_id: 100,
            connection_status: ConnectionStatus::Connected,
            proxy_enabled: false,
        };
        backend.recompute_folder_unreads();
        backend
    }

    /// Returns the expected mock code for hints/tests.
    #[must_use]
    pub fn expected_code(&self) -> &str {
        &self.expected_code
    }

    /// Returns the expected mock 2FA password for hints/tests.
    #[must_use]
    pub fn expected_password(&self) -> &str {
        &self.expected_password
    }

    fn recompute_folder_unreads(&mut self) {
        for folder in &mut self.folders {
            folder.unread_count = self
                .dialogs
                .iter()
                .filter(|d| folder.id == FilterId::ALL || d.folders.contains(&folder.id))
                .map(|d| d.unread_count)
                .sum();
        }
    }
}

impl Backend for MockBackend {
    fn current_user(&self) -> Option<&AuthUser> {
        self.signed_in.then_some(&self.user)
    }

    fn send_login_code(&mut self, phone: &str) -> Result<CodeSentInfo, String> {
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return Err("Please enter at least 4 test digits (e.g. +999 661 0000).".to_owned());
        }
        let normalized = phone.trim().to_owned();
        self.pending_phone = normalized.clone();
        self.user.phone = normalized.clone();
        Ok(CodeSentInfo {
            phone: normalized,
            code_length: self.expected_code.len(),
            delivery_hint: format!(
                "Mock mode: enter code {} (2FA password on next step: \"{}\").",
                self.expected_code, self.expected_password
            ),
        })
    }

    fn verify_login_code(&mut self, code: &str) -> Result<CodeVerifyOutcome, String> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err("Please enter the 5-digit verification code.".to_owned());
        }
        if trimmed != self.expected_code {
            return Err(format!(
                "Invalid code. In mock mode, enter {}.",
                self.expected_code
            ));
        }
        if self.require_2fa {
            Ok(CodeVerifyOutcome::PasswordRequired {
                hint: format!("Mock hint: type \"{}\"", self.expected_password),
            })
        } else {
            self.signed_in = true;
            Ok(CodeVerifyOutcome::SignedIn(self.user.clone()))
        }
    }

    fn verify_cloud_password(&mut self, password: &str) -> Result<AuthUser, String> {
        if password.is_empty() {
            return Err("Please enter your cloud password.".to_owned());
        }
        if password != self.expected_password {
            return Err(format!(
                "Incorrect password. In mock mode, enter \"{}\".",
                self.expected_password
            ));
        }
        self.signed_in = true;
        Ok(self.user.clone())
    }

    fn logout(&mut self) {
        self.signed_in = false;
    }

    fn folders(&self) -> Vec<FolderFilter> {
        self.folders.clone()
    }

    fn dialogs(&self, folder: FilterId, query: &str) -> Vec<DialogEntry> {
        let q = query.trim().to_lowercase();
        self.dialogs
            .iter()
            .filter(|d| folder == FilterId::ALL || d.folders.contains(&folder))
            .filter(|d| {
                if q.is_empty() {
                    true
                } else {
                    d.peer.title.to_lowercase().contains(&q)
                        || d.last_message.to_lowercase().contains(&q)
                        || d.peer
                            .username
                            .as_deref()
                            .is_some_and(|u| u.to_lowercase().contains(&q))
                }
            })
            .cloned()
            .collect()
    }

    fn peer_summary(&self, peer_id: PeerId) -> Option<PeerSummary> {
        self.dialogs
            .iter()
            .find(|d| d.peer.id == peer_id)
            .map(|d| d.peer.clone())
    }

    fn history_slice(&self, peer_id: PeerId) -> Vec<MessageItem> {
        self.histories.get(&peer_id).cloned().unwrap_or_default()
    }

    fn send_message(&mut self, peer_id: PeerId, text: &str) -> Result<MessageItem, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let can_send = self
            .dialogs
            .iter()
            .find(|d| d.peer.id == peer_id)
            .is_some_and(|d| d.peer.can_send);
        if !can_send {
            return Err("You cannot send messages in this read-only channel.".to_owned());
        }

        let id = MsgId(self.next_msg_id);
        self.next_msg_id += 1;
        let item = MessageItem {
            id,
            peer_id,
            sender_name: self.user.name.clone(),
            outgoing: true,
            text: trimmed.to_owned(),
            timestamp: "Now".to_owned(),
            is_read: false,
        };
        self.histories
            .entry(peer_id)
            .or_default()
            .push(item.clone());
        if let Some(dialog) = self.dialogs.iter_mut().find(|d| d.peer.id == peer_id) {
            dialog.last_message = trimmed.to_owned();
            dialog.last_sender = None;
            dialog.timestamp = "Now".to_owned();
            dialog.unread_count = 0;
        }
        self.recompute_folder_unreads();
        Ok(item)
    }

    fn mark_read(&mut self, peer_id: PeerId) {
        if let Some(dialog) = self.dialogs.iter_mut().find(|d| d.peer.id == peer_id) {
            dialog.unread_count = 0;
        }
        if let Some(msgs) = self.histories.get_mut(&peer_id) {
            for msg in msgs {
                msg.is_read = true;
            }
        }
        self.recompute_folder_unreads();
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        if let Some(dialog) = self.dialogs.iter_mut().find(|d| d.peer.id == peer_id) {
            dialog.peer.muted = !dialog.peer.muted;
            return dialog.peer.muted;
        }
        false
    }

    fn connection_status(&self) -> ConnectionStatus {
        self.connection_status
    }

    fn proxy_enabled(&self) -> bool {
        self.proxy_enabled
    }

    fn set_connection_status(&mut self, status: ConnectionStatus) {
        self.connection_status = status;
    }

    fn set_proxy_enabled(&mut self, enabled: bool) {
        self.proxy_enabled = enabled;
    }

    fn retry_connection(&mut self) {
        self.connection_status = ConnectionStatus::Connected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_login_flow_phone_code_password() {
        let mut backend = MockBackend::new(false);
        assert!(backend.current_user().is_none());

        assert!(backend.send_login_code("12").is_err());
        let sent = backend
            .send_login_code("+999 661 0000")
            .expect("valid synthetic phone");
        assert_eq!(sent.code_length, 5);

        assert!(backend.verify_login_code("00000").is_err());
        let outcome = backend.verify_login_code("12345").expect("valid mock code");
        assert!(matches!(
            outcome,
            CodeVerifyOutcome::PasswordRequired { .. }
        ));

        assert!(backend.verify_cloud_password("wrong").is_err());
        let user = backend
            .verify_cloud_password("rust")
            .expect("valid mock 2FA password");
        assert_eq!(user.name, "Ada Lovelace");
        assert!(backend.current_user().is_some());

        backend.logout();
        assert!(backend.current_user().is_none());
    }

    #[test]
    fn mock_send_message_and_dialog_filtering() {
        let mut backend = MockBackend::new(true);
        let all = backend.dialogs(FilterId::ALL, "");
        assert!(all.len() >= 5);

        // Sending to a writable peer appends to history and updates dialog preview.
        let target = all[1].peer.id;
        let before = backend.history_slice(target).len();
        let sent = backend
            .send_message(target, "Hello from unit test")
            .expect("writable peer");
        assert!(sent.outgoing);
        assert_eq!(backend.history_slice(target).len(), before + 1);

        // Search narrows dialog list.
        let filtered = backend.dialogs(FilterId::ALL, "grace");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].peer.title, "Mira Lane");

        // Broadcast channel rejects sends.
        let channel = all
            .iter()
            .find(|d| !d.peer.can_send)
            .expect("at least one read-only mock channel")
            .peer
            .id;
        assert!(backend.send_message(channel, "nope").is_err());
    }
}
