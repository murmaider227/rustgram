//! Deterministic in-memory mock store for WP-26 profile extras & peer management:
//! - Profile Stories & Albums (`stories.getPinnedStories`, `getStoriesArchive`, `getAlbums`, `getAlbumStories`)
//! - Peer Gifts & Collections (`payments.getSavedStarGifts`, `getStarGiftCollections`, `saveStarGift`)
//! - Saved Music on Profile (`users.getSavedMusic`, `account.saveMusic`)
//! - Emoji Userpic Builder uploads (`videoSizeEmojiMarkup`)
//! - Per-chat available reactions (`messages.setChatAvailableReactions`)
//! - Collectible username activation & reordering (`channels.{toggle,reorder}Usernames`)
//! - Bot custom verification (`bots.setCustomVerification`)
//! - Business-bot pause/resume bar in chat (`account.toggleConnectedBotPaused`, `disablePeerConnectedBot`)
//!
//! Shared by [`crate::info::mock::MockBackend`] and [`tg_app_core::stand_in::mock_session::MockSession`].

use std::collections::HashMap;

use tg_data::{CollectibleId, DocumentId, MsgId as DataMsgId};
use tg_payments::star_gifts::{
    GiftCollection, PeerGiftsFilter, RgbColor, SavedStarGift, SavedStarGiftId, StarGift,
    UniqueGift, UniqueGiftAttribute, UniqueGiftBackdrop, UniqueGiftModel, UniqueGiftPattern,
};

use crate::info::backend::{
    ChatAvailableReactionsMode, EmojiUserpicRequest, PeerReactionsConfig, PeerUsername,
    PeerVerificationStatus, PersonalChannelOption, ProfileBalanceInfo, ProfileMainTab,
    ProfilePeerGiftsPage, ProfilePhotoRef, ProfileStoriesPage, ProfileStoryAlbum, ProfileStoryItem,
    ReportReason, SavedMusicTrack, default_standard_reactions,
};
use crate::info::values::Birthday;
use crate::payments::backend::ReceivedGiftItem;
use tg_app_core::history::backend::BusinessBotBarState;
use tg_app_core::stand_in::types::PeerId;

/// Deterministic Unix timestamp for synthetic WP-26 demo entries (`2026-11-22 14:00:00 UTC`).
const MOCK_NOW: i32 = 1_795_356_000;

/// Reusable in-memory state backing WP-26 features in mock/demo sessions.
#[derive(Debug, Clone)]
pub struct ProfileExtrasMockStore {
    pinned_stories: HashMap<PeerId, Vec<ProfileStoryItem>>,
    archived_stories: HashMap<PeerId, Vec<ProfileStoryItem>>,
    story_albums: HashMap<PeerId, Vec<ProfileStoryAlbum>>,
    peer_gifts: HashMap<PeerId, Vec<ReceivedGiftItem>>,
    gift_collections: HashMap<PeerId, Vec<GiftCollection>>,
    saved_music: HashMap<PeerId, Vec<SavedMusicTrack>>,
    reactions: HashMap<PeerId, PeerReactionsConfig>,
    verifications: HashMap<(PeerId, PeerId), PeerVerificationStatus>,
    business_bots: HashMap<PeerId, BusinessBotBarState>,
    /// Audit log of uploaded emoji userpics `(peer_id, request)`.
    pub uploaded_emoji_userpics: Vec<(PeerId, EmojiUserpicRequest)>,
    /// Per-peer preferred main profile tab (`account.setMainProfileTab`).
    main_profile_tabs: HashMap<PeerId, ProfileMainTab>,
    /// Per-peer linked personal channel (`account.updatePersonalChannel`).
    personal_channels: HashMap<PeerId, PersonalChannelOption>,
    /// Recorded birthday suggestions `(peer_id, birthday)` (`users.suggestBirthday`).
    pub suggested_birthdays: Vec<(PeerId, Birthday)>,
    /// Recorded profile photo reports `(peer_id, photo, reason, comment)`.
    pub reported_profile_photos: Vec<(PeerId, ProfilePhotoRef, ReportReason, String)>,
    /// Per-peer balance pill state (`info_profile_balance_pill`).
    peer_balances: HashMap<PeerId, ProfileBalanceInfo>,
    /// Per-channel/chat Star Gift notification preference (`payments.toggleChatStarGiftNotifications`).
    chat_gift_notifications: HashMap<PeerId, bool>,
}

