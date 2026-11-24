//! Deterministic in-memory [`InfoBackend`] implementation with synthetic sample data.
//!
//! All names, phone numbers (`+999 ...`), handles, and media metadata are strictly
//! fictional and contain no real people's names or real Telegram accounts.

use std::collections::HashMap;

use tg_data::rights::{ChatAdminRights, ChatBannedRights};

use crate::info::backend::{
    CommonGroupEntry, CreateChannelRequest, CreateGroupRequest, DownloadEntry, EmojiUserpicRequest,
    InfoBackend, InfoPeerKind, InviteImporterInfo, InviteImportersList, InviteLinkInfo,
    InviteLinkRequest, InviteLinksList, JoinRequestsSummary, LanguageEntry, MediaItem, MemberEntry,
    MemberRole, OwnershipTransferCheck, OwnershipTransferError, PeerColorSettings, PeerProfile,
    PeerReactionsConfig, PeerUsername, PeerVerificationStatus, PersonalChannelOption,
    ProfileBalanceInfo, ProfileMainTab, ProfilePeerGiftsPage, ProfilePhotoRef, ProfileStoriesPage,
    ProfileStoryAlbum, ReportReason, SavedMusicTrack, SharedMediaCounts, SharedMediaTab,
    SimilarChannelsResult, TermsOfServiceData, UpdatePeerColorRequest, UpdatePeerInfoRequest,
};
use crate::info::communities_mock::CommunitiesMockStore;
use crate::info::peer_mgmt_mock::PeerManagementMockStore;
use crate::info::profile_extras_mock::ProfileExtrasMockStore;
use crate::info::values::{
    Birthday, MAX_ADMIN_TITLE_LENGTH, MAX_CHANNEL_DESCRIPTION, MAX_GROUP_CHANNEL_TITLE,
    MAX_USER_FIRST_LAST_NAME, UsernameAvailability, format_members_status, format_phone,
    format_subscribers_status, is_valid_phone, normalize_username, validate_username_syntax,
};
use tg_app_core::stand_in::types::PeerId;

/// Well-known synthetic peer IDs shipped by [`MockBackend::new`].
pub mod mock_peers {
    use tg_app_core::stand_in::types::PeerId;

    /// Current account / Saved Messages (`PeerId::from_user(1000)`).
    pub const SAVED_MESSAGES: PeerId = PeerId::from_user(1000);
    /// Primary contact user ("Mira Vance").
    pub const USER_MIRA: PeerId = PeerId::from_user(1001);
    /// Secondary contact user ("Kaelen Orlov").
    pub const USER_KAELEN: PeerId = PeerId::from_user(1002);
    /// Contact user ("Tessa Lind").
    pub const USER_TESSA: PeerId = PeerId::from_user(1003);
    /// Contact user ("Nolan Brooks").
    pub const USER_NOLAN: PeerId = PeerId::from_user(1004);
    /// Contact user ("Sora Takahashi").
    pub const USER_SORA: PeerId = PeerId::from_user(1005);
    /// Contact user ("Darian Holt").
    pub const USER_DARIAN: PeerId = PeerId::from_user(1006);
    /// Contact user ("Priya Nair").
    pub const USER_PRIYA: PeerId = PeerId::from_user(1007);
    /// Contact user ("Rowan Hayes").
    pub const USER_ROWAN: PeerId = PeerId::from_user(1008);
    /// Automated helper bot ("Build Helper Bot").
    pub const BOT_BUILD: PeerId = PeerId::from_user(1099);
    /// Active engineering supergroup ("Rustgram Core Port").
    pub const GROUP_CORE: PeerId = PeerId::from_chat(2001);
    /// Design systems group ("UI & Theme Lab").
    pub const GROUP_DESIGN: PeerId = PeerId::from_chat(2002);
    /// Broadcast channel ("Rustgram Release Notes").
    pub const CHANNEL_RELEASES: PeerId = PeerId::from_channel(3001);
    /// Broadcast channel ("Rust GUI Digest").
    pub const CHANNEL_DIGEST: PeerId = PeerId::from_channel(3002);
}

/// Recorded report submission in [`MockBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedReport {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Selected abuse reason.
    pub reason: ReportReason,
    /// Optional user comment.
    pub comment: String,
}

/// Recorded forward operation in [`MockBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRecord {
    /// Target chat IDs.
    pub targets: Vec<PeerId>,
    /// Forwarded message IDs.
    pub message_ids: Vec<i32>,
    /// Optional accompanying comment.
    pub comment: String,
}

