//! Deterministic in-memory mock store for WP-27 features:
//! Communities (Subgroups), Managed Bots & Bot Info Localization,
//! Group Moderation (`ModerateMessagesBox`), Welcome Messages, and Channel Anti-Spam.
//!
//! Shared by [`crate::info::mock::MockBackend`] and [`tg_app_core::stand_in::mock_session::MockSession`].
//! All sample names, handles, and IDs are strictly synthetic/fictional.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::boxes::community::{
    COMMUNITY_PEERS_TOO_MUCH, CommunityDetails, CommunityLinkedPeer,
    CommunityParticipantJoinedChats, CommunityPeerLinkAction, CommunityPeerRequest,
    CommunityPeerRequestsSlice, CreateCommunityRequest, DEFAULT_COMMUNITY_PEERS_LIMIT,
};
use crate::boxes::managed_bot::{BotInfoChange, BotInfoTexts};
use crate::boxes::moderate_messages::{ModerateMessagesRequest, ModerateMessagesResult};
use crate::boxes::welcome_messages::{WELCOME_MESSAGES_DEFAULT_LIMIT, WelcomeMessageItem};
use crate::dialogs::community::CommunityDialogGroup;
use tg_app_core::stand_in::types::PeerId;

/// Demo community peer ID (`Rustgram Contributors Hub`).
pub const DEMO_COMMUNITY_ID: PeerId = PeerId::from_channel(3090);

/// Demo discoverable (unjoined) group peer ID linked to [`DEMO_COMMUNITY_ID`].
pub const DEMO_DISCOVERABLE_GROUP_ID: PeerId = PeerId::from_channel(3091);

/// Demo pending link request peer ID #1.
pub const DEMO_PENDING_LINK_PEER_1: PeerId = PeerId::from_channel(3092);

/// Demo pending link request peer ID #2.
pub const DEMO_PENDING_LINK_PEER_2: PeerId = PeerId::from_channel(3093);

/// Base deterministic Unix timestamp (`2026-11-22 12:00:00 UTC`).
const MOCK_NOW_UNIX: i32 = 1_795_348_800;

/// Reusable in-memory state backing WP-27 features in mock backends.
#[derive(Debug, Clone)]
pub struct CommunitiesMockStore {
    communities: HashMap<PeerId, CommunityDetails>,
    pending_requests: HashMap<PeerId, Vec<CommunityPeerRequest>>,
    banned_in_community: HashMap<PeerId, HashSet<PeerId>>,
    next_community_seq: u64,
    next_bot_seq: u64,
    bot_usernames: HashSet<String>,
    bot_infos: HashMap<PeerId, BTreeMap<String, BotInfoTexts>>,
    welcome_messages: HashMap<PeerId, Vec<WelcomeMessageItem>>,
    next_welcome_id: i32,
    anti_spam_enabled: HashMap<PeerId, bool>,
    /// Audit log of anti-spam false positive reports `(peer_id, msg_id)`.
    pub antispam_false_positives: Vec<(PeerId, i32)>,
    /// Audit log of executed moderation requests.
    pub moderation_log: Vec<ModerateMessagesRequest>,
}

impl Default for CommunitiesMockStore {
    fn default() -> Self {
        Self::new_for_peers(
            PeerId::from_user(1000),
            PeerId::from_chat(2001),
            PeerId::from_channel(3001),
            PeerId::from_user(5_000_001),
        )
    }
}

