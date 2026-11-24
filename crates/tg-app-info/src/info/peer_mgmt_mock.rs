//! Deterministic in-memory mock store for WP-10 peer management features:
//! invite links, join requests, peer colors, ownership transfer, similar channels,
//! and the Downloads Manager.
//!
//! Shared by both [`crate::info::mock::MockBackend`] and
//! [`tg_app_core::stand_in::mock_session::MockSession`] so demo mode and tests have
//! consistent, realistic synthetic data (strictly fictional names and IDs).

use std::collections::HashMap;

use crate::info::backend::{
    DownloadEntry, DownloadStatus, InviteImporterInfo, InviteImportersList, InviteLinkInfo,
    InviteLinkRequest, InviteLinksList, JoinRequestsSummary, OwnershipTransferCheck,
    OwnershipTransferError, PeerColorSettings, SimilarChannelEntry, SimilarChannelsResult,
    UpdatePeerColorRequest,
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
}