/// Full in-memory mock implementation of [`InfoBackend`].
#[derive(Debug, Clone)]
pub struct MockBackend {
    peers: Vec<PeerProfile>,
    members: HashMap<PeerId, Vec<MemberEntry>>,
    common_groups: HashMap<PeerId, Vec<CommonGroupEntry>>,
    media: HashMap<(PeerId, SharedMediaTab), Vec<MediaItem>>,
    languages: Vec<LanguageEntry>,
    active_language: String,
    tos: TermsOfServiceData,
    next_user_bare: u64,
    next_chat_bare: u64,
    next_channel_bare: u64,
    /// Audit log of submitted abuse reports (for tests & verification).
    pub submitted_reports: Vec<SubmittedReport>,
    /// Audit log of message forwards (for tests & verification).
    pub forward_records: Vec<ForwardRecord>,
    /// Audit log of `(peer_id, deleted_count, revoke_for_everyone)` deletions.
    pub deleted_batches: Vec<(PeerId, usize, bool)>,
    /// Peer management mock state (invite links, join requests, peer colors, ownership, similar channels, downloads).
    pub peer_mgmt: PeerManagementMockStore,
    /// WP-26 profile extras mock state (stories/albums, gifts, saved music, emoji userpic, reactions, verification).
    pub profile_extras: ProfileExtrasMockStore,
    /// WP-27 communities, managed bots, moderation, welcome messages, and anti-spam mock store.
    pub communities: CommunitiesMockStore,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// Creates a deterministic mock backend pre-populated with users, groups,
    /// channels, a bot, Saved Messages, members, shared media, and language packs.
    #[must_use]
    pub fn new() -> Self {
        let mut peers = Vec::new();
        let mut members: HashMap<PeerId, Vec<MemberEntry>> = HashMap::new();
        let mut common_groups: HashMap<PeerId, Vec<CommonGroupEntry>> = HashMap::new();
        let mut media: HashMap<(PeerId, SharedMediaTab), Vec<MediaItem>> = HashMap::new();

        // 1. Saved Messages
        peers.push(PeerProfile {
            id: mock_peers::SAVED_MESSAGES,
            kind: InfoPeerKind::SavedMessages,
            title: "Saved Messages".to_owned(),
            status: "cloud storage".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 5,
            phone: Some("+999 661 0000".to_owned()),
            usernames: vec![PeerUsername {
                username: "rustgram_tester".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Personal cloud notes, drafts, and bookmarked links.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 4,
                files: 2,
                links: 3,
                voice: 1,
                music: 2,
                gifs: 2,
                common_groups: 0,
            },
        });

        // 2. Primary User ("Mira Vance") with phone, multiple usernames (1 collectible), bio, birthday, common groups
        peers.push(PeerProfile {
            id: mock_peers::USER_MIRA,
            kind: InfoPeerKind::User,
            title: "Mira Vance".to_owned(),
            status: "online".to_owned(),
            is_online: true,
            verified: false,
            premium: true,
            color_index: 0,
            phone: Some("+999 661 0142".to_owned()),
            usernames: vec![
                PeerUsername {
                    username: "mira_vance".to_owned(),
                    is_editable: true,
                    is_active: true,
                },
                PeerUsername {
                    username: "mv_graphics".to_owned(),
                    is_editable: false,
                    is_active: true,
                },
            ],
            bio: "Systems & graphics engineer working on pure-Rust UI toolkits, custom shaders, and accessible desktop apps.".to_owned(),
            birthday: Some(Birthday {
                day: 14,
                month: 5,
                year: Some(1994),
            }),
            invite_link: None,
            muted: false,
            is_contact: true,
            is_blocked: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 86_400 * 7,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 6,
                files: 4,
                links: 4,
                voice: 3,
                music: 3,
                gifs: 4,
                common_groups: 2,
            },
        });

        // 3. Additional fictional contacts
        let extra_contacts = [
            (
                mock_peers::USER_KAELEN,
                "Kaelen Orlov",
                "kaelen_orlov",
                "+999 661 0218",
                "online",
                true,
                "Protocol & cryptography maintainer. Reviewing MTProto framing.",
                1,
            ),
            (
                mock_peers::USER_TESSA,
                "Tessa Lind",
                "tessa_lind",
                "+999 661 0305",
                "last seen 12 minutes ago",
                false,
                "Interface designer focusing on typography and adaptive layouts.",
                2,
            ),
            (
                mock_peers::USER_NOLAN,
                "Nolan Brooks",
                "nolan_brooks",
                "+999 661 0411",
                "online",
                true,
                "Audio & video pipeline engineer.",
                3,
            ),
            (
                mock_peers::USER_SORA,
                "Sora Takahashi",
                "sora_takahashi",
                "+999 661 0520",
                "last seen recently",
                false,
                "Localization & RTL layout specialist.",
                4,
            ),
            (
                mock_peers::USER_DARIAN,
                "Darian Holt",
                "darian_holt",
                "+999 661 0633",
                "online",
                true,
                "Storage formats, SQLite caches, and migration tools.",
                5,
            ),
            (
                mock_peers::USER_PRIYA,
                "Priya Nair",
                "priya_nair",
                "+999 661 0749",
                "last seen 2 hours ago",
                false,
                "QA automation and cross-platform packaging.",
                6,
            ),
            (
                mock_peers::USER_ROWAN,
                "Rowan Hayes",
                "rowan_hayes",
                "+999 661 0881",
                "last seen within a week",
                false,
                "Bot platform & keyboard markup contributor.",
                0,
            ),
        ];

        for (id, name, uname, phone, status, is_online, bio, color_index) in extra_contacts {
            peers.push(PeerProfile {
                id,
                kind: InfoPeerKind::User,
                title: name.to_owned(),
                status: status.to_owned(),
                is_online,
                verified: false,
                premium: false,
                color_index,
                phone: Some(phone.to_owned()),
                usernames: vec![PeerUsername {
                    username: uname.to_owned(),
                    is_editable: true,
                    is_active: true,
                }],
                bio: bio.to_owned(),
                birthday: None,
                invite_link: None,
                muted: false,
                is_contact: true,
                is_blocked: false,
                can_edit: false,
                can_add_members: false,
                can_manage_admins: false,
                can_manage_permissions: false,
                is_public: false,
                no_forwards: false,
                is_forum: false,
                members_count: 0,
                online_count: 0,
                admins_count: 0,
                default_banned_rights: ChatBannedRights::empty(),
                slowmode_seconds: 0,
                auto_delete_ttl_seconds: 0,
                linked_discussion_peer: None,
                bot_commands_count: 0,
                bot_reads_all_messages: false,
                has_mini_app: false,
                media_counts: SharedMediaCounts {
                    photos_videos: 2,
                    files: 1,
                    links: 1,
                    voice: 1,
                    music: 0,
                    gifs: 0,
                    common_groups: 1,
                },
            });
        }

        // 4. Bot peer ("Build Helper Bot")
        peers.push(PeerProfile {
            id: mock_peers::BOT_BUILD,
            kind: InfoPeerKind::Bot,
            title: "Build Helper Bot".to_owned(),
            status: "bot".to_owned(),
            is_online: false,
            verified: true,
            premium: false,
            color_index: 5,
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_build_bot".to_owned(),
                is_editable: false,
                is_active: true,
            }],
            bio: "Reports Cargo build status, clippy checks, and nightly artifact checksums for the Rustgram workspace.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 6,
            bot_reads_all_messages: false,
            has_mini_app: true,
            media_counts: SharedMediaCounts {
                photos_videos: 2,
                files: 3,
                links: 2,
                voice: 0,
                music: 0,
                gifs: 0,
                common_groups: 1,
            },
        });

        // 5. Supergroup ("Rustgram Core Port")
        let core_members = vec![
            MemberEntry {
                peer_id: mock_peers::USER_MIRA,
                name: "Mira Vance".to_owned(),
                username: Some("mira_vance".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Owner {
                    custom_title: Some("owner".to_owned()),
                },
                color_index: 0,
            },
            MemberEntry {
                peer_id: mock_peers::USER_KAELEN,
                name: "Kaelen Orlov".to_owned(),
                username: Some("kaelen_orlov".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Admin {
                    custom_title: Some("protocol".to_owned()),
                    rights: ChatAdminRights::CHANGE_INFO
                        | ChatAdminRights::DELETE_MESSAGES
                        | ChatAdminRights::BAN_USERS
                        | ChatAdminRights::PIN_MESSAGES
                        | ChatAdminRights::INVITE_USERS,
                },
                color_index: 1,
            },
            MemberEntry {
                peer_id: mock_peers::USER_TESSA,
                name: "Tessa Lind".to_owned(),
                username: Some("tessa_lind".to_owned()),
                status_text: "last seen 12 minutes ago".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Admin {
                    custom_title: Some("design".to_owned()),
                    rights: ChatAdminRights::CHANGE_INFO
                        | ChatAdminRights::PIN_MESSAGES
                        | ChatAdminRights::INVITE_USERS,
                },
                color_index: 2,
            },
            MemberEntry {
                peer_id: mock_peers::USER_NOLAN,
                name: "Nolan Brooks".to_owned(),
                username: Some("nolan_brooks".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 3,
            },
            MemberEntry {
                peer_id: mock_peers::USER_DARIAN,
                name: "Darian Holt".to_owned(),
                username: Some("darian_holt".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 5,
            },
            MemberEntry {
                peer_id: mock_peers::USER_SORA,
                name: "Sora Takahashi".to_owned(),
                username: Some("sora_takahashi".to_owned()),
                status_text: "last seen recently".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 4,
            },
            MemberEntry {
                peer_id: mock_peers::USER_PRIYA,
                name: "Priya Nair".to_owned(),
                username: Some("priya_nair".to_owned()),
                status_text: "last seen 2 hours ago".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 6,
            },
            MemberEntry {
                peer_id: mock_peers::BOT_BUILD,
                name: "Build Helper Bot".to_owned(),
                username: Some("rustgram_build_bot".to_owned()),
                status_text: "bot".to_owned(),
                is_online: false,
                is_bot: true,
                role: MemberRole::Admin {
                    custom_title: Some("CI".to_owned()),
                    rights: ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::PIN_MESSAGES,
                },
                color_index: 5,
            },
            MemberEntry {
                peer_id: mock_peers::USER_ROWAN,
                name: "Rowan Hayes".to_owned(),
                username: Some("rowan_hayes".to_owned()),
                status_text: "last seen within a week".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Restricted {
                    rights: ChatBannedRights::SEND_POLLS | ChatBannedRights::PIN_MESSAGES,
                    until: 0,
                },
                color_index: 0,
            },
        ];
        let core_online = core_members.iter().filter(|m| m.is_online).count() as u32;
        let core_admins = core_members
            .iter()
            .filter(|m| m.role.is_admin_or_owner())
            .count() as u32;

        peers.push(PeerProfile {
            id: mock_peers::GROUP_CORE,
            kind: InfoPeerKind::Supergroup,
            title: "Rustgram Core Port".to_owned(),
            status: format_members_status(core_members.len() as u32, core_online),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 0,
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_core".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Coordination supergroup for porting Telegram Desktop crates to Rust. Share benchmarks, UI screenshots, and protocol test vectors.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/+RustgramCoreInvite99".to_owned()),
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: true,
            no_forwards: false,
            is_forum: true,
            members_count: core_members.len() as u32,
            online_count: core_online,
            admins_count: core_admins,
            default_banned_rights: ChatBannedRights::CHANGE_INFO | ChatBannedRights::PIN_MESSAGES,
            slowmode_seconds: 10,
            auto_delete_ttl_seconds: 86_400 * 31,
            linked_discussion_peer: Some((
                mock_peers::CHANNEL_RELEASES,
                "Rustgram Release Notes".to_owned(),
            )),
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 6,
                files: 4,
                links: 4,
                voice: 3,
                music: 2,
                gifs: 4,
                common_groups: 0,
            },
        });
        members.insert(mock_peers::GROUP_CORE, core_members);

        // 6. Secondary Group ("UI & Theme Lab")
        let design_members = vec![
            MemberEntry {
                peer_id: mock_peers::USER_TESSA,
                name: "Tessa Lind".to_owned(),
                username: Some("tessa_lind".to_owned()),
                status_text: "last seen 12 minutes ago".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Owner { custom_title: None },
                color_index: 2,
            },
            MemberEntry {
                peer_id: mock_peers::USER_MIRA,
                name: "Mira Vance".to_owned(),
                username: Some("mira_vance".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Admin {
                    custom_title: Some("egui".to_owned()),
                    rights: ChatAdminRights::CHANGE_INFO | ChatAdminRights::PIN_MESSAGES,
                },
                color_index: 0,
            },
            MemberEntry {
                peer_id: mock_peers::USER_SORA,
                name: "Sora Takahashi".to_owned(),
                username: Some("sora_takahashi".to_owned()),
                status_text: "last seen recently".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 4,
            },
        ];
        peers.push(PeerProfile {
            id: mock_peers::GROUP_DESIGN,
            kind: InfoPeerKind::Group,
            title: "UI & Theme Lab".to_owned(),
            status: format_members_status(design_members.len() as u32, 1),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 2,
            phone: None,
            usernames: Vec::new(),
            bio: "Palette tuning, animation curves, and widget polish.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/+UiThemeLabPrivate01".to_owned()),
            muted: true,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: design_members.len() as u32,
            online_count: 1,
            admins_count: 2,
            default_banned_rights: ChatBannedRights::CHANGE_INFO,
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 4,
                files: 2,
                links: 2,
                voice: 0,
                music: 0,
                gifs: 2,
                common_groups: 0,
            },
        });
        members.insert(mock_peers::GROUP_DESIGN, design_members);

        // 7. Broadcast Channel ("Rustgram Release Notes")
        peers.push(PeerProfile {
            id: mock_peers::CHANNEL_RELEASES,
            kind: InfoPeerKind::Channel,
            title: "Rustgram Release Notes".to_owned(),
            status: format_subscribers_status(4_280),
            is_online: false,
            verified: true,
            premium: false,
            color_index: 3,
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_releases".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Official changelogs, release builds, and milestone demos for the open-source Rust desktop client.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/+RustgramReleasesPriv".to_owned()),
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: true,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 4_280,
            online_count: 0,
            admins_count: 3,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: Some((mock_peers::GROUP_CORE, "Rustgram Core Port".to_owned())),
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 6,
                files: 3,
                links: 3,
                voice: 1,
                music: 1,
                gifs: 2,
                common_groups: 0,
            },
        });
        members.insert(
            mock_peers::CHANNEL_RELEASES,
            vec![
                MemberEntry {
                    peer_id: mock_peers::USER_MIRA,
                    name: "Mira Vance".to_owned(),
                    username: Some("mira_vance".to_owned()),
                    status_text: "online".to_owned(),
                    is_online: true,
                    is_bot: false,
                    role: MemberRole::Owner { custom_title: None },
                    color_index: 0,
                },
                MemberEntry {
                    peer_id: mock_peers::USER_KAELEN,
                    name: "Kaelen Orlov".to_owned(),
                    username: Some("kaelen_orlov".to_owned()),
                    status_text: "online".to_owned(),
                    is_online: true,
                    is_bot: false,
                    role: MemberRole::Admin {
                        custom_title: Some("editor".to_owned()),
                        rights: ChatAdminRights::POST_MESSAGES
                            | ChatAdminRights::EDIT_MESSAGES
                            | ChatAdminRights::DELETE_MESSAGES,
                    },
                    color_index: 1,
                },
            ],
        );

        // 8. Secondary Broadcast Channel ("Rust GUI Digest")
        peers.push(PeerProfile {
            id: mock_peers::CHANNEL_DIGEST,
            kind: InfoPeerKind::Channel,
            title: "Rust GUI Digest".to_owned(),
            status: format_subscribers_status(1_840),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 4,
            phone: None,
            usernames: vec![PeerUsername {
                username: "rust_gui_digest".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Weekly links on egui, wgpu, typography, and desktop engineering.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: true,
            is_contact: false,
            is_blocked: false,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 1_840,
            online_count: 0,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts {
                photos_videos: 3,
                files: 1,
                links: 4,
                voice: 0,
                music: 0,
                gifs: 0,
                common_groups: 0,
            },
        });

        // Common groups
        common_groups.insert(
            mock_peers::USER_MIRA,
            vec![
                CommonGroupEntry {
                    peer_id: mock_peers::GROUP_CORE,
                    title: "Rustgram Core Port".to_owned(),
                    members_count: 9,
                    color_index: 0,
                },
                CommonGroupEntry {
                    peer_id: mock_peers::GROUP_DESIGN,
                    title: "UI & Theme Lab".to_owned(),
                    members_count: 3,
                    color_index: 2,
                },
            ],
        );
        common_groups.insert(
            mock_peers::USER_KAELEN,
            vec![CommonGroupEntry {
                peer_id: mock_peers::GROUP_CORE,
                title: "Rustgram Core Port".to_owned(),
                members_count: 9,
                color_index: 0,
            }],
        );
        common_groups.insert(
            mock_peers::BOT_BUILD,
            vec![CommonGroupEntry {
                peer_id: mock_peers::GROUP_CORE,
                title: "Rustgram Core Port".to_owned(),
                members_count: 9,
                color_index: 0,
            }],
        );

        // Shared media samples populated for main showcase peers
        let sample_media_peers = [
            mock_peers::USER_MIRA,
            mock_peers::GROUP_CORE,
            mock_peers::CHANNEL_RELEASES,
            mock_peers::SAVED_MESSAGES,
            mock_peers::BOT_BUILD,
        ];
        for peer_id in sample_media_peers {
            populate_sample_media(&mut media, peer_id);
        }

        let languages = vec![
            LanguageEntry {
                id: "en".to_owned(),
                name: "English".to_owned(),
                native_name: "English".to_owned(),
                official: true,
                progress_percent: 100,
            },
            LanguageEntry {
                id: "de".to_owned(),
                name: "German".to_owned(),
                native_name: "Deutsch".to_owned(),
                official: true,
                progress_percent: 100,
            },
            LanguageEntry {
                id: "es".to_owned(),
                name: "Spanish".to_owned(),
                native_name: "Español".to_owned(),
                official: true,
                progress_percent: 100,
            },
            LanguageEntry {
                id: "fr".to_owned(),
                name: "French".to_owned(),
                native_name: "Français".to_owned(),
                official: true,
                progress_percent: 99,
            },
            LanguageEntry {
                id: "it".to_owned(),
                name: "Italian".to_owned(),
                native_name: "Italiano".to_owned(),
                official: true,
                progress_percent: 98,
            },
            LanguageEntry {
                id: "pt-br".to_owned(),
                name: "Portuguese (Brazil)".to_owned(),
                native_name: "Português (Brasil)".to_owned(),
                official: true,
                progress_percent: 99,
            },
            LanguageEntry {
                id: "uk".to_owned(),
                name: "Ukrainian".to_owned(),
                native_name: "Українська".to_owned(),
                official: true,
                progress_percent: 100,
            },
            LanguageEntry {
                id: "pl".to_owned(),
                name: "Polish".to_owned(),
                native_name: "Polski".to_owned(),
                official: true,
                progress_percent: 97,
            },
            LanguageEntry {
                id: "ja".to_owned(),
                name: "Japanese".to_owned(),
                native_name: "日本語".to_owned(),
                official: true,
                progress_percent: 95,
            },
            LanguageEntry {
                id: "ko".to_owned(),
                name: "Korean".to_owned(),
                native_name: "한국어".to_owned(),
                official: true,
                progress_percent: 96,
            },
            LanguageEntry {
                id: "nl".to_owned(),
                name: "Dutch".to_owned(),
                native_name: "Nederlands".to_owned(),
                official: true,
                progress_percent: 94,
            },
            LanguageEntry {
                id: "tr".to_owned(),
                name: "Turkish".to_owned(),
                native_name: "Türkçe".to_owned(),
                official: true,
                progress_percent: 98,
            },
        ];

        let tos = TermsOfServiceData {
            id: "tos-2026-v1".to_owned(),
            title: "Terms of Service".to_owned(),
            body: concat!(
                "By signing up for and using this messaging service through Rustgram, you agree that you will not:\n\n",
                "• Use our service to send spam or scam users.\n",
                "• Promote violence on publicly viewable channels, bots, or groups.\n",
                "• Post illegal content on publicly viewable channels, bots, or groups.\n\n",
                "We reserve the right to update these Terms of Service later. ",
                "See https://telegram.org/tos for the full service terms and privacy policy."
            )
            .to_owned(),
            min_age_confirm: Some(16),
            accepted: false,
        };

        Self {
            peers,
            members,
            common_groups,
            media,
            languages,
            active_language: "en".to_owned(),
            tos,
            next_user_bare: 1100,
            next_chat_bare: 2100,
            next_channel_bare: 3100,
            submitted_reports: Vec::new(),
            forward_records: Vec::new(),
            deleted_batches: Vec::new(),
            peer_mgmt: PeerManagementMockStore::new_for_peers(
                mock_peers::SAVED_MESSAGES,
                mock_peers::GROUP_CORE,
                mock_peers::CHANNEL_RELEASES,
                mock_peers::CHANNEL_DIGEST,
            ),
            profile_extras: ProfileExtrasMockStore::new_for_peers(
                mock_peers::SAVED_MESSAGES,
                mock_peers::USER_MIRA,
                mock_peers::GROUP_CORE,
                mock_peers::CHANNEL_RELEASES,
                mock_peers::BOT_BUILD,
            ),
            communities: CommunitiesMockStore::new_for_peers(
                mock_peers::SAVED_MESSAGES,
                mock_peers::GROUP_CORE,
                mock_peers::CHANNEL_RELEASES,
                mock_peers::BOT_BUILD,
            ),
        }
    }

    fn peer_mut(&mut self, peer_id: PeerId) -> Option<&mut PeerProfile> {
        self.peers.iter_mut().find(|p| p.id == peer_id)
    }
}