impl CommunitiesMockStore {
    /// Builds deterministic fictional demo data anchored on the given peer IDs.
    #[must_use]
    pub fn new_for_peers(
        self_id: PeerId,
        primary_group: PeerId,
        primary_channel: PeerId,
        demo_bot: PeerId,
    ) -> Self {
        let mut communities = HashMap::new();
        let mut pending_requests = HashMap::new();

        communities.insert(
            DEMO_COMMUNITY_ID,
            CommunityDetails {
                id: DEMO_COMMUNITY_ID,
                title: "Rustgram Contributors Hub".to_owned(),
                about:
                    "Workspace chats, release feeds, and automation bots for Rustgram contributors."
                        .to_owned(),
                collapsed_in_dialogs: false,
                anyone_can_add_peers: true,
                can_edit: true,
                can_manage_linked_peers: true,
                can_delete: true,
                admins_count: 3,
                kicked_count: 1,
                pending_requests_count: 2,
                linked_peers: vec![
                    CommunityLinkedPeer {
                        peer_id: primary_group,
                        title: "Rustgram Core Port".to_owned(),
                        username: Some("rustgram_core".to_owned()),
                        is_channel: false,
                        is_bot: false,
                        members_count: 248,
                        joined: true,
                        visible: Some(true),
                        can_view_history: true,
                        unread_count: 3,
                        muted: false,
                        last_message: "Layer 233 communities RPC schema verified.".to_owned(),
                        last_date_unix: MOCK_NOW_UNIX - 120,
                        color_index: 2,
                    },
                    CommunityLinkedPeer {
                        peer_id: primary_channel,
                        title: "Rustgram Dev Log".to_owned(),
                        username: Some("rustgram_log".to_owned()),
                        is_channel: true,
                        is_bot: false,
                        members_count: 1_420,
                        joined: true,
                        visible: Some(true),
                        can_view_history: true,
                        unread_count: 1,
                        muted: false,
                        last_message: "Nightly build #412 published.".to_owned(),
                        last_date_unix: MOCK_NOW_UNIX - 600,
                        color_index: 4,
                    },
                    CommunityLinkedPeer {
                        peer_id: DEMO_DISCOVERABLE_GROUP_ID,
                        title: "Design Systems Lab".to_owned(),
                        username: Some("rustgram_design_lab".to_owned()),
                        is_channel: false,
                        is_bot: false,
                        members_count: 84,
                        joined: false,
                        visible: Some(true),
                        can_view_history: true,
                        unread_count: 0,
                        muted: false,
                        last_message: String::new(),
                        last_date_unix: MOCK_NOW_UNIX - 3_600,
                        color_index: 5,
                    },
                    CommunityLinkedPeer {
                        peer_id: demo_bot,
                        title: "Catalog Bot".to_owned(),
                        username: Some("catalog_demo_bot".to_owned()),
                        is_channel: false,
                        is_bot: true,
                        members_count: 0,
                        joined: true,
                        visible: Some(true),
                        can_view_history: true,
                        unread_count: 0,
                        muted: false,
                        last_message: String::new(),
                        last_date_unix: MOCK_NOW_UNIX - 7_200,
                        color_index: 1,
                    },
                ],
                color_index: 3,
            },
        );

        pending_requests.insert(
            DEMO_COMMUNITY_ID,
            vec![
                CommunityPeerRequest {
                    peer_id: DEMO_PENDING_LINK_PEER_1,
                    peer_title: "QA & Fuzzing Guild".to_owned(),
                    peer_username: Some("rustgram_qa_guild".to_owned()),
                    is_channel: false,
                    is_bot: false,
                    requested_by: Some(PeerId::from_user(1001)),
                    requested_by_name: "Mira Vance".to_owned(),
                    date: MOCK_NOW_UNIX - 900,
                    visible: true,
                    color_index: 0,
                },
                CommunityPeerRequest {
                    peer_id: DEMO_PENDING_LINK_PEER_2,
                    peer_title: "Release Mirror Feed".to_owned(),
                    peer_username: Some("rustgram_release_mirror".to_owned()),
                    is_channel: true,
                    is_bot: false,
                    requested_by: Some(PeerId::from_user(1003)),
                    requested_by_name: "Sam Chen".to_owned(),
                    date: MOCK_NOW_UNIX - 1_800,
                    visible: false,
                    color_index: 6,
                },
            ],
        );

        let mut bot_usernames = HashSet::new();
        bot_usernames.insert("catalog_demo_bot".to_owned());
        bot_usernames.insert("taken_demo_bot".to_owned());

        let mut bot_infos: HashMap<PeerId, BTreeMap<String, BotInfoTexts>> = HashMap::new();
        let mut demo_bot_langs = BTreeMap::new();
        demo_bot_langs.insert(
            String::new(),
            BotInfoTexts {
                name: "Catalog Bot".to_owned(),
                about: "Interactive demo catalog & managed bot helper.".to_owned(),
                description: "Browse sample components, test inline queries, and manage sub-bots."
                    .to_owned(),
            },
        );
        demo_bot_langs.insert(
            "es".to_owned(),
            BotInfoTexts {
                name: "Bot de Catálogo".to_owned(),
                about: "Catálogo interactivo de demostración.".to_owned(),
                description: "Explora componentes de ejemplo y prueba consultas en línea."
                    .to_owned(),
            },
        );
        bot_infos.insert(demo_bot, demo_bot_langs);

        let mut welcome_messages = HashMap::new();
        welcome_messages.insert(
            primary_group,
            vec![
                WelcomeMessageItem {
                    id: 1,
                    peer_id: primary_group,
                    from_id: Some(self_id),
                    date: MOCK_NOW_UNIX - 86_400,
                    text:
                        "Welcome to Rustgram Core Port! Please read the pinned architecture notes."
                            .to_owned(),
                    out: true,
                    welcome_template: true,
                    invert_media: false,
                },
                WelcomeMessageItem {
                    id: 2,
                    peer_id: primary_group,
                    from_id: Some(self_id),
                    date: MOCK_NOW_UNIX - 86_000,
                    text: "Run cargo test -p tg-app before opening a pull request.".to_owned(),
                    out: true,
                    welcome_template: true,
                    invert_media: false,
                },
            ],
        );

        let mut anti_spam_enabled = HashMap::new();
        anti_spam_enabled.insert(primary_group, false);
        anti_spam_enabled.insert(primary_channel, true);

        Self {
            communities,
            pending_requests,
            banned_in_community: HashMap::new(),
            next_community_seq: 3095,
            next_bot_seq: 5_000_100,
            bot_usernames,
            bot_infos,
            welcome_messages,
            next_welcome_id: 3,
            anti_spam_enabled,
            antispam_false_positives: Vec::new(),
            moderation_log: Vec::new(),
        }
    }