impl Default for ProfileExtrasMockStore {
    fn default() -> Self {
        Self::new_for_peers(
            PeerId::from_user(1000),
            PeerId::from_user(1001),
            PeerId::from_chat(2001),
            PeerId::from_channel(3001),
            PeerId::from_user(1005),
        )
    }
}

impl ProfileExtrasMockStore {
    /// Builds deterministic fictional demo data anchored on known peer IDs.
    #[must_use]
    pub fn new_for_peers(
        self_id: PeerId,
        contact_user: PeerId,
        primary_group: PeerId,
        primary_channel: PeerId,
        helper_bot: PeerId,
    ) -> Self {
        let mut pinned_stories: HashMap<PeerId, Vec<ProfileStoryItem>> = HashMap::new();
        let mut archived_stories: HashMap<PeerId, Vec<ProfileStoryItem>> = HashMap::new();
        let mut story_albums: HashMap<PeerId, Vec<ProfileStoryAlbum>> = HashMap::new();
        let mut peer_gifts: HashMap<PeerId, Vec<ReceivedGiftItem>> = HashMap::new();
        let mut gift_collections: HashMap<PeerId, Vec<GiftCollection>> = HashMap::new();
        let mut saved_music: HashMap<PeerId, Vec<SavedMusicTrack>> = HashMap::new();
        let mut reactions: HashMap<PeerId, PeerReactionsConfig> = HashMap::new();
        let mut verifications: HashMap<(PeerId, PeerId), PeerVerificationStatus> = HashMap::new();
        let mut business_bots: HashMap<PeerId, BusinessBotBarState> = HashMap::new();

        // 1. Profile Stories & Albums
        let user_stories = vec![
            ProfileStoryItem {
                id: 501,
                peer_id: contact_user,
                date: MOCK_NOW - 86_400 * 2,
                caption: "Alpine trail sunrise".to_owned(),
                pinned: true,
                is_video: false,
                duration_secs: 0,
                views_count: 142,
                album_ids: vec![1],
                preview_rgb: [0x4A, 0x8F, 0xD6],
            },
            ProfileStoryItem {
                id: 502,
                peer_id: contact_user,
                date: MOCK_NOW - 86_400 * 5,
                caption: "Rust egui shader test".to_owned(),
                pinned: true,
                is_video: true,
                duration_secs: 18,
                views_count: 289,
                album_ids: vec![2],
                preview_rgb: [0x8B, 0x5C, 0xF6],
            },
            ProfileStoryItem {
                id: 503,
                peer_id: contact_user,
                date: MOCK_NOW - 86_400 * 11,
                caption: "Harbor evening timelapse".to_owned(),
                pinned: true,
                is_video: true,
                duration_secs: 24,
                views_count: 194,
                album_ids: vec![1],
                preview_rgb: [0xE6, 0x7E, 0x22],
            },
        ];
        pinned_stories.insert(contact_user, user_stories.clone());
        pinned_stories.insert(
            self_id,
            user_stories
                .iter()
                .map(|s| ProfileStoryItem {
                    peer_id: self_id,
                    ..s.clone()
                })
                .collect(),
        );
        pinned_stories.insert(
            primary_channel,
            vec![
                ProfileStoryItem {
                    id: 601,
                    peer_id: primary_channel,
                    date: MOCK_NOW - 3_600 * 6,
                    caption: "Layer 233 build milestone".to_owned(),
                    pinned: true,
                    is_video: false,
                    duration_secs: 0,
                    views_count: 1_840,
                    album_ids: vec![10],
                    preview_rgb: [0x2E, 0xA6, 0x52],
                },
                ProfileStoryItem {
                    id: 602,
                    peer_id: primary_channel,
                    date: MOCK_NOW - 86_400 * 3,
                    caption: "Audio engine demo clip".to_owned(),
                    pinned: true,
                    is_video: true,
                    duration_secs: 32,
                    views_count: 2_310,
                    album_ids: vec![10],
                    preview_rgb: [0x33, 0x90, 0xEC],
                },
            ],
        );

        let self_archive = vec![
            ProfileStoryItem {
                id: 401,
                peer_id: self_id,
                date: MOCK_NOW - 86_400 * 30,
                caption: "Draft prototype notes".to_owned(),
                pinned: false,
                is_video: false,
                duration_secs: 0,
                views_count: 64,
                album_ids: vec![2],
                preview_rgb: [0x5B, 0x6E, 0x82],
            },
            ProfileStoryItem {
                id: 402,
                peer_id: self_id,
                date: MOCK_NOW - 86_400 * 45,
                caption: "Old sprint retrospective".to_owned(),
                pinned: false,
                is_video: true,
                duration_secs: 12,
                views_count: 51,
                album_ids: Vec::new(),
                preview_rgb: [0x6E, 0x8F, 0x5B],
            },
        ];
        archived_stories.insert(self_id, self_archive);
        archived_stories.insert(
            contact_user,
            vec![ProfileStoryItem {
                id: 499,
                peer_id: contact_user,
                date: MOCK_NOW - 86_400 * 21,
                caption: "Archived sketch".to_owned(),
                pinned: false,
                is_video: false,
                duration_secs: 0,
                views_count: 78,
                album_ids: Vec::new(),
                preview_rgb: [0x78, 0x6F, 0xA6],
            }],
        );

        story_albums.insert(
            contact_user,
            vec![
                ProfileStoryAlbum {
                    album_id: 1,
                    title: "Outdoors".to_owned(),
                    count: 2,
                },
                ProfileStoryAlbum {
                    album_id: 2,
                    title: "Dev Logs".to_owned(),
                    count: 1,
                },
            ],
        );
        story_albums.insert(
            self_id,
            vec![
                ProfileStoryAlbum {
                    album_id: 1,
                    title: "Travel".to_owned(),
                    count: 2,
                },
                ProfileStoryAlbum {
                    album_id: 2,
                    title: "Work".to_owned(),
                    count: 2,
                },
            ],
        );
        story_albums.insert(
            primary_channel,
            vec![ProfileStoryAlbum {
                album_id: 10,
                title: "Releases".to_owned(),
                count: 2,
            }],
        );

        // 2. Peer Star Gifts & Collections (reusing tg-payments models)
        let sample_gifts = build_sample_received_gifts();
        let sample_collections = vec![
            GiftCollection {
                id: 101,
                title: "Favorites".to_owned(),
                count: 2,
                icon_document_id: None,
                hash: 101,
            },
            GiftCollection {
                id: 102,
                title: "Collectibles".to_owned(),
                count: 1,
                icon_document_id: None,
                hash: 102,
            },
        ];
        peer_gifts.insert(contact_user, sample_gifts.clone());
        peer_gifts.insert(self_id, sample_gifts);
        gift_collections.insert(contact_user, sample_collections.clone());
        gift_collections.insert(self_id, sample_collections);

        // 3. Saved Profile Music
        let demo_tracks = vec![
            SavedMusicTrack {
                document_id: 910_001,
                access_hash: 0x1122_3344,
                file_reference: vec![1, 2, 3, 4],
                title: "Midnight Compiler".to_owned(),
                performer: "Synthwave Collective".to_owned(),
                duration_secs: 214,
                size_bytes: 5_242_880,
                saved_on_profile: true,
            },
            SavedMusicTrack {
                document_id: 910_002,
                access_hash: 0x2233_4455,
                file_reference: vec![5, 6, 7, 8],
                title: "Northern Lights Loop".to_owned(),
                performer: "Ambient Lab".to_owned(),
                duration_secs: 186,
                size_bytes: 4_194_304,
                saved_on_profile: true,
            },
            SavedMusicTrack {
                document_id: 910_003,
                access_hash: 0x3344_5566,
                file_reference: vec![9, 10, 11, 12],
                title: "Packet Waltz".to_owned(),
                performer: "Protocol Quartet".to_owned(),
                duration_secs: 248,
                size_bytes: 6_100_000,
                saved_on_profile: true,
            },
        ];
        saved_music.insert(contact_user, demo_tracks.clone());
        saved_music.insert(self_id, demo_tracks);

        // 4. Per-chat Available Reactions
        reactions.insert(
            primary_group,
            PeerReactionsConfig {
                mode: ChatAvailableReactionsMode::All { allow_custom: true },
                reactions_limit: Some(11),
                paid_enabled: false,
                available_standard: default_standard_reactions(),
            },
        );
        reactions.insert(
            primary_channel,
            PeerReactionsConfig {
                mode: ChatAvailableReactionsMode::Some {
                    emojis: vec!["👍".to_owned(), "🔥".to_owned(), "❤".to_owned()],
                    custom_emoji_ids: Vec::new(),
                },
                reactions_limit: Some(6),
                paid_enabled: true,
                available_standard: default_standard_reactions(),
            },
        );

        // 5. Bot Custom Verification demo entry
        verifications.insert(
            (helper_bot, primary_channel),
            PeerVerificationStatus {
                bot_id: helper_bot,
                peer_id: primary_channel,
                verified: true,
                custom_description: "Verified official release channel".to_owned(),
            },
        );

        // 6. Business-bot bar on a demo private chat
        business_bots.insert(
            contact_user,
            BusinessBotBarState {
                bot_id: helper_bot,
                bot_name: "Support Desk Bot".to_owned(),
                bot_username: Some("support_desk_bot".to_owned()),
                manage_url: Some("https://t.me/support_desk_bot?start=biz_manage".to_owned()),
                can_reply: true,
                paused: false,
            },
        );

        let mut main_profile_tabs: HashMap<PeerId, ProfileMainTab> = HashMap::new();
        main_profile_tabs.insert(self_id, ProfileMainTab::Posts);
        main_profile_tabs.insert(contact_user, ProfileMainTab::Gifts);

        let mut personal_channels: HashMap<PeerId, PersonalChannelOption> = HashMap::new();
        personal_channels.insert(
            contact_user,
            PersonalChannelOption {
                peer_id: primary_channel,
                title: "Rustgram Release Notes".to_owned(),
                username: Some("rustgram_releases".to_owned()),
                subscribers_count: 4_280,
            },
        );

        let mut peer_balances: HashMap<PeerId, ProfileBalanceInfo> = HashMap::new();
        peer_balances.insert(
            self_id,
            ProfileBalanceInfo {
                ton_nanos: 12_450_000_000,
                stars_balance: 1_450,
            },
        );
        peer_balances.insert(
            contact_user,
            ProfileBalanceInfo {
                ton_nanos: 8_750_000_000,
                stars_balance: 620,
            },
        );

        let mut chat_gift_notifications: HashMap<PeerId, bool> = HashMap::new();
        chat_gift_notifications.insert(primary_channel, true);

        Self {
            pinned_stories,
            archived_stories,
            story_albums,
            peer_gifts,
            gift_collections,
            saved_music,
            reactions,
            verifications,
            business_bots,
            uploaded_emoji_userpics: Vec::new(),
            main_profile_tabs,
            personal_channels,
            suggested_birthdays: Vec::new(),
            reported_profile_photos: Vec::new(),
            peer_balances,
            chat_gift_notifications,
        }
    }