fn populate_sample_media(
    media: &mut HashMap<(PeerId, SharedMediaTab), Vec<MediaItem>>,
    peer_id: PeerId,
) {
    media.insert(
        (peer_id, SharedMediaTab::PhotosVideos),
        vec![
            MediaItem::Photo {
                id: 501,
                date_group: "November 2026".to_owned(),
                width: 1920,
                height: 1080,
                caption: "Day theme layout benchmark".to_owned(),
                preview_rgb: [0x5C, 0xAF, 0xFA],
            },
            MediaItem::Video {
                id: 502,
                date_group: "November 2026".to_owned(),
                duration_secs: 47,
                width: 1280,
                height: 720,
                caption: "Adaptive column resize capture".to_owned(),
                preview_rgb: [0x3E, 0x92, 0xCC],
            },
            MediaItem::Photo {
                id: 503,
                date_group: "November 2026".to_owned(),
                width: 1440,
                height: 1440,
                caption: "Avatar crop grid test".to_owned(),
                preview_rgb: [0x9A, 0xD1, 0x64],
            },
            MediaItem::Photo {
                id: 504,
                date_group: "October 2026".to_owned(),
                width: 1600,
                height: 1200,
                caption: "Shader ripple stress test".to_owned(),
                preview_rgb: [0xB6, 0x94, 0xF9],
            },
            MediaItem::Video {
                id: 505,
                date_group: "October 2026".to_owned(),
                duration_secs: 134,
                width: 1920,
                height: 1080,
                caption: "Modal stack animation walkthrough".to_owned(),
                preview_rgb: [0xFE, 0xBB, 0x5B],
            },
            MediaItem::Photo {
                id: 506,
                date_group: "October 2026".to_owned(),
                width: 1200,
                height: 900,
                caption: "Palette swatch comparison".to_owned(),
                preview_rgb: [0xFF, 0x8A, 0xAC],
            },
        ],
    );

    media.insert(
        (peer_id, SharedMediaTab::Files),
        vec![
            MediaItem::File {
                id: 601,
                date_group: "November 2026".to_owned(),
                filename: "rustgram-architecture-v2.pdf".to_owned(),
                size_bytes: 2_458_624,
                ext: "PDF".to_owned(),
                date_label: "Nov 20 at 16:40".to_owned(),
            },
            MediaItem::File {
                id: 602,
                date_group: "November 2026".to_owned(),
                filename: "theme-day-palette.tdesktop-theme".to_owned(),
                size_bytes: 48_120,
                ext: "THM".to_owned(),
                date_label: "Nov 18 at 11:15".to_owned(),
            },
            MediaItem::File {
                id: 603,
                date_group: "October 2026".to_owned(),
                filename: "layer233-schema-diff.patch".to_owned(),
                size_bytes: 312_900,
                ext: "TXT".to_owned(),
                date_label: "Oct 29 at 09:05".to_owned(),
            },
            MediaItem::File {
                id: 604,
                date_group: "October 2026".to_owned(),
                filename: "benchmark-traces.zip".to_owned(),
                size_bytes: 8_910_400,
                ext: "ZIP".to_owned(),
                date_label: "Oct 14 at 19:22".to_owned(),
            },
        ],
    );

    media.insert(
        (peer_id, SharedMediaTab::Links),
        vec![
            MediaItem::Link {
                id: 701,
                date_group: "November 2026".to_owned(),
                url: "https://core.telegram.org/api".to_owned(),
                title: "Telegram API Documentation".to_owned(),
                description: "MTProto schema reference, methods, and layer updates.".to_owned(),
                domain: "core.telegram.org".to_owned(),
            },
            MediaItem::Link {
                id: 702,
                date_group: "November 2026".to_owned(),
                url: "https://github.com/telegramdesktop/tdesktop".to_owned(),
                title: "telegramdesktop/tdesktop".to_owned(),
                description: "Telegram Desktop official C++/Qt reference client (GPL-3.0)."
                    .to_owned(),
                domain: "github.com".to_owned(),
            },
            MediaItem::Link {
                id: 703,
                date_group: "October 2026".to_owned(),
                url: "https://docs.rs/egui/latest/egui/".to_owned(),
                title: "egui — Rust Immediate Mode GUI".to_owned(),
                description: "API documentation for egui widgets, painters, and input handling."
                    .to_owned(),
                domain: "docs.rs".to_owned(),
            },
            MediaItem::Link {
                id: 704,
                date_group: "October 2026".to_owned(),
                url: "https://www.gnu.org/licenses/gpl-3.0.html".to_owned(),
                title: "GNU General Public License v3.0".to_owned(),
                description: "Full text of the GPL-3.0 copyleft software license.".to_owned(),
                domain: "gnu.org".to_owned(),
            },
        ],
    );

    media.insert(
        (peer_id, SharedMediaTab::Voice),
        vec![
            MediaItem::Voice {
                id: 801,
                date_group: "November 2026".to_owned(),
                sender: "Mira Vance".to_owned(),
                duration_secs: 28,
                waveform: vec![
                    4, 9, 18, 25, 30, 22, 14, 8, 19, 27, 31, 24, 16, 10, 6, 12, 20, 15, 9, 5,
                ],
                listened: false,
                date_label: "Nov 21 at 18:10".to_owned(),
            },
            MediaItem::Voice {
                id: 802,
                date_group: "November 2026".to_owned(),
                sender: "Kaelen Orlov".to_owned(),
                duration_secs: 54,
                waveform: vec![
                    6, 12, 16, 22, 19, 28, 30, 25, 17, 11, 8, 14, 23, 29, 21, 13, 9, 11, 7, 4,
                ],
                listened: true,
                date_label: "Nov 19 at 10:04".to_owned(),
            },
            MediaItem::Voice {
                id: 803,
                date_group: "October 2026".to_owned(),
                sender: "Nolan Brooks".to_owned(),
                duration_secs: 15,
                waveform: vec![
                    3, 7, 15, 24, 28, 20, 11, 6, 14, 21, 18, 12, 8, 5, 9, 16, 12, 7, 4, 2,
                ],
                listened: true,
                date_label: "Oct 26 at 14:52".to_owned(),
            },
        ],
    );

    media.insert(
        (peer_id, SharedMediaTab::Music),
        vec![
            MediaItem::Music {
                id: 901,
                date_group: "November 2026".to_owned(),
                title: "Midnight Compiler".to_owned(),
                performer: "Synthwave Collective".to_owned(),
                duration_secs: 234,
                size_bytes: 5_640_120,
            },
            MediaItem::Music {
                id: 902,
                date_group: "November 2026".to_owned(),
                title: "Zero-Copy Ambient".to_owned(),
                performer: "Packet Radio".to_owned(),
                duration_secs: 189,
                size_bytes: 4_312_000,
            },
            MediaItem::Music {
                id: 903,
                date_group: "October 2026".to_owned(),
                title: "Northern Grid (Acoustic)".to_owned(),
                performer: "Glass Harbor".to_owned(),
                duration_secs: 262,
                size_bytes: 6_102_500,
            },
        ],
    );

    media.insert(
        (peer_id, SharedMediaTab::Gifs),
        vec![
            MediaItem::Gif {
                id: 1001,
                date_group: "November 2026".to_owned(),
                label: "Ship it!".to_owned(),
                duration_secs: 3,
                preview_rgb: [0x46, 0xBA, 0x43],
            },
            MediaItem::Gif {
                id: 1002,
                date_group: "November 2026".to_owned(),
                label: "Compiling...".to_owned(),
                duration_secs: 4,
                preview_rgb: [0x40, 0x8A, 0xCF],
            },
            MediaItem::Gif {
                id: 1003,
                date_group: "October 2026".to_owned(),
                label: "All tests green".to_owned(),
                duration_secs: 2,
                preview_rgb: [0x6C, 0x61, 0xDF],
            },
            MediaItem::Gif {
                id: 1004,
                date_group: "October 2026".to_owned(),
                label: "Coffee break".to_owned(),
                duration_secs: 3,
                preview_rgb: [0xF6, 0x81, 0x36],
            },
        ],
    );
}