    // -------------------------------------------------------------------------
    // Communities
    // -------------------------------------------------------------------------

    /// Returns all joined communities sorted by ID.
    #[must_use]
    pub fn joined_communities(&self) -> Vec<CommunityDetails> {
        let mut out: Vec<CommunityDetails> = self.communities.values().cloned().collect();
        out.sort_by_key(|c| c.id.0);
        out
    }

    /// Returns details for `community_id`, or if `community_id` is a linked peer inside a
    /// community, returns that owning community when `resolve_linked` is true.
    #[must_use]
    pub fn community_details(&self, community_id: PeerId) -> Option<CommunityDetails> {
        self.communities.get(&community_id).cloned()
    }

    /// Finds the community that `peer_id` is linked to, if any.
    #[must_use]
    pub fn community_for_peer(&self, peer_id: PeerId) -> Option<CommunityDetails> {
        if let Some(direct) = self.communities.get(&peer_id) {
            return Some(direct.clone());
        }
        self.communities
            .values()
            .find(|c| c.linked_peers.iter().any(|lp| lp.peer_id == peer_id))
            .cloned()
    }

    /// Builds [`CommunityDialogGroup`] descriptors for dialogs list grouping.
    #[must_use]
    pub fn community_dialog_groups(&self) -> Vec<CommunityDialogGroup> {
        self.joined_communities()
            .into_iter()
            .map(|c| CommunityDialogGroup {
                community_id: c.id,
                title: c.title,
                collapsed_in_dialogs: c.collapsed_in_dialogs,
                pinned: false,
                member_peer_ids: c
                    .linked_peers
                    .into_iter()
                    .filter(|p| p.joined)
                    .map(|p| p.peer_id)
                    .collect(),
                color_index: c.color_index,
            })
            .collect()
    }

