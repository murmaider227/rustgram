//! Deterministic in-memory mock store for WP-10 peer management features:
//! invite links, join requests, peer colors, ownership transfer, similar channels,
//! and the Downloads Manager.
//!
//! Shared by both [`crate::info::mock::MockBackend`] and
//! [`tg_app_core::stand_in::mock_session::MockSession`] so demo mode and tests have
//! consistent, realistic synthetic data (strictly fictional names and IDs).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::info::backend::{
    AdminWithInvitesEntry, ChannelManagementState, ChannelStickerSetRef, DownloadEntry,
    DownloadStatus, InviteImporterInfo, InviteImportersList, InviteLinkInfo, InviteLinkRequest,
    InviteLinksList, JoinRequestsSummary, OwnershipTransferCheck, OwnershipTransferError,
    PeerColorSettings, ProfilePhotoRef, SimilarChannelEntry, SimilarChannelsResult,
    UpdatePeerColorRequest,
};
use tg_app_core::core_types::dto::contacts_extras::{
    ContactProfilePhotoUpload, ExportedContactTokenInfo, ExportedNoPaidMessagesTokenInfo,
    NoPaidMessagesTokenImportResult,
};
use tg_app_core::stand_in::types::PeerId;

/// Demo 2FA password accepted by [`PeerManagementMockStore::transfer_ownership`].
pub const DEMO_TRANSFER_2FA_PASSWORD: &str = "rustgram2fa";

/// Base deterministic Unix timestamp for mock invite/download dates (`2026-11-22 12:00:00 UTC`).
const MOCK_NOW_UNIX: i32 = 1_795_348_800;

/// Reusable in-memory state backing peer management gaps in mock backends.
#[derive(Debug, Clone)]
pub struct PeerManagementMockStore {
    invites: HashMap<PeerId, Vec<InviteLinkInfo>>,
    importers: HashMap<(PeerId, String), Vec<InviteImporterInfo>>,
    join_requests: HashMap<PeerId, Vec<InviteImporterInfo>>,
    colors: HashMap<PeerId, PeerColorSettings>,
    similar_channels: HashMap<PeerId, SimilarChannelsResult>,
    downloads: Vec<DownloadEntry>,
    next_link_seq: u64,
    /// Optional forced pre-check error for testing blocked ownership transfer flows.
    pub forced_transfer_error: Option<OwnershipTransferError>,
    /// Expected 2FA password for mock ownership transfer.
    pub transfer_password: String,
    /// Password hint displayed in the ownership transfer prompt.
    pub transfer_password_hint: String,
    /// Audit log of completed ownership transfers `(peer_id, new_owner_id)`.
    pub completed_transfers: Vec<(PeerId, PeerId)>,
    /// WP-65 cached channel/supergroup management flags per peer.
    pub channel_mgmt: HashMap<PeerId, ChannelManagementState>,
    /// WP-65 admins who created invite links per group/channel (`messages.getAdminsWithInvites`).
    pub admins_with_invites: HashMap<PeerId, Vec<AdminWithInvitesEntry>>,
    /// WP-65 audit log of deactivated collectible usernames per peer.
    pub deactivated_usernames_peers: Vec<PeerId>,
    /// WP-65 audit log of basic chats deleted via `messages.deleteChat`.
    pub deleted_basic_chats: Vec<PeerId>,
    /// WP-65 audit log of gigagroup conversions (`channels.convertToGigagroup`).
    pub gigagroup_conversions: Vec<PeerId>,
    /// WP-66 private contact notes keyed by user peer ID.
    pub contact_notes: HashMap<PeerId, tg_text::TextWithEntities>,
    /// WP-66 audit log of deleted contacts (`contacts.deleteContacts`).
    pub deleted_contacts: Vec<PeerId>,
    /// WP-66 audit log of deleted own profile photo IDs (`photos.deletePhotos`).
    pub deleted_profile_photos: Vec<i64>,
    /// WP-66 audit log of uploaded/suggested/cleared contact profile photos.
    pub uploaded_contact_photos: Vec<(PeerId, ContactProfilePhotoUpload)>,
    /// WP-66 counter for deterministic exported contact tokens.
    contact_token_seq: Arc<AtomicU32>,
    /// WP-66 audit log of imported contact tokens (`contacts.importContactToken`).
    pub imported_contact_tokens: Vec<String>,
    /// WP-66 audit log of imported no-paid-messages tokens `(peer_id, token)`.
    pub imported_no_paid_tokens: Vec<(PeerId, String)>,
}

impl Default for PeerManagementMockStore {
    fn default() -> Self {
        Self::new_for_peers(
            PeerId::from_user(1000),
            PeerId::from_chat(2001),
            PeerId::from_channel(3001),
            PeerId::from_channel(3002),
        )
    }
}