    /// Returns pinned stories for `peer_id`.
    #[must_use]
    pub fn pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> ProfileStoriesPage {
        let all = self
            .pinned_stories
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        slice_stories(all, offset_id, limit)
    }

    /// Returns archived stories for `peer_id`.
    #[must_use]
    pub fn stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> ProfileStoriesPage {
        let all = self
            .archived_stories
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        slice_stories(all, offset_id, limit)
    }

    /// Returns story albums for `peer_id`.
    #[must_use]
    pub fn story_albums(&self, peer_id: PeerId) -> Vec<ProfileStoryAlbum> {
        self.story_albums.get(&peer_id).cloned().unwrap_or_default()
    }

    /// Returns stories belonging to `album_id` for `peer_id`.
    #[must_use]
    pub fn album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> ProfileStoriesPage {
        let mut combined = self
            .pinned_stories
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        if let Some(arch) = self.archived_stories.get(&peer_id) {
            combined.extend(arch.iter().cloned());
        }
        let filtered: Vec<ProfileStoryItem> = combined
            .into_iter()
            .filter(|s| s.album_ids.contains(&album_id))
            .collect();
        slice_stories(filtered, offset_id, limit)
    }

    /// Returns filtered Star Gifts & collections for `peer_id`.
    #[must_use]
    pub fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &PeerGiftsFilter,
    ) -> ProfilePeerGiftsPage {
        let all = self.peer_gifts.get(&peer_id).cloned().unwrap_or_default();
        let total_count = all.len() as i32;
        let mut filtered: Vec<ReceivedGiftItem> = all
            .into_iter()
            .filter(|item| {
                let g = &item.saved.info;
                let is_unique = g.unique.is_some();
                let is_limited = g.limited_count > 0;
                if filter.skip_unique && is_unique {
                    return false;
                }
                if filter.skip_limited && is_limited && !is_unique {
                    return false;
                }
                if filter.skip_unlimited && !is_limited && !is_unique {
                    return false;
                }
                if filter.skip_saved && !item.saved.hidden {
                    return false;
                }
                if filter.skip_unsaved && item.saved.hidden {
                    return false;
                }
                if let Some(cid) = filter.collection_id
                    && !item.saved.collection_ids.contains(&cid)
                {
                    return false;
                }
                true
            })
            .collect();

        if filter.sort_by_value {
            filtered.sort_by(|a, b| {
                b.saved
                    .info
                    .stars
                    .cmp(&a.saved.info.stars)
                    .then_with(|| b.saved.date.cmp(&a.saved.date))
            });
        } else {
            filtered.sort_by(|a, b| {
                b.saved
                    .pinned
                    .cmp(&a.saved.pinned)
                    .then_with(|| b.saved.date.cmp(&a.saved.date))
            });
        }

        let collections = self
            .gift_collections
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();

        ProfilePeerGiftsPage {
            total_count,
            gifts: filtered,
            collections,
        }
    }

    /// Toggles saved/hidden status of a Star Gift on `peer_id`.
    pub fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        let Some(list) = self.peer_gifts.get_mut(&peer_id) else {
            return Err("No gifts found for this peer.".to_owned());
        };
        let Some(item) = list.iter_mut().find(|g| match g.saved.manage_id {
            Some(SavedStarGiftId::User(mid)) => msg_id != 0 && mid.0 == i64::from(msg_id),
            Some(SavedStarGiftId::Chat { saved_id: sid, .. }) => saved_id != 0 && sid == saved_id,
            _ => g.saved.info.id == saved_id,
        }) else {
            return Err("Gift not found.".to_owned());
        };
        item.saved.hidden = unsave;
        Ok(())
    }

    /// Returns saved music tracks for `peer_id`.
    #[must_use]
    pub fn saved_music(&self, peer_id: PeerId, offset: i32, limit: i32) -> Vec<SavedMusicTrack> {
        let list = self.saved_music.get(&peer_id).cloned().unwrap_or_default();
        let start = offset.max(0) as usize;
        let lim = if limit <= 0 {
            list.len()
        } else {
            limit as usize
        };
        list.into_iter().skip(start).take(lim).collect()
    }

    /// Saves or unsaves a music track on the self profile.
    pub fn save_profile_music(
        &mut self,
        self_id: PeerId,
        track: &SavedMusicTrack,
        unsave: bool,
    ) -> Result<(), String> {
        let list = self.saved_music.entry(self_id).or_default();
        if unsave {
            if let Some(existing) = list.iter_mut().find(|t| t.document_id == track.document_id) {
                existing.saved_on_profile = false;
            }
            list.retain(|t| t.document_id != track.document_id);
        } else if let Some(existing) = list.iter_mut().find(|t| t.document_id == track.document_id)
        {
            existing.saved_on_profile = true;
        } else {
            let mut copy = track.clone();
            copy.saved_on_profile = true;
            list.insert(0, copy);
        }
        Ok(())
    }

    /// Records an uploaded emoji userpic.
    pub fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        if req.background_colors.is_empty() || req.background_colors.len() > 4 {
            return Err("Background must have between 1 and 4 colors.".to_owned());
        }
        self.uploaded_emoji_userpics.push((peer_id, req));
        Ok(())
    }

    /// Returns current allowed reactions for `peer_id`.
    #[must_use]
    pub fn peer_available_reactions(&self, peer_id: PeerId) -> PeerReactionsConfig {
        self.reactions.get(&peer_id).cloned().unwrap_or_default()
    }

    /// Updates allowed reactions for `peer_id`.
    pub fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: PeerReactionsConfig,
    ) -> Result<(), String> {
        if let Some(lim) = config.reactions_limit
            && !(1..=11).contains(&lim)
        {
            return Err("Reactions limit must be between 1 and 11.".to_owned());
        }
        self.reactions.insert(peer_id, config);
        Ok(())
    }

    /// Toggles a username's `is_active` state in a peer's username list.
    pub fn toggle_username_in_list(
        usernames: &mut [PeerUsername],
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        let target = username.trim().trim_start_matches('@');
        let Some(entry) = usernames
            .iter_mut()
            .find(|u| u.username.eq_ignore_ascii_case(target))
        else {
            return Err(format!("Username @{target} not found."));
        };
        entry.is_active = active;
        // Keep active usernames sorted ahead of inactive ones (matching tdesktop's pinned interval).
        usernames.sort_by_key(|a| std::cmp::Reverse(a.is_active));
        Ok(())
    }

    /// Reorders active usernames in a peer's username list.
    pub fn reorder_usernames_in_list(
        usernames: &mut Vec<PeerUsername>,
        order: &[String],
    ) -> Result<(), String> {
        let mut active: Vec<_> = usernames.iter().filter(|u| u.is_active).cloned().collect();
        let inactive: Vec<_> = usernames.iter().filter(|u| !u.is_active).cloned().collect();

        let mut reordered = Vec::with_capacity(active.len());
        for wanted in order {
            let clean = wanted.trim().trim_start_matches('@');
            if let Some(pos) = active
                .iter()
                .position(|u| u.username.eq_ignore_ascii_case(clean))
            {
                reordered.push(active.remove(pos));
            }
        }
        reordered.extend(active);
        reordered.extend(inactive);
        *usernames = reordered;
        Ok(())
    }

    /// Returns bot verification status for `(bot_id, peer_id)`.
    #[must_use]
    pub fn verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        self.verifications
            .get(&(bot_id, peer_id))
            .cloned()
            .unwrap_or(PeerVerificationStatus {
                bot_id,
                peer_id,
                verified: false,
                custom_description: String::new(),
            })
    }

    /// Updates bot verification status for `(bot_id, peer_id)`.
    pub fn set_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        let desc = custom_description.unwrap_or_default().trim().to_owned();
        if desc.chars().count() > 70 {
            return Err(
                "Custom verification description must be at most 70 characters.".to_owned(),
            );
        }
        if enabled {
            self.verifications.insert(
                (bot_id, peer_id),
                PeerVerificationStatus {
                    bot_id,
                    peer_id,
                    verified: true,
                    custom_description: desc,
                },
            );
        } else {
            self.verifications.remove(&(bot_id, peer_id));
        }
        Ok(())
    }

    /// Returns the Business-bot bar state for `peer_id`, if connected.
    #[must_use]
    pub fn business_bot_bar(&self, peer_id: PeerId) -> Option<BusinessBotBarState> {
        self.business_bots.get(&peer_id).cloned()
    }

    /// Toggles pause/resume for the connected Business bot in `peer_id`.
    pub fn toggle_business_bot_paused(
        &mut self,
        peer_id: PeerId,
        paused: bool,
    ) -> Result<(), String> {
        let Some(bot) = self.business_bots.get_mut(&peer_id) else {
            return Err("No business bot is connected to this chat.".to_owned());
        };
        bot.paused = paused;
        Ok(())
    }

    /// Disconnects the Business bot from `peer_id`.
    pub fn disable_business_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        if self.business_bots.remove(&peer_id).is_none() {
            return Err("No business bot is connected to this chat.".to_owned());
        }
        Ok(())
    }

    /// Returns the preferred main profile tab for `peer_id`.
    #[must_use]
    pub fn main_profile_tab(&self, peer_id: PeerId) -> Option<ProfileMainTab> {
        self.main_profile_tabs.get(&peer_id).copied()
    }

    /// Updates the preferred main profile tab for `peer_id`.
    pub fn set_main_profile_tab(
        &mut self,
        peer_id: PeerId,
        tab: ProfileMainTab,
    ) -> Result<(), String> {
        self.main_profile_tabs.insert(peer_id, tab);
        Ok(())
    }

    /// Records a birthday suggestion for `user_id`.
    pub fn suggest_user_birthday(
        &mut self,
        user_id: PeerId,
        birthday: Birthday,
    ) -> Result<(), String> {
        if !(1..=12).contains(&birthday.month) || !(1..=31).contains(&birthday.day) {
            return Err("Enter a valid calendar date.".to_owned());
        }
        self.suggested_birthdays.push((user_id, birthday));
        Ok(())
    }

    /// Records a profile photo abuse report for `peer_id`.
    pub fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.reported_profile_photos
            .push((peer_id, photo, reason, comment.trim().to_owned()));
        Ok(())
    }

    /// Returns the linked personal channel for `peer_id`, if any.
    #[must_use]
    pub fn personal_channel(&self, peer_id: PeerId) -> Option<PersonalChannelOption> {
        self.personal_channels.get(&peer_id).cloned()
    }

    /// Sets or clears the linked personal channel for `peer_id`.
    pub fn set_personal_channel(
        &mut self,
        peer_id: PeerId,
        channel: Option<PersonalChannelOption>,
    ) {
        if let Some(ch) = channel {
            self.personal_channels.insert(peer_id, ch);
        } else {
            self.personal_channels.remove(&peer_id);
        }
    }

    /// Returns the TON/Stars balance pill data for `peer_id`.
    #[must_use]
    pub fn peer_profile_balance(&self, peer_id: PeerId) -> Option<ProfileBalanceInfo> {
        self.peer_balances.get(&peer_id).copied()
    }

    /// Returns whether channel/chat Star Gift notifications are enabled for `peer_id`.
    #[must_use]
    pub fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        self.chat_gift_notifications.get(&peer_id).copied()
    }

    /// Toggles channel/chat Star Gift notifications for `peer_id`.
    pub fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        self.chat_gift_notifications.insert(peer_id, enabled);
        Ok(enabled)
    }
}