    /// Creates a new community (`communities.create`).
    pub fn create_community(
        &mut self,
        req: CreateCommunityRequest,
        initial_peer_title: &str,
        initial_peer_is_channel: bool,
    ) -> Result<PeerId, String> {
        if req.title.trim().is_empty() {
            return Err("Community title cannot be empty.".to_owned());
        }
        let id = PeerId::from_channel(self.next_community_seq);
        self.next_community_seq += 1;
        let details = CommunityDetails {
            id,
            title: req.title.trim().to_owned(),
            about: req.about.trim().to_owned(),
            collapsed_in_dialogs: false,
            anyone_can_add_peers: true,
            can_edit: true,
            can_manage_linked_peers: true,
            can_delete: true,
            admins_count: 1,
            kicked_count: 0,
            pending_requests_count: 0,
            linked_peers: vec![CommunityLinkedPeer {
                peer_id: req.initial_peer_id,
                title: initial_peer_title.to_owned(),
                username: None,
                is_channel: initial_peer_is_channel,
                is_bot: false,
                members_count: 1,
                joined: true,
                visible: Some(!req.hidden),
                can_view_history: true,
                unread_count: 0,
                muted: false,
                last_message: String::new(),
                last_date_unix: MOCK_NOW_UNIX,
                color_index: (req.initial_peer_id.bare() % 7) as u8,
            }],
            color_index: (id.bare() % 7) as u8,
        };
        self.communities.insert(id, details);
        Ok(id)
    }

    /// Links, updates visibility of, or removes a peer in `community_id` (`communities.togglePeerLink`).
    pub fn toggle_peer_link(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        peer_title: &str,
        is_channel: bool,
        is_bot: bool,
        action: CommunityPeerLinkAction,
    ) -> Result<(), String> {
        let comm = self
            .communities
            .get_mut(&community_id)
            .ok_or_else(|| "Community not found.".to_owned())?;
        match action {
            CommunityPeerLinkAction::Deleted => {
                comm.linked_peers.retain(|p| p.peer_id != peer_id);
                Ok(())
            }
            CommunityPeerLinkAction::Visible | CommunityPeerLinkAction::Hidden => {
                let visible = action == CommunityPeerLinkAction::Visible;
                if let Some(existing) = comm.linked_peers.iter_mut().find(|p| p.peer_id == peer_id)
                {
                    existing.visible = Some(visible);
                    return Ok(());
                }
                if comm.linked_peers.len() >= DEFAULT_COMMUNITY_PEERS_LIMIT {
                    return Err(COMMUNITY_PEERS_TOO_MUCH.to_owned());
                }
                comm.linked_peers.push(CommunityLinkedPeer {
                    peer_id,
                    title: peer_title.to_owned(),
                    username: None,
                    is_channel,
                    is_bot,
                    members_count: 12,
                    joined: true,
                    visible: Some(visible),
                    can_view_history: true,
                    unread_count: 0,
                    muted: false,
                    last_message: String::new(),
                    last_date_unix: MOCK_NOW_UNIX,
                    color_index: (peer_id.bare() % 7) as u8,
                });
                Ok(())
            }
        }
    }

    /// Toggles `collapsed_in_dialogs` (`communities.toggleCommunityCollapsedInDialogs`).
    pub fn toggle_collapsed_in_dialogs(
        &mut self,
        community_id: PeerId,
        collapsed: bool,
    ) -> Result<(), String> {
        let comm = self
            .communities
            .get_mut(&community_id)
            .ok_or_else(|| "Community not found.".to_owned())?;
        comm.collapsed_in_dialogs = collapsed;
        Ok(())
    }