impl PeerManagementMockStore {
    /// Builds deterministic fictional demo data anchored on the given self, group, and channel IDs.
    #[must_use]
    pub fn new_for_peers(
        self_id: PeerId,
        primary_group: PeerId,
        primary_channel: PeerId,
        secondary_channel: PeerId,
    ) -> Self {
        let mut invites: HashMap<PeerId, Vec<InviteLinkInfo>> = HashMap::new();
        let mut importers: HashMap<(PeerId, String), Vec<InviteImporterInfo>> = HashMap::new();
        let mut join_requests: HashMap<PeerId, Vec<InviteImporterInfo>> = HashMap::new();
        let mut colors: HashMap<PeerId, PeerColorSettings> = HashMap::new();
        let mut similar_channels: HashMap<PeerId, SimilarChannelsResult> = HashMap::new();

        let group_perm_link = "https://t.me/+RustGramCorePerm01".to_owned();
        let group_expiring_link = "https://t.me/+RustGramSprint02".to_owned();
        let group_approval_link = "https://t.me/+RustGramReview03".to_owned();
        let group_revoked_link = "https://t.me/+RustGramOldRev04".to_owned();

        invites.insert(
            primary_group,
            vec![
                InviteLinkInfo {
                    link: group_perm_link.clone(),
                    title: String::new(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 86_400 * 30,
                    start_date: None,
                    expire_date: None,
                    usage_limit: None,
                    usage: 14,
                    requested: 0,
                    permanent: true,
                    revoked: false,
                    request_needed: false,
                },
                InviteLinkInfo {
                    link: group_expiring_link.clone(),
                    title: "Release Sprint".to_owned(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 86_400 * 2,
                    start_date: Some(MOCK_NOW_UNIX - 86_400 * 2),
                    expire_date: Some(MOCK_NOW_UNIX + 86_400 * 5),
                    usage_limit: Some(25),
                    usage: 7,
                    requested: 0,
                    permanent: false,
                    revoked: false,
                    request_needed: false,
                },
                InviteLinkInfo {
                    link: group_approval_link.clone(),
                    title: "Contributor Screening".to_owned(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 86_400,
                    start_date: None,
                    expire_date: None,
                    usage_limit: None,
                    usage: 3,
                    requested: 2,
                    permanent: false,
                    revoked: false,
                    request_needed: true,
                },
                InviteLinkInfo {
                    link: group_revoked_link.clone(),
                    title: "Archived Beta Link".to_owned(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 86_400 * 60,
                    start_date: None,
                    expire_date: None,
                    usage_limit: Some(10),
                    usage: 10,
                    requested: 0,
                    permanent: false,
                    revoked: true,
                    request_needed: false,
                },
            ],
        );

        invites.insert(
            primary_channel,
            vec![
                InviteLinkInfo {
                    link: "https://t.me/+RustGramChanPerm01".to_owned(),
                    title: String::new(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 86_400 * 14,
                    start_date: None,
                    expire_date: None,
                    usage_limit: None,
                    usage: 42,
                    requested: 0,
                    permanent: true,
                    revoked: false,
                    request_needed: false,
                },
                InviteLinkInfo {
                    link: "https://t.me/+RustGramChanVip02".to_owned(),
                    title: "Early Testers".to_owned(),
                    admin_id: self_id,
                    admin_name: "Taylor Kim".to_owned(),
                    date: MOCK_NOW_UNIX - 3_600 * 12,
                    start_date: Some(MOCK_NOW_UNIX - 3_600 * 12),
                    expire_date: Some(MOCK_NOW_UNIX + 3_600 * 36),
                    usage_limit: Some(50),
                    usage: 11,
                    requested: 0,
                    permanent: false,
                    revoked: false,
                    request_needed: false,
                },
            ],
        );

        importers.insert(
            (primary_group, group_perm_link),
            vec![
                InviteImporterInfo {
                    user_id: PeerId::from_user(1001),
                    name: "Mira Vance".to_owned(),
                    username: Some("mira_v".to_owned()),
                    about: Some("Systems engineer".to_owned()),
                    date: MOCK_NOW_UNIX - 86_400 * 10,
                    requested: false,
                    via_chatlist: false,
                    approved_by: None,
                    color_index: 1,
                },
                InviteImporterInfo {
                    user_id: PeerId::from_user(1003),
                    name: "Sam Chen".to_owned(),
                    username: Some("sam_chen".to_owned()),
                    about: None,
                    date: MOCK_NOW_UNIX - 86_400 * 4,
                    requested: false,
                    via_chatlist: true,
                    approved_by: None,
                    color_index: 3,
                },
            ],
        );
        importers.insert(
            (primary_group, group_expiring_link),
            vec![InviteImporterInfo {
                user_id: PeerId::from_user(1004),
                name: "Riley Brooks".to_owned(),
                username: Some("riley_b".to_owned()),
                about: Some("QA automation".to_owned()),
                date: MOCK_NOW_UNIX - 3_600 * 6,
                requested: false,
                via_chatlist: false,
                approved_by: None,
                color_index: 4,
            }],
        );

        let group_requests = vec![
            InviteImporterInfo {
                user_id: PeerId::from_user(1051),
                name: "Avery Quinn".to_owned(),
                username: Some("avery_q".to_owned()),
                about: Some("Rust GUI contributor interested in egui layout".to_owned()),
                date: MOCK_NOW_UNIX - 1_800,
                requested: true,
                via_chatlist: false,
                approved_by: None,
                color_index: 2,
            },
            InviteImporterInfo {
                user_id: PeerId::from_user(1052),
                name: "Devon Morales".to_owned(),
                username: Some("devon_m".to_owned()),
                about: Some("MTProto protocol tester".to_owned()),
                date: MOCK_NOW_UNIX - 5_400,
                requested: true,
                via_chatlist: false,
                approved_by: None,
                color_index: 5,
            },
        ];
        importers.insert((primary_group, group_approval_link), group_requests.clone());
        join_requests.insert(primary_group, group_requests);

        colors.insert(
            self_id,
            PeerColorSettings {
                color_index: Some(5),
                background_emoji_id: 5368324170671202286,
                profile_color_index: Some(3),
                profile_background_emoji_id: 0,
            },
        );
        colors.insert(
            primary_channel,
            PeerColorSettings {
                color_index: Some(4),
                background_emoji_id: 0,
                profile_color_index: Some(1),
                profile_background_emoji_id: 0,
            },
        );
        colors.insert(
            primary_group,
            PeerColorSettings {
                color_index: Some(2),
                background_emoji_id: 0,
                profile_color_index: None,
                profile_background_emoji_id: 0,
            },
        );

        let recs = SimilarChannelsResult {
            channels: vec![
                SimilarChannelEntry {
                    peer_id: secondary_channel,
                    title: "Nightly Builds".to_owned(),
                    username: Some("rustgram_builds".to_owned()),
                    subscribers_count: 8_420,
                    verified: true,
                    color_index: 2,
                },
                SimilarChannelEntry {
                    peer_id: PeerId::from_channel(3901),
                    title: "Rust Async Patterns".to_owned(),
                    username: Some("rust_async_lab".to_owned()),
                    subscribers_count: 24_150,
                    verified: true,
                    color_index: 5,
                },
                SimilarChannelEntry {
                    peer_id: PeerId::from_channel(3902),
                    title: "Desktop UI Craft".to_owned(),
                    username: Some("desktop_ui_craft".to_owned()),
                    subscribers_count: 11_300,
                    verified: false,
                    color_index: 0,
                },
                SimilarChannelEntry {
                    peer_id: PeerId::from_channel(3903),
                    title: "Cryptography Notes".to_owned(),
                    username: Some("crypto_notes_ch".to_owned()),
                    subscribers_count: 6_890,
                    verified: false,
                    color_index: 4,
                },
            ],
            total_count: 14,
            has_more_for_premium: true,
        };
        similar_channels.insert(primary_channel, recs.clone());
        similar_channels.insert(secondary_channel, recs);

        let downloads = vec![
            DownloadEntry {
                id: 1,
                peer_id: primary_group,
                peer_title: "Rustgram Core Port".to_owned(),
                msg_id: 108,
                file_name: "architecture_layer233.pdf".to_owned(),
                mime_type: "application/pdf".to_owned(),
                total_bytes: 4_820_992,
                local_path: None,
                started_at: MOCK_NOW_UNIX - 45,
                date_group: "Today".to_owned(),
                status: DownloadStatus::Downloading {
                    received_bytes: 2_892_595,
                    total_bytes: 4_820_992,
                },
            },
            DownloadEntry {
                id: 2,
                peer_id: primary_channel,
                peer_title: "Rustgram Dev Log".to_owned(),
                msg_id: 204,
                file_name: "demo_capture_60fps.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
                total_bytes: 18_450_000,
                local_path: None,
                started_at: MOCK_NOW_UNIX - 300,
                date_group: "Today".to_owned(),
                status: DownloadStatus::Paused {
                    received_bytes: 6_457_500,
                    total_bytes: 18_450_000,
                },
            },
            DownloadEntry {
                id: 3,
                peer_id: primary_group,
                peer_title: "Rustgram Core Port".to_owned(),
                msg_id: 94,
                file_name: "nightly_symbols.zip".to_owned(),
                mime_type: "application/zip".to_owned(),
                total_bytes: 12_340_224,
                local_path: Some("downloads/nightly_symbols.zip".to_owned()),
                started_at: MOCK_NOW_UNIX - 86_400,
                date_group: "Yesterday".to_owned(),
                status: DownloadStatus::Completed,
            },
            DownloadEntry {
                id: 4,
                peer_id: secondary_channel,
                peer_title: "Nightly Builds".to_owned(),
                msg_id: 55,
                file_name: "theme_palette_pack.tdesktop-theme".to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                total_bytes: 94_208,
                local_path: Some("downloads/theme_palette_pack.tdesktop-theme".to_owned()),
                started_at: MOCK_NOW_UNIX - 86_400 * 3,
                date_group: "November 2026".to_owned(),
                status: DownloadStatus::Completed,
            },
        ];

        let mut channel_mgmt: HashMap<PeerId, ChannelManagementState> = HashMap::new();
        channel_mgmt.insert(
            primary_group,
            ChannelManagementState {
                pre_history_hidden: true,
                is_forum: false,
                forum_tabs: false,
                view_forum_as_messages: false,
                participants_hidden: false,
                is_gigagroup: false,
                boosts_unrestrict: 2,
                boosts_applied: 1,
                stars_per_message: 0,
                broadcast_messages_allowed: false,
                autotranslation: false,
                can_set_stickers: true,
                sticker_set: Some(ChannelStickerSetRef::ShortName(
                    "rust_core_stickers".to_owned(),
                )),
                emoji_set: None,
            },
        );
        channel_mgmt.insert(
            primary_channel,
            ChannelManagementState {
                pre_history_hidden: false,
                is_forum: false,
                forum_tabs: false,
                view_forum_as_messages: false,
                participants_hidden: false,
                is_gigagroup: false,
                boosts_unrestrict: 0,
                boosts_applied: 3,
                stars_per_message: 10,
                broadcast_messages_allowed: true,
                autotranslation: true,
                can_set_stickers: false,
                sticker_set: None,
                emoji_set: None,
            },
        );

        let mut admins_with_invites: HashMap<PeerId, Vec<AdminWithInvitesEntry>> = HashMap::new();
        admins_with_invites.insert(
            primary_group,
            vec![
                AdminWithInvitesEntry {
                    admin_id: self_id,
                    name: "Taylor Kim".to_owned(),
                    username: Some("rustgram_tester".to_owned()),
                    is_self: true,
                    invites_count: 3,
                    revoked_invites_count: 1,
                },
                AdminWithInvitesEntry {
                    admin_id: PeerId::from_user(1001),
                    name: "Mira Vance".to_owned(),
                    username: Some("mira_vance".to_owned()),
                    is_self: false,
                    invites_count: 4,
                    revoked_invites_count: 1,
                },
                AdminWithInvitesEntry {
                    admin_id: PeerId::from_user(1002),
                    name: "Kaelen Orlov".to_owned(),
                    username: Some("kaelen_orlov".to_owned()),
                    is_self: false,
                    invites_count: 2,
                    revoked_invites_count: 0,
                },
            ],
        );
        admins_with_invites.insert(
            primary_channel,
            vec![AdminWithInvitesEntry {
                admin_id: PeerId::from_user(1001),
                name: "Mira Vance".to_owned(),
                username: Some("mira_vance".to_owned()),
                is_self: false,
                invites_count: 2,
                revoked_invites_count: 0,
            }],
        );

        let mut contact_notes: HashMap<PeerId, tg_text::TextWithEntities> = HashMap::new();
        contact_notes.insert(
            PeerId::from_user(1001),
            tg_text::TextWithEntities::simple("Met at Rust UI meetup; prefers async code reviews."),
        );

        Self {
            invites,
            importers,
            join_requests,
            colors,
            similar_channels,
            downloads,
            next_link_seq: 100,
            forced_transfer_error: None,
            transfer_password: DEMO_TRANSFER_2FA_PASSWORD.to_owned(),
            transfer_password_hint: "demo hint: rustgram2fa".to_owned(),
            completed_transfers: Vec::new(),
            channel_mgmt,
            admins_with_invites,
            deactivated_usernames_peers: Vec::new(),
            deleted_basic_chats: Vec::new(),
            gigagroup_conversions: Vec::new(),
            contact_notes,
            deleted_contacts: Vec::new(),
            deleted_profile_photos: Vec::new(),
            uploaded_contact_photos: Vec::new(),
            contact_token_seq: Arc::new(AtomicU32::new(0)),
            imported_contact_tokens: Vec::new(),
            imported_no_paid_tokens: Vec::new(),
        }
    }

    /// Lists active or revoked invite links for `peer_id`.
    #[must_use]
    pub fn exported_chat_invites(&self, peer_id: PeerId, revoked: bool) -> InviteLinksList {
        let links: Vec<InviteLinkInfo> = self
            .invites
            .get(&peer_id)
            .map(|list| {
                list.iter()
                    .filter(|l| l.revoked == revoked)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        InviteLinksList {
            count: links.len() as i32,
            links,
        }
    }

    /// Creates a new invite link for `peer_id`.
    pub fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        admin_id: PeerId,
        admin_name: &str,
        req: InviteLinkRequest,
    ) -> InviteLinkInfo {
        self.next_link_seq += 1;
        let url = format!("https://t.me/+RustGramLink{}", self.next_link_seq);
        let usage_limit = if req.request_needed {
            None
        } else {
            req.usage_limit.filter(|&n| n > 0)
        };
        let list = self.invites.entry(peer_id).or_default();
        if req.legacy_revoke_permanent {
            for item in list.iter_mut() {
                if item.permanent && !item.revoked {
                    item.revoked = true;
                }
            }
        }
        let permanent = req.legacy_revoke_permanent
            || (list.iter().all(|l| !l.permanent || l.revoked)
                && req.title.is_empty()
                && req.expire_date.is_none()
                && usage_limit.is_none()
                && !req.request_needed);
        let entry = InviteLinkInfo {
            link: url,
            title: req.title.trim().to_owned(),
            admin_id,
            admin_name: admin_name.to_owned(),
            date: MOCK_NOW_UNIX,
            start_date: req.expire_date.map(|_| MOCK_NOW_UNIX),
            expire_date: req.expire_date.filter(|&d| d > 0),
            usage_limit,
            usage: 0,
            requested: 0,
            permanent,
            revoked: false,
            request_needed: req.request_needed,
        };
        if permanent {
            list.insert(0, entry.clone());
        } else {
            let insert_pos = usize::from(list.first().is_some_and(|l| l.permanent && !l.revoked));
            list.insert(insert_pos, entry.clone());
        }
        entry
    }

    /// Edits an existing invite link's fields.
    pub fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        let list = self
            .invites
            .get_mut(&peer_id)
            .ok_or_else(|| "No invite links found for this chat.".to_owned())?;
        let item = list
            .iter_mut()
            .find(|l| l.link == link)
            .ok_or_else(|| "Invite link not found.".to_owned())?;
        if item.revoked {
            return Err("Revoked invite links cannot be edited.".to_owned());
        }
        item.title = req.title.trim().to_owned();
        item.expire_date = req.expire_date.filter(|&d| d > 0);
        item.start_date = item.expire_date.map(|_| MOCK_NOW_UNIX);
        item.request_needed = req.request_needed;
        item.usage_limit = if req.request_needed {
            None
        } else {
            req.usage_limit.filter(|&n| n > 0)
        };
        Ok(item.clone())
    }

    /// Revokes an invite link; revoking a permanent link automatically creates a fresh permanent link.
    pub fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        admin_id: PeerId,
        admin_name: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        let list = self
            .invites
            .get_mut(&peer_id)
            .ok_or_else(|| "No invite links found for this chat.".to_owned())?;
        let idx = list
            .iter()
            .position(|l| l.link == link)
            .ok_or_else(|| "Invite link not found.".to_owned())?;
        list[idx].revoked = true;
        let was_permanent = list[idx].permanent;
        let revoked_copy = list[idx].clone();

        let replacement = if was_permanent {
            self.next_link_seq += 1;
            let fresh = InviteLinkInfo {
                link: format!("https://t.me/+RustGramPerm{}", self.next_link_seq),
                title: String::new(),
                admin_id,
                admin_name: admin_name.to_owned(),
                date: MOCK_NOW_UNIX,
                start_date: None,
                expire_date: None,
                usage_limit: None,
                usage: 0,
                requested: 0,
                permanent: true,
                revoked: false,
                request_needed: false,
            };
            if let Some(list) = self.invites.get_mut(&peer_id) {
                list.insert(0, fresh.clone());
            }
            Some(fresh)
        } else {
            None
        };

        Ok((revoked_copy, replacement))
    }

    /// Permanently deletes a single revoked invite link.
    pub fn delete_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<(), String> {
        let list = self
            .invites
            .get_mut(&peer_id)
            .ok_or_else(|| "No invite links found for this chat.".to_owned())?;
        let before = list.len();
        list.retain(|l| l.link != link);
        if list.len() == before {
            return Err("Invite link not found.".to_owned());
        }
        self.importers.remove(&(peer_id, link.to_owned()));
        Ok(())
    }

    /// Permanently deletes all revoked invite links for `peer_id`.
    pub fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) {
        if let Some(list) = self.invites.get_mut(&peer_id) {
            list.retain(|l| !l.revoked);
        }
    }

    /// Lists joined importers for a specific link or pending join requests (`requested = true`).
    #[must_use]
    pub fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> InviteImportersList {
        let q = query.trim().to_ascii_lowercase();
        let source: Vec<InviteImporterInfo> = if requested && link.is_none() {
            self.join_requests
                .get(&peer_id)
                .cloned()
                .unwrap_or_default()
        } else if let Some(url) = link {
            self.importers
                .get(&(peer_id, url.to_owned()))
                .map(|list| {
                    list.iter()
                        .filter(|i| i.requested == requested)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let filtered: Vec<InviteImporterInfo> = source
            .into_iter()
            .filter(|entry| {
                q.is_empty()
                    || entry.name.to_ascii_lowercase().contains(&q)
                    || entry
                        .username
                        .as_deref()
                        .is_some_and(|u| u.to_ascii_lowercase().contains(&q))
            })
            .collect();
        InviteImportersList {
            count: filtered.len() as i32,
            importers: filtered,
        }
    }

    /// Approves or dismisses a single join request.
    pub fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<InviteImporterInfo, String> {
        let list = self
            .join_requests
            .get_mut(&peer_id)
            .ok_or_else(|| "No pending join requests for this chat.".to_owned())?;
        let idx = list
            .iter()
            .position(|r| r.user_id == user_id)
            .ok_or_else(|| "Join request not found.".to_owned())?;
        let mut entry = list.remove(idx);
        entry.requested = false;

        // Also mirror removal/promotion into any per-link importer lists.
        for ((pid, _), link_importers) in &mut self.importers {
            if *pid == peer_id {
                if approved {
                    for imp in link_importers.iter_mut() {
                        if imp.user_id == user_id && imp.requested {
                            imp.requested = false;
                        }
                    }
                } else {
                    link_importers.retain(|imp| !(imp.user_id == user_id && imp.requested));
                }
            }
        }
        if let Some(links) = self.invites.get_mut(&peer_id) {
            for link in links.iter_mut() {
                if link.requested > 0 {
                    link.requested -= 1;
                    if approved {
                        link.usage += 1;
                    }
                    break;
                }
            }
        }
        Ok(entry)
    }

    /// Approves or dismisses all pending join requests for `peer_id` (optionally scoped to `link`).
    pub fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Vec<InviteImporterInfo> {
        let removed: Vec<InviteImporterInfo> = if let Some(url) = link {
            let link_uids: Vec<PeerId> = self
                .importers
                .get(&(peer_id, url.to_owned()))
                .map(|list| {
                    list.iter()
                        .filter(|i| i.requested)
                        .map(|i| i.user_id)
                        .collect()
                })
                .unwrap_or_default();
            let mut out = Vec::new();
            for uid in link_uids {
                if let Ok(item) = self.hide_chat_join_request(peer_id, uid, approved) {
                    out.push(item);
                }
            }
            out
        } else {
            let drained = self.join_requests.remove(&peer_id).unwrap_or_default();
            for ((pid, _), link_importers) in &mut self.importers {
                if *pid == peer_id {
                    if approved {
                        for imp in link_importers.iter_mut() {
                            imp.requested = false;
                        }
                    } else {
                        link_importers.retain(|imp| !imp.requested);
                    }
                }
            }
            if let Some(links) = self.invites.get_mut(&peer_id) {
                for l in links.iter_mut() {
                    if l.requested > 0 {
                        if approved {
                            l.usage += l.requested;
                        }
                        l.requested = 0;
                    }
                }
            }
            drained
        };
        removed
    }

    /// Builds the compact top-bar join-requests summary for `peer_id`.
    #[must_use]
    pub fn join_requests_summary(&self, peer_id: PeerId) -> JoinRequestsSummary {
        let Some(list) = self.join_requests.get(&peer_id) else {
            return JoinRequestsSummary::default();
        };
        let recent_requesters = list
            .iter()
            .take(3)
            .map(|r| {
                let short = r
                    .name
                    .split_whitespace()
                    .next()
                    .unwrap_or(&r.name)
                    .to_owned();
                (r.user_id, short, r.color_index)
            })
            .collect();
        JoinRequestsSummary {
            count: list.len() as u32,
            recent_requesters,
        }
    }

    /// Reads peer accent/profile color settings.
    #[must_use]
    pub fn peer_color_settings(&self, peer_id: PeerId, fallback_color: u8) -> PeerColorSettings {
        self.colors
            .get(&peer_id)
            .cloned()
            .unwrap_or(PeerColorSettings {
                color_index: Some(fallback_color),
                background_emoji_id: 0,
                profile_color_index: None,
                profile_background_emoji_id: 0,
            })
    }

    /// Updates peer accent or profile color settings.
    pub fn update_peer_color(&mut self, peer_id: PeerId, req: UpdatePeerColorRequest) {
        let entry = self.colors.entry(peer_id).or_default();
        if req.for_profile {
            entry.profile_color_index = req.color_index;
            entry.profile_background_emoji_id = req.background_emoji_id;
        } else {
            entry.color_index = req.color_index;
            entry.background_emoji_id = req.background_emoji_id;
        }
    }

    /// Runs mock pre-checks for ownership transfer.
    #[must_use]
    pub fn check_ownership_transfer(&self) -> OwnershipTransferCheck {
        if let Some(ref err) = self.forced_transfer_error {
            return OwnershipTransferCheck::Blocked {
                reason: err.clone(),
                message: err.display_message(),
            };
        }
        OwnershipTransferCheck::Ready {
            password_hint: self.transfer_password_hint.clone(),
        }
    }

    /// Validates the mock 2FA password and records an ownership transfer.
    pub fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), OwnershipTransferError> {
        if let Some(ref err) = self.forced_transfer_error {
            return Err(err.clone());
        }
        if password != self.transfer_password {
            return Err(OwnershipTransferError::WrongPassword);
        }
        self.completed_transfers.push((peer_id, new_owner_id));
        Ok(())
    }

    /// Returns recommended similar channels for `peer_id`.
    #[must_use]
    pub fn similar_channels(&self, peer_id: PeerId) -> SimilarChannelsResult {
        self.similar_channels
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Lists Downloads Manager entries matching `query`.
    #[must_use]
    pub fn downloads_list(&self, query: &str) -> Vec<DownloadEntry> {
        let q = query.trim().to_ascii_lowercase();
        self.downloads
            .iter()
            .filter(|d| d.matches_query(&q))
            .cloned()
            .collect()
    }

    /// Toggles pause/resume on a download entry.
    pub fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        let entry = self
            .downloads
            .iter_mut()
            .find(|d| d.id == download_id)
            .ok_or_else(|| "Download not found.".to_owned())?;
        entry.status = match entry.status {
            DownloadStatus::Downloading {
                received_bytes,
                total_bytes,
            } => DownloadStatus::Paused {
                received_bytes,
                total_bytes,
            },
            DownloadStatus::Paused {
                received_bytes,
                total_bytes,
            } => DownloadStatus::Downloading {
                received_bytes,
                total_bytes,
            },
            DownloadStatus::Completed => {
                return Err("Download is already completed.".to_owned());
            }
            DownloadStatus::Failed { .. } => DownloadStatus::Downloading {
                received_bytes: 0,
                total_bytes: entry.total_bytes,
            },
        };
        Ok(())
    }

    /// Cancels or deletes a download entry.
    pub fn cancel_or_delete_download(&mut self, download_id: u64) -> Result<(), String> {
        let before = self.downloads.len();
        self.downloads.retain(|d| d.id != download_id);
        if self.downloads.len() == before {
            return Err("Download not found.".to_owned());
        }
        Ok(())
    }

    /// Removes completed entries from the Downloads list.
    pub fn clear_completed_downloads(&mut self) -> usize {
        let before = self.downloads.len();
        self.downloads
            .retain(|d| !matches!(d.status, DownloadStatus::Completed));
        before - self.downloads.len()
    }

    /// Removes all entries from the Downloads list.
    pub fn delete_all_downloads(&mut self) -> usize {
        let count = self.downloads.len();
        self.downloads.clear();
        count
    }

    // -------------------------------------------------------------------------
    // WP-65 & WP-66 mock state helpers
    // -------------------------------------------------------------------------

    /// Returns cached channel/supergroup management flags for `peer_id`.
    #[must_use]
    pub fn channel_management_state(&self, peer_id: PeerId) -> Option<ChannelManagementState> {
        self.channel_mgmt.get(&peer_id).cloned()
    }

    /// Toggles `pre_history_hidden` for `peer_id`.
    pub fn toggle_pre_history_hidden(&mut self, peer_id: PeerId, hidden: bool) {
        self.channel_mgmt
            .entry(peer_id)
            .or_default()
            .pre_history_hidden = hidden;
    }

    /// Upgrades `peer_id` to a broadcast gigagroup.
    pub fn convert_to_gigagroup(&mut self, peer_id: PeerId) {
        self.channel_mgmt.entry(peer_id).or_default().is_gigagroup = true;
        self.gigagroup_conversions.push(peer_id);
    }

    /// Records deactivating all collectible usernames on `peer_id`.
    pub fn deactivate_all_channel_usernames(&mut self, peer_id: PeerId) {
        self.deactivated_usernames_peers.push(peer_id);
    }

    /// Toggles forum mode and tabbed layout for `peer_id`.
    pub fn toggle_channel_forum(&mut self, peer_id: PeerId, enabled: bool, tabs: bool) {
        let st = self.channel_mgmt.entry(peer_id).or_default();
        st.is_forum = enabled;
        st.forum_tabs = enabled && tabs;
        if enabled {
            st.pre_history_hidden = false;
        }
    }

    /// Toggles hidden participants for `peer_id`.
    pub fn toggle_participants_hidden(&mut self, peer_id: PeerId, hidden: bool) {
        self.channel_mgmt
            .entry(peer_id)
            .or_default()
            .participants_hidden = hidden;
    }

    /// Toggles viewing a forum as flat messages.
    pub fn toggle_view_forum_as_messages(&mut self, peer_id: PeerId, as_messages: bool) {
        self.channel_mgmt
            .entry(peer_id)
            .or_default()
            .view_forum_as_messages = as_messages;
    }

    /// Sets boosts required to unblock default group restrictions.
    pub fn set_boosts_to_unblock_restrictions(&mut self, peer_id: PeerId, boosts: u32) {
        self.channel_mgmt
            .entry(peer_id)
            .or_default()
            .boosts_unrestrict = boosts;
    }

    /// Assigns or clears the group's official sticker pack.
    pub fn set_channel_sticker_set(&mut self, peer_id: PeerId, set: ChannelStickerSetRef) {
        let st = self.channel_mgmt.entry(peer_id).or_default();
        st.sticker_set = match set {
            ChannelStickerSetRef::Empty => None,
            other => Some(other),
        };
    }

    /// Assigns or clears the group's official custom emoji pack.
    pub fn set_channel_emoji_sticker_set(&mut self, peer_id: PeerId, set: ChannelStickerSetRef) {
        let st = self.channel_mgmt.entry(peer_id).or_default();
        st.emoji_set = match set {
            ChannelStickerSetRef::Empty => None,
            other => Some(other),
        };
    }

    /// Updates paid messages price and direct-messages flag.
    pub fn update_channel_paid_messages_price(
        &mut self,
        peer_id: PeerId,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
    ) {
        let st = self.channel_mgmt.entry(peer_id).or_default();
        st.broadcast_messages_allowed = broadcast_messages_allowed;
        st.stars_per_message = stars_per_message.max(0);
    }

    /// Toggles channel auto-translation.
    pub fn toggle_channel_autotranslation(&mut self, peer_id: PeerId, enabled: bool) {
        self.channel_mgmt
            .entry(peer_id)
            .or_default()
            .autotranslation = enabled;
    }

    /// Records deleting a basic chat via `messages.deleteChat`.
    pub fn delete_basic_chat(&mut self, peer_id: PeerId) {
        self.deleted_basic_chats.push(peer_id);
    }

    /// Returns admins with invite links for `peer_id`.
    #[must_use]
    pub fn admins_with_invites(&self, peer_id: PeerId) -> Vec<AdminWithInvitesEntry> {
        self.admins_with_invites
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Records deleting contacts via `contacts.deleteContacts`.
    pub fn record_deleted_contacts(&mut self, peer_ids: &[PeerId]) {
        self.deleted_contacts.extend_from_slice(peer_ids);
    }

    /// Exports a deterministic temporary contact QR token.
    #[must_use]
    pub fn export_contact_token(&self) -> ExportedContactTokenInfo {
        let seq = self.contact_token_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(MOCK_NOW_UNIX, |d| d.as_secs().min(i32::MAX as u64) as i32);
        ExportedContactTokenInfo {
            url: format!("https://t.me/contact/demo_token_{seq:04}"),
            expires: now.saturating_add(900),
        }
    }

    /// Resolves a contact token (`contacts.importContactToken`).
    pub fn import_contact_token(
        &mut self,
        token: &str,
        fallback_peer: PeerId,
    ) -> Result<PeerId, String> {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("expired") {
            return Err("CONTACT_TOKEN_EXPIRED".to_owned());
        }
        if trimmed.eq_ignore_ascii_case("flood") {
            return Err("FLOOD_WAIT_30".to_owned());
        }
        self.imported_contact_tokens.push(trimmed.to_owned());
        Ok(fallback_peer)
    }

    /// Exports a deterministic paid-message fee-waiver token.
    #[must_use]
    pub fn export_no_paid_messages_token(
        &self,
        revoke: bool,
        temporary: bool,
    ) -> ExportedNoPaidMessagesTokenInfo {
        let tag = if revoke { "revoked" } else { "active" };
        let token = format!("free_{tag}_{}", if temporary { "tmp" } else { "perm" });
        ExportedNoPaidMessagesTokenInfo {
            url: format!("https://t.me/contact/demo_token_0001?free={token}"),
            token,
            expires: temporary.then_some(MOCK_NOW_UNIX + 3_600),
        }
    }

    /// Redeems a paid-message fee-waiver token for `peer_id`.
    pub fn import_no_paid_messages_token(
        &mut self,
        peer_id: PeerId,
        token: &str,
    ) -> Result<NoPaidMessagesTokenImportResult, String> {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("invalid") {
            return Err("TOKEN_INVALID".to_owned());
        }
        self.imported_no_paid_tokens
            .push((peer_id, trimmed.to_owned()));
        Ok(NoPaidMessagesTokenImportResult {
            user_id: peer_id,
            price_removed: true,
        })
    }

    /// Returns the private contact note for `peer_id`.
    #[must_use]
    pub fn contact_note(&self, peer_id: PeerId) -> Option<tg_text::TextWithEntities> {
        self.contact_notes.get(&peer_id).cloned()
    }

    /// Sets or clears the private contact note for `peer_id`.
    pub fn update_contact_note(
        &mut self,
        peer_id: PeerId,
        note: tg_text::TextWithEntities,
    ) -> bool {
        if note.text.trim().is_empty() {
            self.contact_notes.remove(&peer_id);
        } else {
            self.contact_notes.insert(peer_id, note);
        }
        true
    }

    /// Records deleting own profile photos (`photos.deletePhotos`).
    pub fn delete_profile_photos(&mut self, photos: &[ProfilePhotoRef]) -> Vec<i64> {
        let ids: Vec<i64> = photos.iter().map(|p| p.id).collect();
        self.deleted_profile_photos.extend_from_slice(&ids);
        ids
    }

    /// Records uploading, suggesting, or clearing a contact profile photo.
    pub fn upload_contact_profile_photo(
        &mut self,
        peer_id: PeerId,
        request: ContactProfilePhotoUpload,
    ) -> i64 {
        let is_clear =
            request.image.is_none() && request.video.is_none() && request.emoji_markup.is_none();
        self.uploaded_contact_photos.push((peer_id, request));
        if is_clear {
            0
        } else {
            700_000 + self.uploaded_contact_photos.len() as i64
        }
    }
}