fn slice_stories(all: Vec<ProfileStoryItem>, offset_id: i32, limit: i32) -> ProfileStoriesPage {
    let total_count = all.len() as i32;
    let pinned_to_top: Vec<i32> = all.iter().filter(|s| s.pinned).map(|s| s.id).collect();
    let lim = if limit <= 0 {
        all.len()
    } else {
        limit as usize
    };
    let stories: Vec<ProfileStoryItem> = all
        .into_iter()
        .filter(|s| offset_id <= 0 || s.id < offset_id)
        .take(lim)
        .collect();
    ProfileStoriesPage {
        total_count,
        stories,
        pinned_to_top,
    }
}

fn build_sample_received_gifts() -> Vec<ReceivedGiftItem> {
    let unique_star = UniqueGift {
        id: CollectibleId(770_001),
        initial_gift_id: 8001,
        slug: "AstralCompass-42".to_owned(),
        title: "Astral Compass".to_owned(),
        owner_name: "Mira Vance".to_owned(),
        stars_for_resale: 750,
        stars_for_transfer: 25,
        number: 42,
        can_be_theme: true,
        model: UniqueGiftModel {
            attribute: UniqueGiftAttribute {
                name: "Nebula Brass".to_owned(),
                rarity_value: 15,
            },
            document_id: DocumentId(701),
            crafted: false,
        },
        pattern: UniqueGiftPattern {
            attribute: UniqueGiftAttribute {
                name: "Star Chart".to_owned(),
                rarity_value: 40,
            },
            document_id: DocumentId(702),
        },
        backdrop: UniqueGiftBackdrop {
            attribute: UniqueGiftAttribute {
                name: "Midnight Indigo".to_owned(),
                rarity_value: 25,
            },
            id: 1,
            center_color: RgbColor::new(0x3C, 0x4E, 0x9C),
            edge_color: RgbColor::new(0x1D, 0x26, 0x59),
            pattern_color: RgbColor::new(0x93, 0xA8, 0xFF),
            text_color: RgbColor::new(0xFF, 0xFF, 0xFF),
        },
        ..UniqueGift::default()
    };

    vec![
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 8001,
                    stars: 500,
                    stars_to_upgrade: 25,
                    stars_converted: 425,
                    limited_count: 500,
                    limited_left: 0,
                    first_sale_date: MOCK_NOW - 86_400 * 60,
                    last_sale_date: MOCK_NOW - 86_400 * 10,
                    sticker_document_id: DocumentId(701),
                    sold_out: true,
                    upgradable: true,
                    unique: Some(unique_star),
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::User(DataMsgId(9001))),
                collection_ids: vec![101, 102],
                date: MOCK_NOW - 86_400 * 4,
                message: "For shipping the release!".to_owned(),
                stars_converted: 425,
                stars_upgraded_by_sender: 25,
                pinned: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Astral Compass #42".to_owned(),
            sender_name: "Taylor Kim".to_owned(),
            artwork_kind: "star".to_owned(),
        },
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 8002,
                    stars: 100,
                    stars_to_upgrade: 50,
                    stars_converted: 85,
                    limited_count: 2_000,
                    limited_left: 340,
                    first_sale_date: MOCK_NOW - 86_400 * 20,
                    sticker_document_id: DocumentId(702),
                    upgradable: true,
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::User(DataMsgId(9002))),
                collection_ids: vec![101],
                date: MOCK_NOW - 86_400 * 7,
                message: "Great presentation!".to_owned(),
                stars_converted: 85,
                upgradable: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Golden Trophy".to_owned(),
            sender_name: "Sam Chen".to_owned(),
            artwork_kind: "trophy".to_owned(),
        },
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 8003,
                    stars: 25,
                    stars_converted: 21,
                    sticker_document_id: DocumentId(703),
                    birthday: true,
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::User(DataMsgId(9003))),
                collection_ids: Vec::new(),
                date: MOCK_NOW - 86_400 * 14,
                message: "Happy launch day!".to_owned(),
                stars_converted: 21,
                anonymous: true,
                hidden: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Celebration Cake".to_owned(),
            sender_name: "Anonymous".to_owned(),
            artwork_kind: "cake".to_owned(),
        },
    ]
}