    /// Lists pending peer link requests (`communities.getPeerLinkRequests`).
    #[must_use]
    pub fn peer_link_requests(&self, community_id: PeerId) -> CommunityPeerRequestsSlice {
        let list = self
            .pending_requests
            .get(&community_id)
            .cloned()
            .unwrap_or_default();
        let total_count = list.len() as i32;
        CommunityPeerRequestsSlice {
            list,
            next_offset: String::new(),
            total_count,
        }
    }

    /// Approves (`reject = false`) or rejects (`reject = true`) one peer link request.
    pub fn toggle_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        let reqs = self
            .pending_requests
            .get_mut(&community_id)
            .ok_or_else(|| "No pending requests for community.".to_owned())?;
        let Some(pos) = reqs.iter().position(|r| r.peer_id == peer_id) else {
            return Err("Pending link request not found.".to_owned());
        };
        let req = reqs.remove(pos);
        let remaining = reqs.len() as u32;
        if let Some(comm) = self.communities.get_mut(&community_id) {
            comm.pending_requests_count = remaining;
            if !reject && !comm.linked_peers.iter().any(|p| p.peer_id == req.peer_id) {
                comm.linked_peers.push(CommunityLinkedPeer {
                    peer_id: req.peer_id,
                    title: req.peer_title,
                    username: req.peer_username,
                    is_channel: req.is_channel,
                    is_bot: req.is_bot,
                    members_count: 24,
                    joined: false,
                    visible: Some(req.visible),
                    can_view_history: true,
                    unread_count: 0,
                    muted: false,
                    last_message: String::new(),
                    last_date_unix: req.date,
                    color_index: req.color_index,
                });
            }
        }
        Ok(())
    }

    /// Approves or rejects all pending peer link requests for `community_id`.
    pub fn toggle_all_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        let reqs = self
            .pending_requests
            .remove(&community_id)
            .unwrap_or_default();
        if let Some(comm) = self.communities.get_mut(&community_id) {
            comm.pending_requests_count = 0;
            if !reject {
                for req in reqs {
                    if !comm.linked_peers.iter().any(|p| p.peer_id == req.peer_id) {
                        comm.linked_peers.push(CommunityLinkedPeer {
                            peer_id: req.peer_id,
                            title: req.peer_title,
                            username: req.peer_username,
                            is_channel: req.is_channel,
                            is_bot: req.is_bot,
                            members_count: 24,
                            joined: false,
                            visible: Some(req.visible),
                            can_view_history: true,
                            unread_count: 0,
                            muted: false,
                            last_message: String::new(),
                            last_date_unix: req.date,
                            color_index: req.color_index,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Bans or unbans a participant across a community (`communities.toggleParticipantBanned`).
    pub fn toggle_participant_banned(
        &mut self,
        community_id: PeerId,
        participant_id: PeerId,
        unban: bool,
    ) -> Result<(), String> {
        let set = self.banned_in_community.entry(community_id).or_default();
        if unban {
            set.remove(&participant_id);
        } else {
            set.insert(participant_id);
        }
        if let Some(comm) = self.communities.get_mut(&community_id) {
            comm.kicked_count = set.len() as u32;
        }
        Ok(())
    }

    /// Returns chats joined by `participant_id` inside `community_id`.
    #[must_use]
    pub fn participant_joined_chats(
        &self,
        community_id: PeerId,
        _participant_id: PeerId,
    ) -> CommunityParticipantJoinedChats {
        let Some(comm) = self.communities.get(&community_id) else {
            return CommunityParticipantJoinedChats::default();
        };
        CommunityParticipantJoinedChats {
            creator_chat_ids: Vec::new(),
            joined_chat_ids: comm
                .linked_peers
                .iter()
                .filter(|p| p.joined && !p.is_bot)
                .map(|p| p.peer_id)
                .collect(),
        }
    }

    /// Updates community title and "who can add chats" policy (`ManageCommunityBox`).
    pub fn update_community_settings(
        &mut self,
        community_id: PeerId,
        title: &str,
        only_admins_can_add: bool,
    ) -> Result<(), String> {
        let comm = self
            .communities
            .get_mut(&community_id)
            .ok_or_else(|| "Community not found.".to_owned())?;
        comm.title = title.trim().to_owned();
        comm.anyone_can_add_peers = !only_admins_can_add;
        Ok(())
    }

    /// Deletes a community (`channels.deleteChannel`).
    pub fn delete_community(&mut self, community_id: PeerId) -> Result<(), String> {
        self.communities
            .remove(&community_id)
            .ok_or_else(|| "Community not found.".to_owned())?;
        self.pending_requests.remove(&community_id);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Managed Bots & Bot Info Localization
    // -------------------------------------------------------------------------

    /// Checks whether a bot username is available (`bots.checkUsername`).
    #[must_use]
    pub fn check_bot_username(&self, username: &str) -> bool {
        let lower = username.trim().to_ascii_lowercase();
        lower.len() >= 5 && !self.bot_usernames.contains(&lower)
    }

    /// Creates a new managed bot (`bots.createBot`).
    pub fn create_managed_bot(
        &mut self,
        _manager_id: PeerId,
        name: &str,
        username: &str,
        _via_deeplink: bool,
    ) -> Result<PeerId, String> {
        let lower = username.trim().to_ascii_lowercase();
        if self.bot_usernames.contains(&lower) {
            return Err("USERNAME_OCCUPIED".to_owned());
        }
        let id = PeerId::from_user(self.next_bot_seq);
        self.next_bot_seq += 1;
        self.bot_usernames.insert(lower);
        let mut langs = BTreeMap::new();
        langs.insert(
            String::new(),
            BotInfoTexts {
                name: name.trim().to_owned(),
                about: String::new(),
                description: String::new(),
            },
        );
        self.bot_infos.insert(id, langs);
        Ok(id)
    }

    /// Reads localized bot info (`bots.getBotInfo`).
    #[must_use]
    pub fn get_bot_info(&self, bot_id: PeerId, lang_code: &str) -> BotInfoTexts {
        self.bot_infos
            .get(&bot_id)
            .and_then(|m| m.get(lang_code))
            .cloned()
            .unwrap_or_default()
    }

    /// Writes localized bot info (`bots.setBotInfo`).
    pub fn set_bot_info(
        &mut self,
        bot_id: PeerId,
        lang_code: &str,
        change: &BotInfoChange,
    ) -> Result<(), String> {
        let entry = self
            .bot_infos
            .entry(bot_id)
            .or_default()
            .entry(lang_code.to_owned())
            .or_default();
        if let Some(ref name) = change.name {
            entry.name = name.clone();
        }
        if let Some(ref about) = change.about {
            entry.about = about.clone();
        }
        if let Some(ref desc) = change.description {
            entry.description = desc.clone();
        }
        Ok(())
    }

    /// Deterministic offline translation stub for bot info localization tests & demo mode.
    #[must_use]
    pub fn translate_bot_info_texts(&self, texts: &[String], lang_code: &str) -> Vec<String> {
        let tag = lang_code.to_ascii_uppercase();
        texts
            .iter()
            .map(|t| {
                if t.is_empty() {
                    String::new()
                } else {
                    format!("[{tag}] {t}")
                }
            })
            .collect()
    }

    // -------------------------------------------------------------------------
    // Group Moderation (`ModerateMessagesBox`)
    // -------------------------------------------------------------------------

    /// Executes moderation actions and logs them for demo mode & tests.
    pub fn moderate_messages(
        &mut self,
        req: ModerateMessagesRequest,
    ) -> Result<ModerateMessagesResult, String> {
        let mut community_banned_peers = 0usize;
        if let Some(comm_id) = req.ban_in_community_id {
            for &pid in &req.ban_peers {
                let _ = self.toggle_participant_banned(comm_id, pid, false);
                community_banned_peers += 1;
            }
        }
        let result = ModerateMessagesResult {
            deleted_selected: req.message_ids.len(),
            reported_peers: req.report_spam_peers.len(),
            deleted_all_peers: req.delete_all_from_peers.len(),
            banned_peers: req.ban_peers.len(),
            community_banned_peers,
        };
        self.moderation_log.push(req);
        Ok(result)
    }

    // -------------------------------------------------------------------------
    // Welcome Messages
    // -------------------------------------------------------------------------

    /// Returns stored welcome message templates for `peer_id`.
    #[must_use]
    pub fn welcome_messages(&self, peer_id: PeerId) -> Vec<WelcomeMessageItem> {
        self.welcome_messages
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Appends a new welcome message template (`ephemeral.sendMessage` with `welcome = true`).
    pub fn send_welcome_message(
        &mut self,
        peer_id: PeerId,
        self_id: PeerId,
        text: &str,
    ) -> Result<WelcomeMessageItem, String> {
        let list = self.welcome_messages.entry(peer_id).or_default();
        if list.len() >= WELCOME_MESSAGES_DEFAULT_LIMIT {
            return Err(format!(
                "Maximum of {WELCOME_MESSAGES_DEFAULT_LIMIT} welcome messages reached."
            ));
        }
        let id = self.next_welcome_id;
        self.next_welcome_id += 1;
        let item = WelcomeMessageItem {
            id,
            peer_id,
            from_id: Some(self_id),
            date: MOCK_NOW_UNIX,
            text: text.trim().to_owned(),
            out: true,
            welcome_template: true,
            invert_media: false,
        };
        list.push(item.clone());
        Ok(item)
    }

    /// Edits an existing welcome message template (`ephemeral.editMessage` with `welcome = true`).
    pub fn edit_welcome_message(
        &mut self,
        peer_id: PeerId,
        id: i32,
        text: &str,
    ) -> Result<WelcomeMessageItem, String> {
        let list = self
            .welcome_messages
            .get_mut(&peer_id)
            .ok_or_else(|| "Welcome message not found.".to_owned())?;
        let item = list
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| "Welcome message not found.".to_owned())?;
        item.text = text.trim().to_owned();
        Ok(item.clone())
    }

    /// Deletes one welcome message template (`ephemeral.deleteWelcomeMessage`).
    pub fn delete_welcome_message(&mut self, peer_id: PeerId, id: i32) -> Result<(), String> {
        if let Some(list) = self.welcome_messages.get_mut(&peer_id) {
            list.retain(|m| m.id != id);
        }
        Ok(())
    }

    /// Deletes all welcome message templates for `peer_id` (`ephemeral.deleteAllWelcomeMessages`).
    pub fn delete_all_welcome_messages(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.welcome_messages.remove(&peer_id);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Channel Anti-Spam
    // -------------------------------------------------------------------------

    /// Returns whether aggressive anti-spam is enabled for `peer_id`.
    #[must_use]
    pub fn channel_anti_spam_enabled(&self, peer_id: PeerId) -> bool {
        self.anti_spam_enabled
            .get(&peer_id)
            .copied()
            .unwrap_or(false)
    }

    /// Toggles aggressive anti-spam (`channels.toggleAntiSpam`).
    pub fn toggle_channel_anti_spam(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<(), String> {
        self.anti_spam_enabled.insert(peer_id, enabled);
        Ok(())
    }

    /// Reports an anti-spam false positive (`channels.reportAntiSpamFalsePositive`).
    pub fn report_antispam_false_positive(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> Result<(), String> {
        self.antispam_false_positives.push((peer_id, msg_id));
        Ok(())
    }
}