impl InfoBackend for MockBackend {
    fn peer_profile(&self, peer_id: PeerId) -> Option<PeerProfile> {
        self.peers.iter().find(|p| p.id == peer_id).cloned()
    }

    fn all_peers(&self) -> Vec<PeerProfile> {
        self.peers.clone()
    }

    fn contacts(&self) -> Vec<PeerProfile> {
        self.peers
            .iter()
            .filter(|p| p.kind == InfoPeerKind::User && p.is_contact)
            .cloned()
            .collect()
    }

    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
        let Some(list) = self.members.get(&peer_id) else {
            return Vec::new();
        };
        let q = query.trim().to_lowercase();
        let mut out: Vec<MemberEntry> = list
            .iter()
            .filter(|m| {
                if q.is_empty() {
                    return true;
                }
                m.name.to_lowercase().contains(&q)
                    || m.username
                        .as_deref()
                        .is_some_and(|u| u.to_lowercase().contains(&q))
                    || m.role
                        .badge_label()
                        .is_some_and(|b| b.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        // Sort owners first, then admins, then online members, then by name.
        out.sort_by(|a, b| {
            let rank = |r: &MemberRole| match r {
                MemberRole::Owner { .. } => 0u8,
                MemberRole::Admin { .. } => 1,
                MemberRole::Member => 2,
                MemberRole::Restricted { .. } => 3,
            };
            rank(&a.role)
                .cmp(&rank(&b.role))
                .then_with(|| b.is_online.cmp(&a.is_online))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry> {
        self.common_groups
            .get(&user_id)
            .cloned()
            .unwrap_or_default()
    }

    fn shared_media(&self, peer_id: PeerId, tab: SharedMediaTab, query: &str) -> Vec<MediaItem> {
        let q = query.trim().to_lowercase();
        self.media
            .get(&(peer_id, tab))
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.matches_query(&q))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool {
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.muted = !peer.muted;
            peer.muted
        } else {
            false
        }
    }

    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool {
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.is_blocked = !peer.is_blocked;
            peer.is_blocked
        } else {
            false
        }
    }

    fn add_contact(
        &mut self,
        first_name: &str,
        last_name: &str,
        phone: &str,
    ) -> Result<PeerId, String> {
        let first = first_name.trim();
        let last = last_name.trim();
        if first.is_empty() {
            return Err("First name is required.".to_owned());
        }
        if first.chars().count() > MAX_USER_FIRST_LAST_NAME
            || last.chars().count() > MAX_USER_FIRST_LAST_NAME
        {
            return Err(format!(
                "Name must be at most {MAX_USER_FIRST_LAST_NAME} characters."
            ));
        }
        if !is_valid_phone(phone) {
            return Err("Please enter a valid phone number.".to_owned());
        }
        let bare = self.next_user_bare;
        self.next_user_bare += 1;
        let id = PeerId::from_user(bare);
        let full_name = if last.is_empty() {
            first.to_owned()
        } else {
            format!("{first} {last}")
        };
        self.peers.push(PeerProfile {
            id,
            kind: InfoPeerKind::User,
            title: full_name,
            status: "last seen recently".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: (bare % 7) as u8,
            phone: Some(format_phone(phone)),
            usernames: Vec::new(),
            bio: String::new(),
            birthday: None,
            invite_link: None,
            muted: false,
            is_contact: true,
            is_blocked: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts::default(),
        });
        Ok(id)
    }

    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err("Group name cannot be empty.".to_owned());
        }
        if title.chars().count() > MAX_GROUP_CHANNEL_TITLE {
            return Err(format!(
                "Group name must be at most {MAX_GROUP_CHANNEL_TITLE} characters."
            ));
        }
        let bare = self.next_chat_bare;
        self.next_chat_bare += 1;
        let id = PeerId::from_chat(bare);

        let mut group_members = vec![MemberEntry {
            peer_id: mock_peers::SAVED_MESSAGES,
            name: "You".to_owned(),
            username: Some("rustgram_tester".to_owned()),
            status_text: "online".to_owned(),
            is_online: true,
            is_bot: false,
            role: MemberRole::Owner { custom_title: None },
            color_index: 5,
        }];

        for member_id in &req.member_ids {
            if let Some(p) = self.peer_profile(*member_id)
                && !group_members.iter().any(|m| m.peer_id == p.id)
            {
                let username = p.primary_username().map(str::to_owned);
                group_members.push(MemberEntry {
                    peer_id: p.id,
                    name: p.title,
                    username,
                    status_text: p.status,
                    is_online: p.is_online,
                    is_bot: p.kind == InfoPeerKind::Bot,
                    role: MemberRole::Member,
                    color_index: p.color_index,
                });
            }
        }

        let total = group_members.len() as u32;
        let online = group_members.iter().filter(|m| m.is_online).count() as u32;
        self.members.insert(id, group_members);
        self.peers.push(PeerProfile {
            id,
            kind: InfoPeerKind::Supergroup,
            title: title.to_owned(),
            status: format_members_status(total, online),
            is_online: false,
            verified: false,
            premium: false,
            color_index: (bare % 7) as u8,
            phone: None,
            usernames: Vec::new(),
            bio: String::new(),
            birthday: None,
            invite_link: Some(format!("https://t.me/+GroupInvite{bare}")),
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: total,
            online_count: online,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::CHANGE_INFO,
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: req.auto_delete_ttl_seconds,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts::default(),
        });
        Ok(id)
    }

    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err("Channel name cannot be empty.".to_owned());
        }
        if title.chars().count() > MAX_GROUP_CHANNEL_TITLE {
            return Err(format!(
                "Channel name must be at most {MAX_GROUP_CHANNEL_TITLE} characters."
            ));
        }
        if req.description.chars().count() > MAX_CHANNEL_DESCRIPTION {
            return Err(format!(
                "Description must be at most {MAX_CHANNEL_DESCRIPTION} characters."
            ));
        }

        let mut usernames = Vec::new();
        if req.is_public {
            let raw = req.username.as_deref().unwrap_or_default();
            let avail = self.check_username(raw, None);
            if !avail.is_valid_public() {
                return Err(avail.message(raw));
            }
            usernames.push(PeerUsername {
                username: normalize_username(raw).to_owned(),
                is_editable: true,
                is_active: true,
            });
        }

        let bare = self.next_channel_bare;
        self.next_channel_bare += 1;
        let id = PeerId::from_channel(bare);

        self.members.insert(
            id,
            vec![MemberEntry {
                peer_id: mock_peers::SAVED_MESSAGES,
                name: "You".to_owned(),
                username: Some("rustgram_tester".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Owner { custom_title: None },
                color_index: 5,
            }],
        );
        self.peers.push(PeerProfile {
            id,
            kind: InfoPeerKind::Channel,
            title: title.to_owned(),
            status: format_subscribers_status(1),
            is_online: false,
            verified: false,
            premium: false,
            color_index: (bare % 7) as u8,
            phone: None,
            usernames,
            bio: req.description.trim().to_owned(),
            birthday: None,
            invite_link: Some(format!("https://t.me/+ChannelInvite{bare}")),
            muted: false,
            is_contact: false,
            is_blocked: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: true,
            can_manage_permissions: false,
            is_public: req.is_public,
            no_forwards: req.no_forwards,
            is_forum: false,
            members_count: 1,
            online_count: 0,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            media_counts: SharedMediaCounts::default(),
        });
        Ok(id)
    }

    fn update_peer_info(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerInfoRequest,
    ) -> Result<(), String> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err("Title cannot be empty.".to_owned());
        }
        if title.chars().count() > MAX_GROUP_CHANNEL_TITLE {
            return Err(format!(
                "Title must be at most {MAX_GROUP_CHANNEL_TITLE} characters."
            ));
        }
        if req.description.chars().count() > MAX_CHANNEL_DESCRIPTION {
            return Err(format!(
                "Description must be at most {MAX_CHANNEL_DESCRIPTION} characters."
            ));
        }
        if req.is_public
            && let Some(ref uname) = req.username
        {
            let avail = self.check_username(uname, Some(peer_id));
            if !avail.is_valid_public() {
                return Err(avail.message(uname));
            }
        }
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        peer.title = title.to_owned();
        peer.bio = req.description.trim().to_owned();
        peer.is_public = req.is_public;
        peer.no_forwards = req.no_forwards;
        if req.is_public {
            if let Some(uname) = req.username {
                let clean = normalize_username(&uname).to_owned();
                if let Some(slot) = peer.usernames.iter_mut().find(|u| u.is_editable) {
                    slot.username = clean;
                    slot.is_active = true;
                } else {
                    peer.usernames.insert(
                        0,
                        PeerUsername {
                            username: clean,
                            is_editable: true,
                            is_active: true,
                        },
                    );
                }
            }
        } else {
            peer.usernames.retain(|u| !u.is_editable);
        }
        Ok(())
    }

    fn update_default_permissions(
        &mut self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        slowmode_seconds: u32,
    ) -> Result<(), String> {
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        peer.default_banned_rights = rights;
        peer.slowmode_seconds = slowmode_seconds;
        Ok(())
    }

    fn update_admin_rights(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rights: ChatAdminRights,
        custom_title: String,
    ) -> Result<(), String> {
        let trimmed_title = custom_title.trim();
        if trimmed_title.chars().count() > MAX_ADMIN_TITLE_LENGTH {
            return Err(format!(
                "Custom title must be at most {MAX_ADMIN_TITLE_LENGTH} characters."
            ));
        }
        let list = self
            .members
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer has no members list.".to_owned())?;
        let member = list
            .iter_mut()
            .find(|m| m.peer_id == user_id)
            .ok_or_else(|| "Member not found.".to_owned())?;
        if matches!(member.role, MemberRole::Owner { .. }) {
            member.role = MemberRole::Owner {
                custom_title: (!trimmed_title.is_empty()).then(|| trimmed_title.to_owned()),
            };
        } else if rights.is_empty() {
            member.role = MemberRole::Member;
        } else {
            member.role = MemberRole::Admin {
                custom_title: (!trimmed_title.is_empty()).then(|| trimmed_title.to_owned()),
                rights,
            };
        }
        let admins_count = list.iter().filter(|m| m.role.is_admin_or_owner()).count() as u32;
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.admins_count = admins_count;
        }
        Ok(())
    }

    fn add_members(&mut self, peer_id: PeerId, member_ids: &[PeerId]) -> Result<usize, String> {
        let profiles: Vec<PeerProfile> = member_ids
            .iter()
            .filter_map(|&id| self.peer_profile(id))
            .collect();
        let list = self.members.entry(peer_id).or_default();
        let mut added = 0usize;
        for p in profiles {
            if !list.iter().any(|m| m.peer_id == p.id) {
                let username = p.primary_username().map(str::to_owned);
                list.push(MemberEntry {
                    peer_id: p.id,
                    name: p.title,
                    username,
                    status_text: p.status,
                    is_online: p.is_online,
                    is_bot: p.kind == InfoPeerKind::Bot,
                    role: MemberRole::Member,
                    color_index: p.color_index,
                });
                added += 1;
            }
        }
        let total = list.len() as u32;
        let online = list.iter().filter(|m| m.is_online).count() as u32;
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.members_count = total;
            peer.online_count = online;
            peer.status = format_members_status(total, online);
        }
        Ok(added)
    }

    fn remove_member(&mut self, peer_id: PeerId, user_id: PeerId) -> Result<(), String> {
        let list = self
            .members
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer has no members list.".to_owned())?;
        if list
            .iter()
            .any(|m| m.peer_id == user_id && matches!(m.role, MemberRole::Owner { .. }))
        {
            return Err("Cannot remove group owner.".to_owned());
        }
        let before = list.len();
        list.retain(|m| m.peer_id != user_id);
        if list.len() == before {
            return Err("Member not found.".to_owned());
        }
        let total = list.len() as u32;
        let online = list.iter().filter(|m| m.is_online).count() as u32;
        let admins = list.iter().filter(|m| m.role.is_admin_or_owner()).count() as u32;
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.members_count = total;
            peer.online_count = online;
            peer.admins_count = admins;
            peer.status = format_members_status(total, online);
        }
        Ok(())
    }

    fn set_auto_delete_ttl(&mut self, peer_id: PeerId, ttl_seconds: u32) -> Result<(), String> {
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        peer.auto_delete_ttl_seconds = ttl_seconds;
        Ok(())
    }

    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String> {
        let linked = match group_id {
            Some(gid) => {
                let title = self
                    .peer_profile(gid)
                    .map(|p| p.title)
                    .ok_or_else(|| "Discussion group not found.".to_owned())?;
                Some((gid, title))
            }
            None => None,
        };
        let Some(channel) = self.peer_mut(channel_id) else {
            return Err("Channel not found.".to_owned());
        };
        channel.linked_discussion_peer = linked;
        Ok(())
    }

    fn check_username(&self, username: &str, exclude_peer: Option<PeerId>) -> UsernameAvailability {
        let syntax = validate_username_syntax(username);
        if syntax != UsernameAvailability::Available {
            return syntax;
        }
        let clean = normalize_username(username).to_lowercase();
        let reserved = ["telegram", "support", "admin", "durov"];
        if reserved.contains(&clean.as_str()) {
            return UsernameAvailability::Occupied;
        }
        let taken = self.peers.iter().any(|peer| {
            if Some(peer.id) == exclude_peer {
                return false;
            }
            peer.usernames
                .iter()
                .any(|u| u.username.eq_ignore_ascii_case(&clean))
        });
        if taken {
            UsernameAvailability::Occupied
        } else {
            UsernameAvailability::Available
        }
    }

    fn update_username(&mut self, peer_id: PeerId, username: &str) -> Result<(), String> {
        let clean = normalize_username(username);
        if clean.is_empty() {
            let Some(peer) = self.peer_mut(peer_id) else {
                return Err("Peer not found.".to_owned());
            };
            peer.usernames.retain(|u| !u.is_editable);
            if peer.kind.is_channel() || peer.kind.is_group() {
                peer.is_public = !peer.usernames.is_empty();
            }
            return Ok(());
        }
        let avail = self.check_username(clean, Some(peer_id));
        if !avail.is_valid_public() {
            return Err(avail.message(clean));
        }
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        if let Some(slot) = peer.usernames.iter_mut().find(|u| u.is_editable) {
            slot.username = clean.to_owned();
            slot.is_active = true;
        } else {
            peer.usernames.insert(
                0,
                PeerUsername {
                    username: clean.to_owned(),
                    is_editable: true,
                    is_active: true,
                },
            );
        }
        if peer.kind.is_channel() || peer.kind.is_group() {
            peer.is_public = true;
        }
        Ok(())
    }

    fn forward_messages(
        &mut self,
        target_peers: &[PeerId],
        message_ids: &[i32],
        comment: &str,
    ) -> Result<usize, String> {
        if target_peers.is_empty() {
            return Err("Select at least one chat to forward to.".to_owned());
        }
        self.forward_records.push(ForwardRecord {
            targets: target_peers.to_vec(),
            message_ids: message_ids.to_vec(),
            comment: comment.trim().to_owned(),
        });
        Ok(target_peers.len())
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        message_ids: &[i32],
        revoke_for_everyone: bool,
    ) -> Result<usize, String> {
        let count = message_ids.len();
        self.deleted_batches
            .push((peer_id, count, revoke_for_everyone));
        Ok(count)
    }

    fn submit_report(
        &mut self,
        peer_id: PeerId,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.submitted_reports.push(SubmittedReport {
            peer_id,
            reason,
            comment: comment.trim().to_owned(),
        });
        Ok(())
    }

    fn languages(&self) -> Vec<LanguageEntry> {
        self.languages.clone()
    }

    fn active_language(&self) -> String {
        self.active_language.clone()
    }

    fn set_language(&mut self, lang_code: &str) -> Result<(), String> {
        if self.languages.iter().any(|l| l.id == lang_code) {
            self.active_language = lang_code.to_owned();
            Ok(())
        } else {
            Err(format!("Unknown language code '{lang_code}'."))
        }
    }

    fn terms_of_service(&self) -> TermsOfServiceData {
        self.tos.clone()
    }

    fn accept_terms(&mut self, tos_id: &str) -> Result<(), String> {
        if self.tos.id == tos_id {
            self.tos.accepted = true;
            Ok(())
        } else {
            Err("Terms of Service revision mismatch.".to_owned())
        }
    }

    fn exported_chat_invites(
        &self,
        peer_id: PeerId,
        revoked: bool,
    ) -> Result<InviteLinksList, String> {
        Ok(self.peer_mgmt.exported_chat_invites(peer_id, revoked))
    }

    fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        let entry =
            self.peer_mgmt
                .export_chat_invite(peer_id, mock_peers::SAVED_MESSAGES, "You", req);
        if entry.permanent
            && let Some(p) = self.peer_mut(peer_id)
        {
            p.invite_link = Some(entry.link.clone());
        }
        Ok(entry)
    }

    fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        self.peer_mgmt.edit_exported_chat_invite(peer_id, link, req)
    }

    fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        let (revoked, replacement) = self.peer_mgmt.revoke_exported_chat_invite(
            peer_id,
            link,
            mock_peers::SAVED_MESSAGES,
            "You",
        )?;
        if let Some(ref fresh) = replacement
            && let Some(p) = self.peer_mut(peer_id)
        {
            p.invite_link = Some(fresh.link.clone());
        }
        Ok((revoked, replacement))
    }

    fn delete_exported_chat_invite(&mut self, peer_id: PeerId, link: &str) -> Result<(), String> {
        self.peer_mgmt.delete_exported_chat_invite(peer_id, link)
    }

    fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.peer_mgmt.delete_revoked_exported_chat_invites(peer_id);
        Ok(())
    }

    fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> Result<InviteImportersList, String> {
        Ok(self
            .peer_mgmt
            .chat_invite_importers(peer_id, link, requested, query))
    }

    fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        let entry = self
            .peer_mgmt
            .hide_chat_join_request(peer_id, user_id, approved)?;
        if approved {
            self.add_approved_member(peer_id, &entry);
        }
        Ok(())
    }

    fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Result<usize, String> {
        let resolved = self
            .peer_mgmt
            .hide_all_chat_join_requests(peer_id, link, approved);
        let count = resolved.len();
        if approved {
            for entry in &resolved {
                self.add_approved_member(peer_id, entry);
            }
        }
        Ok(count)
    }

    fn join_requests_summary(&self, peer_id: PeerId) -> JoinRequestsSummary {
        self.peer_mgmt.join_requests_summary(peer_id)
    }

    fn peer_color_settings(&self, peer_id: PeerId) -> PeerColorSettings {
        let fallback = self.peer_profile(peer_id).map_or(0, |p| p.color_index);
        self.peer_mgmt.peer_color_settings(peer_id, fallback)
    }

    fn update_peer_color(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerColorRequest,
    ) -> Result<(), String> {
        if !req.for_profile
            && let Some(idx) = req.color_index
            && let Some(p) = self.peer_mut(peer_id)
        {
            p.color_index = idx % 7;
        }
        self.peer_mgmt.update_peer_color(peer_id, req);
        Ok(())
    }

    fn check_ownership_transfer(&self, _peer_id: PeerId) -> Result<OwnershipTransferCheck, String> {
        Ok(self.peer_mgmt.check_ownership_transfer())
    }

    fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), OwnershipTransferError> {
        self.peer_mgmt
            .transfer_ownership(peer_id, new_owner_id, password)?;
        if let Some(list) = self.members.get_mut(&peer_id) {
            for m in list.iter_mut() {
                if matches!(m.role, MemberRole::Owner { .. }) {
                    m.role = MemberRole::Admin {
                        custom_title: None,
                        rights: ChatAdminRights::all(),
                    };
                }
                if m.peer_id == new_owner_id {
                    m.role = MemberRole::Owner { custom_title: None };
                }
            }
        }
        Ok(())
    }

    fn similar_channels(&self, peer_id: PeerId) -> Result<SimilarChannelsResult, String> {
        Ok(self.peer_mgmt.similar_channels(peer_id))
    }

    fn downloads_list(&self, query: &str) -> Vec<DownloadEntry> {
        self.peer_mgmt.downloads_list(query)
    }

    fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        self.peer_mgmt.toggle_download_pause(download_id)
    }

    fn cancel_or_delete_download(
        &mut self,
        download_id: u64,
        _delete_file: bool,
    ) -> Result<(), String> {
        self.peer_mgmt.cancel_or_delete_download(download_id)
    }

    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        Ok(self.peer_mgmt.clear_completed_downloads())
    }

    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        Ok(self.peer_mgmt.delete_all_downloads())
    }

    fn global_media(
        &self,
        tab: SharedMediaTab,
        query: &str,
        limit: usize,
    ) -> crate::info::global_media::GlobalMediaPage {
        let q = query.trim().to_ascii_lowercase();
        let mut entries = Vec::new();
        for peer in &self.peers {
            if let Some(items) = self.media.get(&(peer.id, tab)) {
                for item in items {
                    if q.is_empty()
                        || item.matches_query(&q)
                        || peer.title.to_ascii_lowercase().contains(&q)
                    {
                        entries.push(crate::info::global_media::GlobalMediaEntry {
                            peer_id: peer.id,
                            peer_title: peer.title.clone(),
                            item: item.clone(),
                        });
                    }
                }
            }
        }
        let total_count = entries.len();
        let clamped = limit.max(1);
        let has_more = total_count > clamped;
        entries.truncate(clamped);
        crate::info::global_media::GlobalMediaPage {
            entries,
            total_count,
            next_cursor: None,
            has_more,
        }
    }
    fn peer_pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(self
            .profile_extras
            .pinned_stories(peer_id, offset_id, limit))
    }

    fn peer_stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(self
            .profile_extras
            .stories_archive(peer_id, offset_id, limit))
    }

    fn peer_story_albums(&self, peer_id: PeerId) -> Result<Vec<ProfileStoryAlbum>, String> {
        Ok(self.profile_extras.story_albums(peer_id))
    }

    fn peer_album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(self
            .profile_extras
            .album_stories(peer_id, album_id, offset_id, limit))
    }

    fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<ProfilePeerGiftsPage, String> {
        Ok(self.profile_extras.peer_saved_gifts(peer_id, filter))
    }

    fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        self.profile_extras
            .toggle_peer_gift_saved(peer_id, msg_id, saved_id, unsave)
    }

    fn peer_saved_music(
        &self,
        peer_id: PeerId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<SavedMusicTrack>, String> {
        Ok(self.profile_extras.saved_music(peer_id, offset, limit))
    }

    fn save_profile_music(&mut self, track: &SavedMusicTrack, unsave: bool) -> Result<(), String> {
        self.profile_extras
            .save_profile_music(mock_peers::SAVED_MESSAGES, track, unsave)
    }

    fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        self.profile_extras.upload_emoji_userpic(peer_id, req)
    }

    fn peer_available_reactions(&self, peer_id: PeerId) -> PeerReactionsConfig {
        self.profile_extras.peer_available_reactions(peer_id)
    }

    fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: PeerReactionsConfig,
    ) -> Result<(), String> {
        self.profile_extras
            .set_peer_available_reactions(peer_id, config)
    }

    fn toggle_peer_username(
        &mut self,
        peer_id: PeerId,
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        ProfileExtrasMockStore::toggle_username_in_list(&mut peer.usernames, username, active)
    }

    fn reorder_peer_usernames(&mut self, peer_id: PeerId, order: &[String]) -> Result<(), String> {
        let Some(peer) = self.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        ProfileExtrasMockStore::reorder_usernames_in_list(&mut peer.usernames, order)
    }

    fn peer_verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        self.profile_extras.verification_status(bot_id, peer_id)
    }

    fn set_peer_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        self.profile_extras
            .set_custom_verification(bot_id, peer_id, enabled, custom_description)
    }
    fn joined_communities(&self) -> Result<Vec<crate::boxes::community::CommunityDetails>, String> {
        Ok(self.communities.joined_communities())
    }

    fn community_details(
        &self,
        community_id: PeerId,
    ) -> Option<crate::boxes::community::CommunityDetails> {
        self.communities.community_details(community_id)
    }

    fn community_for_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<crate::boxes::community::CommunityDetails> {
        self.communities.community_for_peer(peer_id)
    }

    fn create_community(
        &mut self,
        req: crate::boxes::community::CreateCommunityRequest,
    ) -> Result<PeerId, String> {
        let (title, is_ch) = self.peer_profile(req.initial_peer_id).map_or_else(
            || ("Chat".to_owned(), false),
            |p| (p.title, p.kind.is_channel()),
        );
        self.communities.create_community(req, &title, is_ch)
    }

    fn toggle_community_peer_link(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        action: crate::boxes::community::CommunityPeerLinkAction,
    ) -> Result<(), String> {
        let (title, is_ch, is_bot) = self.peer_profile(peer_id).map_or_else(
            || (format!("Peer {}", peer_id.bare()), false, false),
            |p| (p.title, p.kind.is_channel(), p.kind == InfoPeerKind::Bot),
        );
        self.communities
            .toggle_peer_link(community_id, peer_id, &title, is_ch, is_bot, action)
    }

    fn toggle_community_collapsed_in_dialogs(
        &mut self,
        community_id: PeerId,
        collapsed: bool,
    ) -> Result<(), String> {
        self.communities
            .toggle_collapsed_in_dialogs(community_id, collapsed)
    }

    fn community_peer_link_requests(
        &self,
        community_id: PeerId,
        _offset: &str,
        _limit: i32,
    ) -> Result<crate::boxes::community::CommunityPeerRequestsSlice, String> {
        Ok(self.communities.peer_link_requests(community_id))
    }

    fn toggle_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        self.communities
            .toggle_peer_link_request_approval(community_id, peer_id, reject)
    }

    fn toggle_all_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        self.communities
            .toggle_all_peer_link_request_approval(community_id, reject)
    }

    fn toggle_community_participant_banned(
        &mut self,
        community_id: PeerId,
        participant_id: PeerId,
        unban: bool,
    ) -> Result<(), String> {
        self.communities
            .toggle_participant_banned(community_id, participant_id, unban)
    }

    fn community_participant_joined_chats(
        &self,
        community_id: PeerId,
        participant_id: PeerId,
    ) -> Result<crate::boxes::community::CommunityParticipantJoinedChats, String> {
        Ok(self
            .communities
            .participant_joined_chats(community_id, participant_id))
    }

    fn update_community_settings(
        &mut self,
        community_id: PeerId,
        title: &str,
        only_admins_can_add: bool,
    ) -> Result<(), String> {
        self.communities
            .update_community_settings(community_id, title, only_admins_can_add)
    }

    fn delete_community(&mut self, community_id: PeerId) -> Result<(), String> {
        self.communities.delete_community(community_id)
    }

    fn check_bot_username(&self, username: &str) -> Result<bool, String> {
        Ok(self.communities.check_bot_username(username))
    }

    fn create_managed_bot(
        &mut self,
        manager_id: PeerId,
        name: &str,
        username: &str,
        via_deeplink: bool,
    ) -> Result<PeerId, String> {
        self.communities
            .create_managed_bot(manager_id, name, username, via_deeplink)
    }

    fn get_bot_info(
        &self,
        bot_id: PeerId,
        lang_code: &str,
    ) -> Result<crate::boxes::managed_bot::BotInfoTexts, String> {
        Ok(self.communities.get_bot_info(bot_id, lang_code))
    }

    fn set_bot_info(
        &mut self,
        bot_id: PeerId,
        lang_code: &str,
        change: &crate::boxes::managed_bot::BotInfoChange,
    ) -> Result<(), String> {
        self.communities.set_bot_info(bot_id, lang_code, change)
    }

    fn translate_bot_info_texts(
        &self,
        texts: &[String],
        lang_code: &str,
    ) -> Result<Vec<String>, String> {
        Ok(self.communities.translate_bot_info_texts(texts, lang_code))
    }

    fn moderate_messages(
        &mut self,
        req: crate::boxes::moderate_messages::ModerateMessagesRequest,
    ) -> Result<crate::boxes::moderate_messages::ModerateMessagesResult, String> {
        self.communities.moderate_messages(req)
    }

    fn welcome_messages(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<crate::boxes::welcome_messages::WelcomeMessageItem>, String> {
        Ok(self.communities.welcome_messages(peer_id))
    }

    fn send_welcome_message(
        &mut self,
        peer_id: PeerId,
        text: &str,
    ) -> Result<crate::boxes::welcome_messages::WelcomeMessageItem, String> {
        self.communities
            .send_welcome_message(peer_id, mock_peers::SAVED_MESSAGES, text)
    }

    fn edit_welcome_message(
        &mut self,
        peer_id: PeerId,
        id: i32,
        text: &str,
    ) -> Result<crate::boxes::welcome_messages::WelcomeMessageItem, String> {
        self.communities.edit_welcome_message(peer_id, id, text)
    }

    fn delete_welcome_message(&mut self, peer_id: PeerId, id: i32) -> Result<(), String> {
        self.communities.delete_welcome_message(peer_id, id)
    }

    fn delete_all_welcome_messages(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.communities.delete_all_welcome_messages(peer_id)
    }

    fn channel_anti_spam_enabled(&self, peer_id: PeerId) -> bool {
        self.communities.channel_anti_spam_enabled(peer_id)
    }

    fn toggle_channel_anti_spam(&mut self, peer_id: PeerId, enabled: bool) -> Result<(), String> {
        self.communities.toggle_channel_anti_spam(peer_id, enabled)
    }

    fn report_antispam_false_positive(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> Result<(), String> {
        self.communities
            .report_antispam_false_positive(peer_id, msg_id)
    }

    fn get_collectible_info(
        &self,
        peer_id: PeerId,
        entity: &str,
    ) -> Result<crate::boxes::collectible_info::CollectibleInfoData, String> {
        let (owner_name, color_index) = self.peer_profile(peer_id).map_or_else(
            || ("Demo Owner".to_owned(), 0),
            |p| (p.title, p.color_index),
        );
        let clean = entity.trim().trim_start_matches('@');
        let slug: String = clean
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let url = if entity.trim().starts_with('+') {
            format!("https://fragment.com/number/{slug}")
        } else {
            format!("https://fragment.com/username/{slug}")
        };
        Ok(
            crate::boxes::collectible_info::CollectibleInfoData::from_fragment_fields(
                entity.trim(),
                peer_id,
                owner_name,
                color_index,
                1_700_000_000,
                "USD",
                45_000,
                "TON",
                85_000_000_000,
                url,
            ),
        )
    }

    fn set_main_profile_tab(&mut self, peer_id: PeerId, tab: ProfileMainTab) -> Result<(), String> {
        self.profile_extras.set_main_profile_tab(peer_id, tab)
    }

    fn main_profile_tab(&self, peer_id: PeerId) -> Option<ProfileMainTab> {
        self.profile_extras.main_profile_tab(peer_id)
    }

    fn suggest_user_birthday(&mut self, user_id: PeerId, birthday: Birthday) -> Result<(), String> {
        self.profile_extras.suggest_user_birthday(user_id, birthday)
    }

    fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.profile_extras
            .report_profile_photo(peer_id, photo, reason, comment)
    }

    fn personal_channel(&self, peer_id: PeerId) -> Option<PersonalChannelOption> {
        self.profile_extras.personal_channel(peer_id)
    }

    fn personal_channel_candidates(&self) -> Vec<PersonalChannelOption> {
        self.peers
            .iter()
            .filter(|p| p.kind == InfoPeerKind::Channel)
            .map(|p| PersonalChannelOption {
                peer_id: p.id,
                title: p.title.clone(),
                username: p.primary_username().map(ToOwned::to_owned),
                subscribers_count: p.members_count,
            })
            .collect()
    }

    fn update_personal_channel(&mut self, channel_id: Option<PeerId>) -> Result<(), String> {
        let opt = match channel_id {
            Some(cid) => {
                let p = self
                    .peers
                    .iter()
                    .find(|p| p.id == cid && p.kind == InfoPeerKind::Channel)
                    .ok_or_else(|| "Selected channel not found.".to_owned())?;
                Some(PersonalChannelOption {
                    peer_id: p.id,
                    title: p.title.clone(),
                    username: p.primary_username().map(ToOwned::to_owned),
                    subscribers_count: p.members_count,
                })
            }
            None => None,
        };
        self.profile_extras
            .set_personal_channel(mock_peers::SAVED_MESSAGES, opt);
        Ok(())
    }

    fn peer_profile_balance(&self, peer_id: PeerId) -> Option<ProfileBalanceInfo> {
        self.profile_extras.peer_profile_balance(peer_id)
    }

    fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        self.profile_extras
            .chat_star_gift_notifications_enabled(peer_id)
    }

    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        self.profile_extras
            .toggle_chat_star_gift_notifications(peer_id, enabled)
    }
}

impl MockBackend {
    fn add_approved_member(&mut self, peer_id: PeerId, entry: &InviteImporterInfo) {
        let list = self.members.entry(peer_id).or_default();
        if !list.iter().any(|m| m.peer_id == entry.user_id) {
            list.push(MemberEntry {
                peer_id: entry.user_id,
                name: entry.name.clone(),
                username: entry.username.clone(),
                status_text: "recently".to_owned(),
                is_online: false,
                is_bot: false,
                role: MemberRole::Member,
                color_index: entry.color_index,
            });
            if let Some(p) = self.peer_mut(peer_id) {
                p.members_count += 1;
            }
        }
    }
}
