//! Live [`BusinessBackend`] implementation for [`LiveSession`] over real Telegram TL RPCs.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/business/data_business_common.cpp`
//! - `Telegram/SourceFiles/data/business/data_business_info.cpp`
//! - `Telegram/SourceFiles/data/business/data_business_chatbots.cpp`
//! - `Telegram/SourceFiles/data/business/data_shortcut_messages.cpp`
//! - `Telegram/SourceFiles/api/api_chat_links.cpp`
//! - `Telegram/SourceFiles/data/data_emoji_statuses.cpp`
//! - `Telegram/SourceFiles/boxes/premium_limits_box.cpp`

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_data::{ChannelId, Peer, PeerId as DataPeerId, Session, UserId as DataUserId};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, format_short_time, known_peer_ids, now_unix_i32, to_data_peer,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::business::backend::BusinessBackend;
use tg_app_core::business::model::{
    ActiveEmojiStatus, AwaySchedule, AwayScheduleType, AwaySettings, BotLookupState,
    BotSearchResult, BusinessBotCandidate, BusinessChatLink, BusinessChatTypes, BusinessChats,
    BusinessDetails, BusinessLocation, BusinessOverviewSnapshot, BusinessRecipients,
    BusinessShortcutId, BusinessUserRef, CHAT_INTRO_DESCRIPTION_LIMIT, CHAT_INTRO_TITLE_LIMIT,
    CHAT_LINKS_LIMIT, ChatIntro, ChatbotsPermissions, ChatbotsSettings, EmojiStatusCategory,
    EmojiStatusDuration, EmojiStatusItem, GeoPoint, GreetingSettings, InactiveChannelEntry,
    IntroStickerOption, MAX_SEARCH_BOTS, PremiumLimitsData, PublicLinkChannelEntry,
    QUICK_REPLIES_LIMIT, QuickReplyAttachment, QuickReplyMessage, QuickReplyShortcut,
    SHORTCUT_MESSAGES_LIMIT, Timezone, WorkingHours, WorkingInterval, WorkingIntervals,
    parse_bot_username_query, valid_shortcut_name,
};
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for synchronous business RPC invocations.
const BUSINESS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Cached per-session state for Telegram Business operations so clones of [`LiveSession`]
/// share state without altering `LiveSession`'s struct layout.
#[derive(Default)]
struct BusinessLiveState {
    /// Cached timezones hash for `help.getTimezonesList`.
    timezones_hash: i32,
    /// Cached server timezones.
    timezones: Vec<Timezone>,
    /// Cached connected bot (needed when disconnecting via `account.updateConnectedBot`).
    connected_bot: Option<BusinessBotCandidate>,
    /// Cached quick-reply shortcuts keyed by ID.
    shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcut>,
    /// Local-only shortcut IDs that have not yet been persisted via a message send.
    next_local_shortcut_id: BusinessShortcutId,
    /// Cached `InputDocument` metadata `(access_hash, file_reference)` for intro stickers.
    intro_stickers_meta: HashMap<u64, (i64, Vec<u8>)>,
    /// Cached business chat links.
    chat_links: Vec<BusinessChatLink>,
    /// Monotonic counter for `random_id` generation.
    random_seq: u64,
}

impl BusinessLiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.random_seq = self.random_seq.wrapping_add(1);
        ((now << 24) ^ self.random_seq ^ 0x4249_5A51_7711) as i64
    }

    fn alloc_local_shortcut_id(&mut self) -> BusinessShortcutId {
        if self.next_local_shortcut_id == 0 {
            self.next_local_shortcut_id = -1;
        }
        let id = self.next_local_shortcut_id;
        self.next_local_shortcut_id -= 1;
        id
    }
}

struct BusinessStateEntry {
    owner: Weak<LiveBridgeState>,
    state: BusinessLiveState,
}

fn business_state_registry() -> &'static Mutex<HashMap<usize, BusinessStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, BusinessStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_business_state<R>(live: &LiveSession, f: impl FnOnce(&mut BusinessLiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = business_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| BusinessStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: BusinessLiveState::default(),
    });
    f(&mut entry.state)
}

// -----------------------------------------------------------------------------
// Internal RPC & Session helpers
// -----------------------------------------------------------------------------

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: tg_tl::RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(BUSINESS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn input_user_for_id(session: &Session, user_id: i64) -> tl_enums::InputUser {
    let uid = DataUserId::from_tl(user_id);
    let resolved = input_user_for(session, uid);
    if matches!(resolved, tl_enums::InputUser::Empty) && user_id > 0 {
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id,
            access_hash: 0,
        })
    } else {
        resolved
    }
}

fn input_channel_for_id(session: &Session, channel_id: i64) -> Option<tl_enums::InputChannel> {
    let cid = ChannelId::from_tl(channel_id);
    let peer = session.peer(DataPeerId::from_channel(cid))?;
    let ch = peer.as_channel()?;
    Some(tl_enums::InputChannel::InputChannel(
        tl_types::InputChannel {
            channel_id: ch.channel_id().0 as i64,
            access_hash: ch.access_hash as i64,
        },
    ))
}

// -----------------------------------------------------------------------------
// TL <-> business::model conversions (exact port of data_business_common.cpp)
// -----------------------------------------------------------------------------

/// Converts [`WorkingHours`] into TL `BusinessWorkHours` (`seconds / 60` minutes).
pub(crate) fn working_hours_to_tl(hours: &WorkingHours) -> tl_enums::BusinessWorkHours {
    let normalized = hours.intervals.normalized();
    let weekly_open = normalized
        .list
        .into_iter()
        .map(|iv| {
            tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(tl_types::BusinessWeeklyOpen {
                start_minute: iv.start / 60,
                end_minute: iv.end / 60,
            })
        })
        .collect();
    tl_enums::BusinessWorkHours::BusinessWorkHours(tl_types::BusinessWorkHours {
        open_now: false,
        timezone_id: hours.timezone_id.clone(),
        weekly_open,
    })
}

/// Converts TL `BusinessWorkHours` into [`WorkingHours`] (`minutes * 60` seconds).
pub(crate) fn working_hours_from_tl(tl: &tl_enums::BusinessWorkHours) -> WorkingHours {
    let tl_enums::BusinessWorkHours::BusinessWorkHours(data) = tl;
    let list = data
        .weekly_open
        .iter()
        .map(|tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(open)| {
            WorkingInterval::new(open.start_minute * 60, open.end_minute * 60)
        })
        .collect();
    WorkingHours {
        intervals: WorkingIntervals::new(list).normalized(),
        timezone_id: data.timezone_id.clone(),
    }
}

/// Converts TL `BusinessLocation` into [`BusinessLocation`].
pub(crate) fn business_location_from_tl(tl: &tl_enums::BusinessLocation) -> BusinessLocation {
    let tl_enums::BusinessLocation::BusinessLocation(loc) = tl;
    let point = loc.geo_point.as_ref().and_then(|gp| match gp {
        tl_enums::GeoPoint::GeoPoint(p) => Some(GeoPoint {
            lat: p.lat,
            lon: p.long,
        }),
        tl_enums::GeoPoint::Empty => None,
    });
    BusinessLocation {
        address: loc.address.clone(),
        point,
    }
}

/// Converts TL `BusinessIntro` into [`ChatIntro`] and optional `(doc_id, access_hash, file_ref)`.
pub(crate) fn business_intro_from_tl(
    tl: &tl_enums::BusinessIntro,
) -> (ChatIntro, Option<(u64, i64, Vec<u8>)>) {
    let tl_enums::BusinessIntro::BusinessIntro(intro) = tl;
    let (sticker_id, meta) = match intro.sticker.as_ref() {
        Some(tl_enums::Document::Document(doc)) => (
            Some(doc.id as u64),
            Some((doc.id as u64, doc.access_hash, doc.file_reference.clone())),
        ),
        Some(tl_enums::Document::Empty(doc)) => (Some(doc.id as u64), None),
        None => (None, None),
    };
    (
        ChatIntro {
            title: intro.title.clone(),
            description: intro.description.clone(),
            sticker_id,
        },
        meta,
    )
}

/// Converts [`BusinessRecipients`] into TL `InputBusinessRecipients` (`ForMessagesToMTP`).
pub(crate) fn recipients_for_messages_to_tl(
    recipients: &BusinessRecipients,
    session: &Session,
) -> tl_enums::InputBusinessRecipients {
    let chats = if recipients.all_but_excluded {
        &recipients.excluded
    } else {
        &recipients.included
    };
    let users: Vec<tl_enums::InputUser> = chats
        .user_ids
        .iter()
        .map(|&uid| input_user_for_id(session, uid))
        .collect();
    tl_enums::InputBusinessRecipients::InputBusinessRecipients(tl_types::InputBusinessRecipients {
        existing_chats: chats.types.contains(BusinessChatTypes::EXISTING_CHATS),
        new_chats: chats.types.contains(BusinessChatTypes::NEW_CHATS),
        contacts: chats.types.contains(BusinessChatTypes::CONTACTS),
        non_contacts: chats.types.contains(BusinessChatTypes::NON_CONTACTS),
        exclude_selected: recipients.all_but_excluded,
        users: (!users.is_empty()).then_some(users),
    })
}

/// Converts TL `BusinessRecipients` into [`BusinessRecipients`].
pub(crate) fn recipients_from_tl(tl: &tl_enums::BusinessRecipients) -> BusinessRecipients {
    let tl_enums::BusinessRecipients::BusinessRecipients(data) = tl;
    let mut types = BusinessChatTypes::EMPTY;
    if data.new_chats {
        types = types.union(BusinessChatTypes::NEW_CHATS);
    }
    if data.existing_chats {
        types = types.union(BusinessChatTypes::EXISTING_CHATS);
    }
    if data.contacts {
        types = types.union(BusinessChatTypes::CONTACTS);
    }
    if data.non_contacts {
        types = types.union(BusinessChatTypes::NON_CONTACTS);
    }
    let chats = BusinessChats {
        types,
        user_ids: data.users.clone().unwrap_or_default(),
    };
    if data.exclude_selected {
        BusinessRecipients {
            included: BusinessChats::default(),
            excluded: chats,
            all_but_excluded: true,
        }
    } else {
        BusinessRecipients {
            included: chats,
            excluded: BusinessChats::default(),
            all_but_excluded: false,
        }
    }
}

/// Converts [`BusinessRecipients`] into TL `InputBusinessBotRecipients` (`ForBotsToMTP`).
pub(crate) fn recipients_for_bots_to_tl(
    recipients: &BusinessRecipients,
    session: &Session,
) -> tl_enums::InputBusinessBotRecipients {
    let chats = if recipients.all_but_excluded {
        &recipients.excluded
    } else {
        &recipients.included
    };
    let users: Vec<tl_enums::InputUser> = chats
        .user_ids
        .iter()
        .map(|&uid| input_user_for_id(session, uid))
        .collect();
    let exclude_users: Vec<tl_enums::InputUser> =
        if recipients.all_but_excluded || recipients.excluded.user_ids.is_empty() {
            Vec::new()
        } else {
            recipients
                .excluded
                .user_ids
                .iter()
                .map(|&uid| input_user_for_id(session, uid))
                .collect()
        };
    tl_enums::InputBusinessBotRecipients::InputBusinessBotRecipients(
        tl_types::InputBusinessBotRecipients {
            existing_chats: chats.types.contains(BusinessChatTypes::EXISTING_CHATS),
            new_chats: chats.types.contains(BusinessChatTypes::NEW_CHATS),
            contacts: chats.types.contains(BusinessChatTypes::CONTACTS),
            non_contacts: chats.types.contains(BusinessChatTypes::NON_CONTACTS),
            exclude_selected: recipients.all_but_excluded,
            users: (!users.is_empty()).then_some(users),
            exclude_users: (!exclude_users.is_empty()).then_some(exclude_users),
        },
    )
}

/// Converts TL `BusinessBotRecipients` into [`BusinessRecipients`].
pub(crate) fn bot_recipients_from_tl(tl: &tl_enums::BusinessBotRecipients) -> BusinessRecipients {
    let tl_enums::BusinessBotRecipients::BusinessBotRecipients(data) = tl;
    let mut types = BusinessChatTypes::EMPTY;
    if data.new_chats {
        types = types.union(BusinessChatTypes::NEW_CHATS);
    }
    if data.existing_chats {
        types = types.union(BusinessChatTypes::EXISTING_CHATS);
    }
    if data.contacts {
        types = types.union(BusinessChatTypes::CONTACTS);
    }
    if data.non_contacts {
        types = types.union(BusinessChatTypes::NON_CONTACTS);
    }
    let chats = BusinessChats {
        types,
        user_ids: data.users.clone().unwrap_or_default(),
    };
    if data.exclude_selected {
        BusinessRecipients {
            included: BusinessChats::default(),
            excluded: chats,
            all_but_excluded: true,
        }
    } else {
        BusinessRecipients {
            included: chats,
            excluded: BusinessChats {
                types: BusinessChatTypes::EMPTY,
                user_ids: data.exclude_users.clone().unwrap_or_default(),
            },
            all_but_excluded: false,
        }
    }
}

/// Converts [`ChatbotsPermissions`] into TL `BusinessBotRights`.
pub(crate) fn bot_rights_to_tl(perms: ChatbotsPermissions) -> tl_enums::BusinessBotRights {
    tl_enums::BusinessBotRights::BusinessBotRights(tl_types::BusinessBotRights {
        reply: perms.contains(ChatbotsPermissions::REPLY_TO_MESSAGES),
        read_messages: perms.contains(ChatbotsPermissions::MARK_AS_READ),
        delete_sent_messages: perms.contains(ChatbotsPermissions::DELETE_SENT),
        delete_received_messages: perms.contains(ChatbotsPermissions::DELETE_RECEIVED),
        edit_name: perms.contains(ChatbotsPermissions::EDIT_NAME),
        edit_bio: perms.contains(ChatbotsPermissions::EDIT_BIO),
        edit_profile_photo: perms.contains(ChatbotsPermissions::EDIT_USERPIC),
        edit_username: perms.contains(ChatbotsPermissions::EDIT_USERNAME),
        view_gifts: perms.contains(ChatbotsPermissions::VIEW_GIFTS),
        sell_gifts: perms.contains(ChatbotsPermissions::SELL_GIFTS),
        change_gift_settings: perms.contains(ChatbotsPermissions::GIFT_SETTINGS),
        transfer_and_upgrade_gifts: perms.contains(ChatbotsPermissions::TRANSFER_GIFTS),
        transfer_stars: perms.contains(ChatbotsPermissions::TRANSFER_STARS),
        manage_stories: perms.contains(ChatbotsPermissions::MANAGE_STORIES),
    })
}

/// Converts TL `BusinessBotRights` into [`ChatbotsPermissions`].
pub(crate) fn bot_rights_from_tl(tl: &tl_enums::BusinessBotRights) -> ChatbotsPermissions {
    let tl_enums::BusinessBotRights::BusinessBotRights(r) = tl;
    let mut perms = ChatbotsPermissions::VIEW_MESSAGES;
    perms.set(ChatbotsPermissions::REPLY_TO_MESSAGES, r.reply);
    perms.set(ChatbotsPermissions::MARK_AS_READ, r.read_messages);
    perms.set(ChatbotsPermissions::DELETE_SENT, r.delete_sent_messages);
    perms.set(
        ChatbotsPermissions::DELETE_RECEIVED,
        r.delete_received_messages,
    );
    perms.set(ChatbotsPermissions::EDIT_NAME, r.edit_name);
    perms.set(ChatbotsPermissions::EDIT_BIO, r.edit_bio);
    perms.set(ChatbotsPermissions::EDIT_USERPIC, r.edit_profile_photo);
    perms.set(ChatbotsPermissions::EDIT_USERNAME, r.edit_username);
    perms.set(ChatbotsPermissions::VIEW_GIFTS, r.view_gifts);
    perms.set(ChatbotsPermissions::SELL_GIFTS, r.sell_gifts);
    perms.set(ChatbotsPermissions::GIFT_SETTINGS, r.change_gift_settings);
    perms.set(
        ChatbotsPermissions::TRANSFER_GIFTS,
        r.transfer_and_upgrade_gifts,
    );
    perms.set(ChatbotsPermissions::TRANSFER_STARS, r.transfer_stars);
    perms.set(ChatbotsPermissions::MANAGE_STORIES, r.manage_stories);
    perms
}

/// Converts TL `BusinessGreetingMessage` into [`GreetingSettings`].
pub(crate) fn greeting_from_tl(tl: &tl_enums::BusinessGreetingMessage) -> GreetingSettings {
    let tl_enums::BusinessGreetingMessage::BusinessGreetingMessage(g) = tl;
    GreetingSettings {
        recipients: recipients_from_tl(&g.recipients),
        no_activity_days: g.no_activity_days,
        shortcut_id: g.shortcut_id,
    }
}

/// Converts TL `BusinessAwayMessage` into [`AwaySettings`].
pub(crate) fn away_from_tl(tl: &tl_enums::BusinessAwayMessage) -> AwaySettings {
    let tl_enums::BusinessAwayMessage::BusinessAwayMessage(a) = tl;
    let schedule = match &a.schedule {
        tl_enums::BusinessAwayMessageSchedule::Always => AwaySchedule {
            schedule_type: AwayScheduleType::Always,
            custom_interval: WorkingInterval::default(),
        },
        tl_enums::BusinessAwayMessageSchedule::OutsideWorkHours => AwaySchedule {
            schedule_type: AwayScheduleType::OutsideWorkingHours,
            custom_interval: WorkingInterval::default(),
        },
        tl_enums::BusinessAwayMessageSchedule::Custom(c) => AwaySchedule {
            schedule_type: AwayScheduleType::Custom,
            custom_interval: WorkingInterval::new(c.start_date, c.end_date),
        },
    };
    AwaySettings {
        recipients: recipients_from_tl(&a.recipients),
        schedule,
        shortcut_id: a.shortcut_id,
        offline_only: a.offline_only,
    }
}

/// Converts TL `BusinessChatLink` into [`BusinessChatLink`].
pub(crate) fn chat_link_from_tl(tl: &tl_enums::BusinessChatLink) -> BusinessChatLink {
    let tl_enums::BusinessChatLink::BusinessChatLink(l) = tl;
    BusinessChatLink {
        link: l.link.clone(),
        title: l.title.clone().unwrap_or_default(),
        message: l.message.clone(),
        clicks: l.views.max(0) as u32,
    }
}

fn format_inactivity_label(last_active_unix: i32, now: i32) -> String {
    let delta = now.saturating_sub(last_active_unix).max(0);
    let days = delta / 86_400;
    if days >= 60 {
        format!("inactive for {} months", days / 30)
    } else if days >= 30 {
        "inactive for 1 month".to_owned()
    } else if days >= 14 {
        format!("inactive for {} weeks", days / 7)
    } else if days > 0 {
        format!("inactive for {days} days")
    } else {
        "inactive recently".to_owned()
    }
}

fn user_to_bot_candidate(user: &tl_types::User, about: String) -> BusinessBotCandidate {
    let name = match (&user.first_name, &user.last_name) {
        (Some(f), Some(l)) if !l.is_empty() => format!("{f} {l}"),
        (Some(f), _) => f.clone(),
        (None, Some(l)) => l.clone(),
        (None, None) => format!("Bot {}", user.id),
    };
    BusinessBotCandidate {
        id: user.id,
        name,
        username: user.username.clone().unwrap_or_default(),
        about,
        supports_business: user.bot_business,
        avatar_rgb: avatar_rgb_for_color_index((user.id.unsigned_abs() % 7) as u8),
    }
}

fn emoji_status_from_tl(tl: Option<&tl_enums::EmojiStatus>, now: i32) -> ActiveEmojiStatus {
    match tl {
        Some(tl_enums::EmojiStatus::EmojiStatus(s)) => {
            let duration = match s.until {
                None | Some(0) => EmojiStatusDuration::Forever,
                Some(until) => {
                    let rem = until.saturating_sub(now);
                    if rem <= 3_600 {
                        EmojiStatusDuration::OneHour
                    } else if rem <= 7_200 {
                        EmojiStatusDuration::TwoHours
                    } else if rem <= 28_800 {
                        EmojiStatusDuration::EightHours
                    } else {
                        EmojiStatusDuration::TwoDays
                    }
                }
            };
            ActiveEmojiStatus {
                status_id: Some(s.document_id as u64),
                duration,
            }
        }
        Some(tl_enums::EmojiStatus::Collectible(c)) => ActiveEmojiStatus {
            status_id: Some(c.document_id as u64),
            duration: EmojiStatusDuration::Forever,
        },
        Some(tl_enums::EmojiStatus::Empty)
        | Some(tl_enums::EmojiStatus::InputEmojiStatusCollectible(_))
        | None => ActiveEmojiStatus::default(),
    }
}

/// Converts a `tg_data::Message` stored in `Session::shortcut_messages` into [`QuickReplyMessage`].
fn quick_reply_message_from_data(msg: &tg_data::Message) -> QuickReplyMessage {
    let remote_id = tg_data::shortcut_local_to_remote_id(msg.id).unwrap_or(msg.id);
    let attachment = match msg.media() {
        Some(tg_data::MessageMedia::Photo(_)) => QuickReplyAttachment::Photo,
        Some(tg_data::MessageMedia::Document(doc)) => {
            if doc.kind == tg_data::DocumentKind::Voice {
                QuickReplyAttachment::Voice
            } else {
                QuickReplyAttachment::Document
            }
        }
        _ => QuickReplyAttachment::None,
    };
    QuickReplyMessage {
        id: remote_id.0 as i32,
        text: msg.text().map(|t| t.text.clone()).unwrap_or_default(),
        attachment,
        time_label: format_short_time(msg.date),
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn collect_shortcuts_from_session(
    session: &Session,
) -> BTreeMap<BusinessShortcutId, QuickReplyShortcut> {
    let store = session.shortcut_messages();
    let mut map = BTreeMap::new();
    for (&sc_id, sc) in store.shortcuts() {
        let mut messages: Vec<QuickReplyMessage> = store
            .messages(sc_id)
            .iter()
            .map(quick_reply_message_from_data)
            .collect();
        messages.sort_by_key(|m| m.id);
        map.insert(
            sc_id,
            QuickReplyShortcut {
                id: sc_id,
                name: sc.name.clone(),
                messages,
            },
        );
    }
    map
}

// -----------------------------------------------------------------------------
// Additional live Quick Reply helpers exposed on LiveSession
// -----------------------------------------------------------------------------

impl LiveSession {
    /// Fetches Business quick-reply shortcuts via `messages.getQuickReplies`
    /// (`ShortcutMessages::preloadShortcuts`, `data/business/data_shortcut_messages.cpp:460-479`),
    /// sending `Session::shortcut_messages().shortcuts_hash()` and loading full message lists
    /// via [`Self::load_quick_reply_messages`] when a shortcut reports more messages than its
    /// inline top preview.
    pub fn load_quick_replies(
        &self,
    ) -> Result<BTreeMap<BusinessShortcutId, QuickReplyShortcut>, String> {
        let hash = read_session(self, |s| s.shortcut_messages().shortcuts_hash());
        let resp = invoke_rpc(self, tl_fns::messages::GetQuickReplies { hash })?;
        let shortcuts_needing_full: Vec<BusinessShortcutId> = match resp {
            tl_enums::messages::QuickReplies::NotModified => {
                write_session(self, |s| {
                    s.shortcut_messages_mut().mark_shortcuts_loaded();
                });
                Vec::new()
            }
            tl_enums::messages::QuickReplies::QuickReplies(data) => {
                write_session(self, |session| {
                    apply_tl_users_and_chats(session, &data.users, &data.chats);
                    let shortcuts: Vec<tg_data::QuickReplyShortcutData> = data
                        .quick_replies
                        .iter()
                        .map(tg_api::scheduled::quick_reply_from_tl)
                        .collect();
                    session.shortcut_messages_mut().update_shortcuts(shortcuts);
                    let self_id = session.self_user_id();
                    for raw in &data.messages {
                        if let Some(msg) = tg_api::convert::message_from_tl(raw, self_id)
                            && let Some(sc_id) = msg.shortcut_id
                        {
                            let _ = session
                                .shortcut_messages_mut()
                                .upsert_preview_message(sc_id, msg);
                        }
                    }
                    let store = session.shortcut_messages();
                    store
                        .shortcuts()
                        .iter()
                        .filter_map(|(&id, sc)| {
                            (sc.count.max(0) as usize > store.messages(id).len()).then_some(id)
                        })
                        .collect()
                })
            }
        };

        let mut map = read_session(self, collect_shortcuts_from_session);
        for sc_id in shortcuts_needing_full {
            if let Ok(full_msgs) = self.load_quick_reply_messages(sc_id)
                && !full_msgs.is_empty()
                && let Some(sc) = map.get_mut(&sc_id)
            {
                sc.messages = full_msgs;
            }
        }
        for sc in map.values_mut() {
            sc.messages.sort_by_key(|m| m.id);
        }
        with_business_state(self, |st| {
            st.shortcuts = map.clone();
        });
        Ok(map)
    }

    /// Fetches all messages belonging to `shortcut_id` via
    /// `tg_api::scheduled::get_quick_reply_messages` (`ShortcutMessages::request` /
    /// `countListHash`, `data/business/data_shortcut_messages.cpp:602-660, 763-781`).
    pub fn load_quick_reply_messages(
        &self,
        shortcut_id: BusinessShortcutId,
    ) -> Result<Vec<QuickReplyMessage>, String> {
        self.load_quick_reply_messages_with_clock(shortcut_id, now_unix_ms(), false)
    }

    /// Clock-injectable variant of [`Self::load_quick_reply_messages`] supporting the
    /// 60-second `SHORTCUT_REQUEST_TIME_LIMIT_MS` cooldown and exact `ShortcutMessages::countListHash`.
    pub fn load_quick_reply_messages_with_clock(
        &self,
        shortcut_id: BusinessShortcutId,
        now_ms: u64,
        force: bool,
    ) -> Result<Vec<QuickReplyMessage>, String> {
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let res = block_on_runtime(self.runtime().tokio_handle(), async move {
            tokio::time::timeout(BUSINESS_RPC_TIMEOUT, async move {
                let mut session = session_arc.write().await;
                tg_api::scheduled::get_quick_reply_messages(
                    &rpc,
                    &mut session,
                    shortcut_id,
                    now_ms,
                    force,
                )
                .await
            })
            .await
        });
        match res {
            Ok(Ok(_outcome)) => {
                let mut parsed: Vec<QuickReplyMessage> = read_session(self, |s| {
                    s.shortcut_messages()
                        .messages(shortcut_id)
                        .iter()
                        .map(quick_reply_message_from_data)
                        .collect()
                });
                parsed.sort_by_key(|m| m.id);
                with_business_state(self, |st| {
                    if let Some(sc) = st.shortcuts.get_mut(&shortcut_id) {
                        sc.messages = parsed.clone();
                    }
                });
                Ok(parsed)
            }
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("Timed out loading quick reply messages.".to_owned()),
        }
    }

    /// Sends selected quick-reply messages to `peer_id` via `messages.sendQuickReplyMessages`.
    pub fn send_quick_reply_messages(
        &self,
        peer_id: PeerId,
        shortcut_id: BusinessShortcutId,
        message_ids: &[i32],
    ) -> Result<(), String> {
        if message_ids.is_empty() {
            return Err("No quick reply message IDs provided.".to_owned());
        }
        let data_peer = to_data_peer(peer_id);
        let input_peer = read_session(self, |s| input_peer_for(s, data_peer));
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Target peer not found in session.".to_owned());
        }
        let random_id: Vec<i64> = with_business_state(self, |st| {
            message_ids.iter().map(|_| st.next_random_id()).collect()
        });
        let updates = invoke_rpc(
            self,
            tl_fns::messages::SendQuickReplyMessages {
                peer: input_peer,
                shortcut_id,
                id: message_ids.to_vec(),
                random_id,
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// BusinessBackend implementation for LiveSession
// -----------------------------------------------------------------------------

impl BusinessBackend for LiveSession {
    fn load_overview(&self) -> Result<BusinessOverviewSnapshot, String> {
        let now = now_unix_i32();

        // 1. Fetch full self user (`users.getFullUser`) for work hours, location, intro,
        //    greeting/away settings, sponsored toggle, and active emoji status.
        let tl_enums::users::UserFull::UserFull(full_resp) = invoke_rpc(
            self,
            tl_fns::users::GetFullUser {
                id: tl_enums::InputUser::Self_,
            },
        )?;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &full_resp.users, &full_resp.chats);
        });
        let tl_enums::UserFull::UserFull(ref full) = full_resp.full_user;

        let hours = full
            .business_work_hours
            .as_ref()
            .map(working_hours_from_tl)
            .unwrap_or_default();
        let location = full
            .business_location
            .as_ref()
            .map(business_location_from_tl)
            .unwrap_or_default();
        let (intro, sticker_meta) = full
            .business_intro
            .as_ref()
            .map(business_intro_from_tl)
            .unwrap_or_default();
        if let Some((doc_id, access_hash, file_ref)) = sticker_meta {
            with_business_state(self, |st| {
                st.intro_stickers_meta
                    .insert(doc_id, (access_hash, file_ref));
            });
        }
        let greeting = full
            .business_greeting_message
            .as_ref()
            .map(greeting_from_tl)
            .unwrap_or_default();
        let away = full
            .business_away_message
            .as_ref()
            .map(away_from_tl)
            .unwrap_or_default();
        let sponsored_disabled = !full.sponsored_enabled;

        let self_tl_user = full_resp.users.iter().find_map(|u| match u {
            tl_enums::User::User(inner) if inner.id == full.id => Some(inner),
            _ => None,
        });
        let emoji_status =
            emoji_status_from_tl(self_tl_user.and_then(|u| u.emoji_status.as_ref()), now);

        // 2. Timezones (`help.getTimezonesList`).
        let tz_hash = with_business_state(self, |st| st.timezones_hash);
        let timezones = match invoke_rpc(self, tl_fns::help::GetTimezonesList { hash: tz_hash }) {
            Ok(tl_enums::help::TimezonesList::TimezonesList(list)) => {
                let parsed: Vec<Timezone> = list
                    .timezones
                    .into_iter()
                    .map(|tl_enums::Timezone::Timezone(t)| Timezone {
                        id: t.id,
                        name: t.name,
                        utc_offset: t.utc_offset,
                    })
                    .collect();
                with_business_state(self, |st| {
                    st.timezones_hash = list.hash;
                    st.timezones = parsed.clone();
                });
                parsed
            }
            Ok(tl_enums::help::TimezonesList::NotModified) => {
                with_business_state(self, |st| st.timezones.clone())
            }
            Err(err) => {
                debug!(error = %err, "help.getTimezonesList failed");
                with_business_state(self, |st| st.timezones.clone())
            }
        };

        // 3. Connected chatbots (`account.getConnectedBots`).
        let chatbots = match invoke_rpc(self, tl_fns::account::GetConnectedBots {}) {
            Ok(tl_enums::account::ConnectedBots::ConnectedBots(resp)) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &resp.users, &[]);
                });
                if let Some(tl_enums::ConnectedBot::ConnectedBot(cb)) = resp.connected_bots.first()
                {
                    let bot = resp
                        .users
                        .iter()
                        .find_map(|u| match u {
                            tl_enums::User::User(inner) if inner.id == cb.bot_id => {
                                Some(user_to_bot_candidate(inner, String::new()))
                            }
                            _ => None,
                        })
                        .or_else(|| {
                            Some(BusinessBotCandidate {
                                id: cb.bot_id,
                                name: format!("Bot {}", cb.bot_id),
                                username: String::new(),
                                about: String::new(),
                                supports_business: true,
                                avatar_rgb: avatar_rgb_for_color_index(
                                    (cb.bot_id.unsigned_abs() % 7) as u8,
                                ),
                            })
                        });
                    with_business_state(self, |st| {
                        st.connected_bot = bot.clone();
                    });
                    ChatbotsSettings {
                        bot,
                        recipients: bot_recipients_from_tl(&cb.recipients),
                        permissions: bot_rights_from_tl(&cb.rights),
                    }
                } else {
                    with_business_state(self, |st| {
                        st.connected_bot = None;
                    });
                    ChatbotsSettings::default()
                }
            }
            Err(err) => {
                debug!(error = %err, "account.getConnectedBots failed");
                ChatbotsSettings::default()
            }
        };

        // 4. Quick replies (`messages.getQuickReplies` + `messages.getQuickReplyMessages`).
        let shortcuts = match self.load_quick_replies() {
            Ok(map) => map,
            Err(err) => {
                debug!(error = %err, "messages.getQuickReplies failed");
                with_business_state(self, |st| st.shortcuts.clone())
            }
        };

        // 5. Business chat links (`account.getBusinessChatLinks`).
        let chat_links = match invoke_rpc(self, tl_fns::account::GetBusinessChatLinks {}) {
            Ok(tl_enums::account::BusinessChatLinks::BusinessChatLinks(resp)) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &resp.users, &resp.chats);
                });
                let links: Vec<BusinessChatLink> =
                    resp.links.iter().map(chat_link_from_tl).collect();
                with_business_state(self, |st| {
                    st.chat_links = links.clone();
                });
                links
            }
            Err(err) => {
                debug!(error = %err, "account.getBusinessChatLinks failed");
                with_business_state(self, |st| st.chat_links.clone())
            }
        };

        // 6. Inactive channels (`channels.getInactiveChannels`).
        let inactive_channels = match invoke_rpc(self, tl_fns::channels::GetInactiveChannels {}) {
            Ok(tl_enums::messages::InactiveChats::InactiveChats(resp)) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &resp.users, &resp.chats);
                });
                resp.chats
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, chat)| {
                        let date = resp.dates.get(idx).copied().unwrap_or(now);
                        match chat {
                            tl_enums::Chat::Channel(ch) => Some(InactiveChannelEntry {
                                id: ch.id,
                                title: ch.title.clone(),
                                is_broadcast: ch.broadcast,
                                inactive_since: format_inactivity_label(date, now),
                                avatar_rgb: avatar_rgb_for_color_index(
                                    (ch.id.unsigned_abs() % 7) as u8,
                                ),
                            }),
                            tl_enums::Chat::Chat(c) => Some(InactiveChannelEntry {
                                id: c.id,
                                title: c.title.clone(),
                                is_broadcast: false,
                                inactive_since: format_inactivity_label(date, now),
                                avatar_rgb: avatar_rgb_for_color_index(
                                    (c.id.unsigned_abs() % 7) as u8,
                                ),
                            }),
                            _ => None,
                        }
                    })
                    .collect()
            }
            Err(err) => {
                debug!(error = %err, "channels.getInactiveChannels failed");
                Vec::new()
            }
        };

        // 7. Admined public channels (`channels.getAdminedPublicChannels`).
        let public_link_channels = match invoke_rpc(
            self,
            tl_fns::channels::GetAdminedPublicChannels {
                by_location: false,
                check_limit: false,
                for_personal: false,
                for_community_peer: false,
            },
        ) {
            Ok(resp) => {
                let chats = match resp {
                    tl_enums::messages::Chats::Chats(c) => c.chats,
                    tl_enums::messages::Chats::Slice(c) => c.chats,
                };
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &[], &chats);
                });
                chats
                    .into_iter()
                    .filter_map(|chat| match chat {
                        tl_enums::Chat::Channel(ch) => {
                            let uname = ch.username.unwrap_or_default();
                            if uname.is_empty() {
                                return None;
                            }
                            Some(PublicLinkChannelEntry {
                                id: ch.id,
                                title: ch.title,
                                public_link: format!("t.me/{uname}"),
                                avatar_rgb: avatar_rgb_for_color_index(
                                    (ch.id.unsigned_abs() % 7) as u8,
                                ),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        // 8. Default emoji status catalog (`account.getDefaultEmojiStatuses`).
        let emoji_statuses =
            match invoke_rpc(self, tl_fns::account::GetDefaultEmojiStatuses { hash: 0 }) {
                Ok(tl_enums::account::EmojiStatuses::EmojiStatuses(es)) => es
                    .statuses
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, s)| match s {
                        tl_enums::EmojiStatus::EmojiStatus(st) => Some(EmojiStatusItem {
                            id: st.document_id as u64,
                            name: format!("Status #{}", i + 1),
                            glyph: "★".to_owned(),
                            category: EmojiStatusCategory::Default,
                            color_rgb: avatar_rgb_for_color_index((i % 7) as u8),
                            accent_rgb: avatar_rgb_for_color_index(((i + 2) % 7) as u8),
                            is_collectible: false,
                            collectible_slug: None,
                        }),
                        tl_enums::EmojiStatus::Collectible(c) => Some(EmojiStatusItem {
                            id: c.document_id as u64,
                            name: c.title,
                            glyph: "◆".to_owned(),
                            category: EmojiStatusCategory::Collectibles,
                            color_rgb: avatar_rgb_for_color_index((i % 7) as u8),
                            accent_rgb: avatar_rgb_for_color_index(((i + 3) % 7) as u8),
                            is_collectible: true,
                            collectible_slug: Some(c.slug),
                        }),
                        tl_enums::EmojiStatus::Empty
                        | tl_enums::EmojiStatus::InputEmojiStatusCollectible(_) => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };

        // 9. Account summary & available contacts from local `Session`.
        let (account_name, account_username, account_phone_display, is_premium, available_users) =
            read_session(self, |s| {
                let self_id = s.self_peer_id();
                let (name, username, phone, prem) = match s.peer(self_id) {
                    Some(Peer::User(u)) => (
                        u.common.name.clone(),
                        u.usernames.username().to_owned(),
                        u.phone.clone(),
                        u.is_premium(),
                    ),
                    _ => (String::new(), String::new(), String::new(), false),
                };
                let users: Vec<BusinessUserRef> = known_peer_ids(s)
                    .into_iter()
                    .filter(|&pid| pid != self_id)
                    .filter_map(|pid| {
                        let u = s.peer(pid)?.as_user()?;
                        Some(BusinessUserRef {
                            id: u.user_id().0 as i64,
                            name: u.common.name.clone(),
                            username: u.usernames.username().to_owned(),
                            avatar_rgb: avatar_rgb_for_color_index(u.common.color_index % 7),
                            is_contact: u.is_contact(),
                        })
                    })
                    .collect();
                (name, username, phone, prem, users)
            });

        let intro_stickers = intro
            .sticker_id
            .map(|id| {
                vec![IntroStickerOption {
                    id,
                    name: format!("Sticker #{id}"),
                    art_kind: "sticker".to_owned(),
                    accent_rgb: avatar_rgb_for_color_index((id % 7) as u8),
                }]
            })
            .unwrap_or_default();

        Ok(BusinessOverviewSnapshot {
            account_name,
            account_username,
            account_phone_display,
            is_premium,
            sponsored_disabled,
            details: BusinessDetails {
                hours,
                location,
                intro,
            },
            timezones,
            greeting,
            away,
            chatbots,
            shortcuts,
            chat_links,
            available_users,
            intro_stickers,
            emoji_status,
            emoji_statuses,
            premium_limits: PremiumLimitsData::default(),
            premium_features: Vec::new(),
            premium_plans: Vec::new(),
            tier_comparison: Vec::new(),
            inactive_channels,
            public_link_channels,
        })
    }

    fn set_premium_enabled(&mut self, _enabled: bool) -> Result<(), String> {
        Err(
            "Telegram Premium membership cannot be toggled directly; use Premium checkout."
                .to_owned(),
        )
    }

    fn set_sponsored_disabled(&mut self, disabled: bool) -> Result<(), String> {
        let _ = invoke_rpc(
            self,
            tl_fns::account::ToggleSponsoredMessages { enabled: !disabled },
        )?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn save_working_hours(&mut self, hours: WorkingHours) -> Result<(), String> {
        let normalized = hours.normalized();
        let req = tl_fns::account::UpdateBusinessWorkHours {
            business_work_hours: normalized
                .is_enabled()
                .then(|| working_hours_to_tl(&normalized)),
        };
        let _ = invoke_rpc(self, req)?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn save_location(&mut self, mut location: BusinessLocation) -> Result<(), String> {
        location.address = location.address.trim().to_owned();
        if location.address.is_empty() {
            location.point = None;
        }
        let req = tl_fns::account::UpdateBusinessLocation {
            geo_point: location.point.map(|p| {
                tl_enums::InputGeoPoint::InputGeoPoint(tl_types::InputGeoPoint {
                    lat: p.lat,
                    long: p.lon,
                    accuracy_radius: None,
                })
            }),
            address: (!location.address.is_empty()).then_some(location.address),
        };
        let _ = invoke_rpc(self, req)?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn save_chat_intro(&mut self, intro: ChatIntro) -> Result<(), String> {
        if intro.title.chars().count() > CHAT_INTRO_TITLE_LIMIT {
            return Err(format!(
                "Title cannot exceed {CHAT_INTRO_TITLE_LIMIT} characters."
            ));
        }
        if intro.description.chars().count() > CHAT_INTRO_DESCRIPTION_LIMIT {
            return Err(format!(
                "Description cannot exceed {CHAT_INTRO_DESCRIPTION_LIMIT} characters."
            ));
        }
        // Match `BusinessInfo::saveChatIntro` (`data_business_info.cpp:99-131`):
        // synthetic sticker ID `0` means "Random" and must omit `f_sticker` rather than
        // sending a bogus `InputDocument { id: 0 }`.
        let sticker = intro.sticker_id.filter(|&id| id != 0).map(|id| {
            let (access_hash, file_reference) = with_business_state(self, |st| {
                st.intro_stickers_meta.get(&id).cloned().unwrap_or_default()
            });
            tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                id: id as i64,
                access_hash,
                file_reference,
            })
        });
        let has_intro = intro.custom_phrases() || sticker.is_some();
        let req = tl_fns::account::UpdateBusinessIntro {
            intro: has_intro.then_some(tl_enums::InputBusinessIntro::InputBusinessIntro(
                tl_types::InputBusinessIntro {
                    title: intro.title,
                    description: intro.description,
                    sticker,
                },
            )),
        };
        let _ = invoke_rpc(self, req)?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn save_greeting_settings(&mut self, settings: GreetingSettings) -> Result<(), String> {
        // Match `BusinessInfo::saveGreetingSettings` (`data_business_info.cpp:221-243`):
        // only send `message: None` when explicitly disabled; never silently clear the
        // server's active greeting if enabled without a persisted `shortcut_id`.
        let req = if !settings.is_enabled() {
            tl_fns::account::UpdateBusinessGreetingMessage { message: None }
        } else if settings.shortcut_id > 0 {
            let recipients = read_session(self, |s| {
                recipients_for_messages_to_tl(&settings.recipients.clone().make_valid(), s)
            });
            tl_fns::account::UpdateBusinessGreetingMessage {
                message: Some(
                    tl_enums::InputBusinessGreetingMessage::InputBusinessGreetingMessage(
                        tl_types::InputBusinessGreetingMessage {
                            shortcut_id: settings.shortcut_id,
                            recipients,
                            no_activity_days: settings.no_activity_days,
                        },
                    ),
                ),
            }
        } else {
            return Err("Greeting message requires a saved quick-reply shortcut.".to_owned());
        };
        let _ = invoke_rpc(self, req)?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn save_away_settings(&mut self, settings: AwaySettings) -> Result<(), String> {
        // Match `BusinessInfo::saveAwaySettings` (`data_business_info.cpp:177-199`):
        // only send `message: None` when schedule is `Never`; reject enabled settings
        // without a persisted `shortcut_id` instead of wiping the server setting.
        let req = if !settings.is_enabled() {
            tl_fns::account::UpdateBusinessAwayMessage { message: None }
        } else if settings.shortcut_id > 0 {
            let schedule = match settings.schedule.schedule_type {
                AwayScheduleType::Always => tl_enums::BusinessAwayMessageSchedule::Always,
                AwayScheduleType::OutsideWorkingHours => {
                    tl_enums::BusinessAwayMessageSchedule::OutsideWorkHours
                }
                AwayScheduleType::Custom => {
                    if AwaySchedule::is_bad_custom_interval(settings.schedule.custom_interval) {
                        return Err("Invalid custom away schedule interval.".to_owned());
                    }
                    tl_enums::BusinessAwayMessageSchedule::Custom(
                        tl_types::BusinessAwayMessageScheduleCustom {
                            start_date: settings.schedule.custom_interval.start,
                            end_date: settings.schedule.custom_interval.end,
                        },
                    )
                }
                AwayScheduleType::Never => unreachable!("handled by !settings.is_enabled()"),
            };
            let recipients = read_session(self, |s| {
                recipients_for_messages_to_tl(&settings.recipients.clone().make_valid(), s)
            });
            tl_fns::account::UpdateBusinessAwayMessage {
                message: Some(
                    tl_enums::InputBusinessAwayMessage::InputBusinessAwayMessage(
                        tl_types::InputBusinessAwayMessage {
                            offline_only: settings.offline_only,
                            shortcut_id: settings.shortcut_id,
                            schedule,
                            recipients,
                        },
                    ),
                ),
            }
        } else {
            return Err("Away message requires a saved quick-reply shortcut.".to_owned());
        };
        let _ = invoke_rpc(self, req)?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn search_business_bots(&self, query: &str) -> BotSearchResult {
        let parsed = parse_bot_username_query(query);
        if !parsed.valid {
            return BotSearchResult {
                bots: Vec::new(),
                state: BotLookupState::Empty,
            };
        }
        let Ok(tl_enums::contacts::Found::Found(found)) = invoke_rpc(
            self,
            tl_fns::contacts::Search {
                broadcasts: false,
                bots: true,
                q: parsed.value.clone(),
                limit: 10,
            },
        ) else {
            return BotSearchResult {
                bots: Vec::new(),
                state: BotLookupState::Ready,
            };
        };
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &found.users, &found.chats);
        });
        let q_lower = parsed.value.to_ascii_lowercase();
        let mut bots = Vec::new();
        for user in &found.users {
            let tl_enums::User::User(u) = user else {
                continue;
            };
            if !u.bot {
                continue;
            }
            let uname = u.username.as_deref().unwrap_or_default();
            if !u.bot_business {
                if uname.eq_ignore_ascii_case(&q_lower) {
                    return BotSearchResult {
                        bots: Vec::new(),
                        state: BotLookupState::Unsupported,
                    };
                }
                continue;
            }
            bots.push(user_to_bot_candidate(u, String::new()));
            if bots.len() >= MAX_SEARCH_BOTS {
                break;
            }
        }
        BotSearchResult {
            bots,
            state: BotLookupState::Ready,
        }
    }

    fn save_chatbots(&mut self, settings: ChatbotsSettings) -> Result<(), String> {
        let prev_bot = with_business_state(self, |st| st.connected_bot.clone());
        let (deleted, target_bot) = match (&settings.bot, &prev_bot) {
            (Some(b), _) => (false, b.clone()),
            (None, Some(prev)) => (true, prev.clone()),
            (None, None) => return Ok(()),
        };
        let (bot_input, recipients) = read_session(self, |s| {
            let recipients = if deleted {
                // Match `Authorizations::review` (`api_authorizations.cpp:466`):
                // disconnecting sends empty `ForBotsToMTP({})`.
                recipients_for_bots_to_tl(&BusinessRecipients::default(), s)
            } else {
                recipients_for_bots_to_tl(&settings.recipients.clone().make_valid(), s)
            };
            (input_user_for_id(s, target_bot.id), recipients)
        });
        let req = tl_fns::account::UpdateConnectedBot {
            deleted,
            rights: (!deleted).then(|| bot_rights_to_tl(settings.permissions)),
            bot: bot_input,
            recipients,
        };
        let updates = invoke_rpc(self, req)?;
        // Match `Chatbots::save` (`data_business_chatbots.cpp:113-114`): apply returned updates.
        let _ = self.apply_updates_to_session(&updates);
        with_business_state(self, |st| {
            st.connected_bot = if deleted { None } else { settings.bot };
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn emplace_shortcut(&mut self, name: &str) -> Result<QuickReplyShortcut, String> {
        let cleaned = name.trim().trim_start_matches('/');
        if !valid_shortcut_name(cleaned) {
            return Err(
                "Shortcut name must be 1..=32 alphanumeric or underscore characters.".to_owned(),
            );
        }
        with_business_state(self, |st| {
            if let Some(existing) = st
                .shortcuts
                .values()
                .find(|sc| sc.name.eq_ignore_ascii_case(cleaned))
            {
                return Ok(existing.clone());
            }
            if st.shortcuts.len() >= QUICK_REPLIES_LIMIT {
                return Err(format!(
                    "Maximum of {QUICK_REPLIES_LIMIT} quick reply shortcuts reached."
                ));
            }
            let id = st.alloc_local_shortcut_id();
            let shortcut = QuickReplyShortcut {
                id,
                name: cleaned.to_owned(),
                messages: Vec::new(),
            };
            st.shortcuts.insert(id, shortcut.clone());
            Ok(shortcut)
        })
    }

    fn rename_shortcut(
        &mut self,
        shortcut_id: BusinessShortcutId,
        new_name: &str,
    ) -> Result<QuickReplyShortcut, String> {
        let cleaned = new_name.trim().trim_start_matches('/');
        if !valid_shortcut_name(cleaned) {
            return Err(
                "Shortcut name must be 1..=32 alphanumeric or underscore characters.".to_owned(),
            );
        }
        if shortcut_id > 0 {
            let _ = invoke_rpc(
                self,
                tl_fns::messages::EditQuickReplyShortcut {
                    shortcut_id,
                    shortcut: cleaned.to_owned(),
                },
            )?;
        }
        let updated = with_business_state(self, |st| {
            if let Some(sc) = st.shortcuts.get_mut(&shortcut_id) {
                sc.name = cleaned.to_owned();
                sc.clone()
            } else {
                let sc = QuickReplyShortcut {
                    id: shortcut_id,
                    name: cleaned.to_owned(),
                    messages: Vec::new(),
                };
                st.shortcuts.insert(shortcut_id, sc.clone());
                sc
            }
        });
        self.bridge_state().notify_ui();
        Ok(updated)
    }

    fn remove_shortcut(&mut self, shortcut_id: BusinessShortcutId) -> Result<(), String> {
        if shortcut_id > 0 {
            let _ = invoke_rpc(
                self,
                tl_fns::messages::DeleteQuickReplyShortcut { shortcut_id },
            )?;
        }
        with_business_state(self, |st| {
            st.shortcuts.remove(&shortcut_id);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn add_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        text: &str,
        attachment: QuickReplyAttachment,
    ) -> Result<QuickReplyMessage, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() && attachment == QuickReplyAttachment::None {
            return Err("Quick reply message cannot be empty.".to_owned());
        }
        let (current_count, shortcut_name, random_id) = with_business_state(self, |st| {
            let sc = st.shortcuts.get(&shortcut_id);
            (
                sc.map_or(0, |s| s.messages.len()),
                sc.map(|s| s.name.clone()),
                st.next_random_id(),
            )
        });
        if current_count >= SHORTCUT_MESSAGES_LIMIT {
            return Err(format!(
                "A quick reply shortcut can contain at most {SHORTCUT_MESSAGES_LIMIT} messages."
            ));
        }
        let quick_reply_shortcut = if shortcut_id > 0 {
            tl_enums::InputQuickReplyShortcut::Id(tl_types::InputQuickReplyShortcutId {
                shortcut_id,
            })
        } else {
            let name = shortcut_name
                .ok_or_else(|| "Shortcut must be created before adding messages.".to_owned())?;
            tl_enums::InputQuickReplyShortcut::InputQuickReplyShortcut(
                tl_types::InputQuickReplyShortcut { shortcut: name },
            )
        };
        let updates = invoke_rpc(
            self,
            tl_fns::messages::SendMessage {
                no_webpage: false,
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_enums::InputPeer::Self_,
                reply_to: None,
                message: trimmed.to_owned(),
                random_id,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: Some(quick_reply_shortcut),
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: None,
            },
        )?;
        let (msg_id, real_shortcut_id, date) =
            extract_quick_reply_msg_from_updates(&updates, shortcut_id);
        let _ = self.apply_updates_to_session(&updates);
        let msg = QuickReplyMessage {
            id: msg_id,
            text: trimmed.to_owned(),
            attachment,
            time_label: format_short_time(date),
        };
        with_business_state(self, |st| {
            if real_shortcut_id != shortcut_id
                && let Some(mut old) = st.shortcuts.remove(&shortcut_id)
            {
                old.id = real_shortcut_id;
                old.messages.push(msg.clone());
                st.shortcuts.insert(real_shortcut_id, old);
            } else if let Some(sc) = st.shortcuts.get_mut(&real_shortcut_id) {
                sc.messages.push(msg.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(msg)
    }

    fn edit_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
        new_text: &str,
    ) -> Result<QuickReplyMessage, String> {
        let trimmed = new_text.trim();
        if trimmed.is_empty() {
            return Err("Message text cannot be empty.".to_owned());
        }
        if let Some(updates) = self.edit_message_via_runtime(tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer: tl_enums::InputPeer::Self_,
            id: message_id,
            message: Some(trimmed.to_owned()),
            media: None,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: Some(shortcut_id),
            rich_message: None,
        })? {
            self.apply_updates_side_caches(&updates);
        }
        let updated = with_business_state(self, |st| {
            if let Some(sc) = st.shortcuts.get_mut(&shortcut_id)
                && let Some(msg) = sc.messages.iter_mut().find(|m| m.id == message_id)
            {
                msg.text = trimmed.to_owned();
                return msg.clone();
            }
            QuickReplyMessage {
                id: message_id,
                text: trimmed.to_owned(),
                attachment: QuickReplyAttachment::None,
                time_label: format_short_time(now_unix_i32()),
            }
        });
        self.bridge_state().notify_ui();
        Ok(updated)
    }

    fn delete_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
    ) -> Result<(), String> {
        let updates = invoke_rpc(
            self,
            tl_fns::messages::DeleteQuickReplyMessages {
                shortcut_id,
                id: vec![message_id],
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        with_business_state(self, |st| {
            if let Some(sc) = st.shortcuts.get_mut(&shortcut_id) {
                sc.messages.retain(|m| m.id != message_id);
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn create_chat_link(&mut self, title: &str, message: &str) -> Result<BusinessChatLink, String> {
        let count = with_business_state(self, |st| st.chat_links.len());
        if count >= CHAT_LINKS_LIMIT {
            return Err(format!(
                "Maximum of {CHAT_LINKS_LIMIT} business chat links reached."
            ));
        }
        let trimmed_title = title.trim();
        let req = tl_fns::account::CreateBusinessChatLink {
            link: tl_enums::InputBusinessChatLink::InputBusinessChatLink(
                tl_types::InputBusinessChatLink {
                    message: message.trim().to_owned(),
                    entities: None,
                    title: (!trimmed_title.is_empty()).then(|| trimmed_title.to_owned()),
                },
            ),
        };
        let resp = invoke_rpc(self, req)?;
        let parsed = chat_link_from_tl(&resp);
        with_business_state(self, |st| {
            st.chat_links.push(parsed.clone());
        });
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn edit_chat_link(
        &mut self,
        link: &str,
        title: &str,
        message: &str,
    ) -> Result<BusinessChatLink, String> {
        let trimmed_title = title.trim();
        let req = tl_fns::account::EditBusinessChatLink {
            slug: link.to_owned(),
            link: tl_enums::InputBusinessChatLink::InputBusinessChatLink(
                tl_types::InputBusinessChatLink {
                    message: message.trim().to_owned(),
                    entities: None,
                    title: (!trimmed_title.is_empty()).then(|| trimmed_title.to_owned()),
                },
            ),
        };
        let resp = invoke_rpc(self, req)?;
        let parsed = chat_link_from_tl(&resp);
        with_business_state(self, |st| {
            if let Some(slot) = st.chat_links.iter_mut().find(|l| l.link == link) {
                *slot = parsed.clone();
            } else {
                st.chat_links.push(parsed.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn delete_chat_link(&mut self, link: &str) -> Result<(), String> {
        let _ = invoke_rpc(
            self,
            tl_fns::account::DeleteBusinessChatLink {
                slug: link.to_owned(),
            },
        )?;
        with_business_state(self, |st| {
            st.chat_links.retain(|l| l.link != link);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn set_emoji_status(&mut self, status: ActiveEmojiStatus) -> Result<(), String> {
        let now = now_unix_i32();
        let tl_status = match status.status_id {
            Some(doc_id) => {
                let secs = status.duration.seconds();
                let until = (secs > 0).then(|| now.saturating_add(secs));
                tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                    document_id: doc_id as i64,
                    until,
                })
            }
            None => tl_enums::EmojiStatus::Empty,
        };
        let _ = invoke_rpc(
            self,
            tl_fns::account::UpdateEmojiStatus {
                emoji_status: tl_status,
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn leave_inactive_channels(&mut self, channel_ids: &[i64]) -> Result<usize, String> {
        let mut left = 0usize;
        for &cid in channel_ids {
            let input_channel = read_session(self, |s| input_channel_for_id(s, cid)).unwrap_or(
                tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                    channel_id: cid,
                    access_hash: 0,
                }),
            );
            let _ = invoke_rpc(
                self,
                tl_fns::channels::LeaveChannel {
                    channel: input_channel,
                },
            )?;
            left += 1;
        }
        if left > 0 {
            self.bridge_state().notify_ui();
        }
        Ok(left)
    }

    fn revoke_public_link(&mut self, channel_id: i64) -> Result<(), String> {
        let input_channel = read_session(self, |s| input_channel_for_id(s, channel_id)).unwrap_or(
            tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id,
                access_hash: 0,
            }),
        );
        let _ = invoke_rpc(
            self,
            tl_fns::channels::UpdateUsername {
                channel: input_channel,
                username: String::new(),
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(())
    }
}

fn extract_quick_reply_msg_from_updates(
    updates: &tl_enums::Updates,
    fallback_shortcut_id: BusinessShortcutId,
) -> (i32, BusinessShortcutId, i32) {
    let now = now_unix_i32();
    match updates {
        tl_enums::Updates::UpdateShortSentMessage(short) => {
            (short.id, fallback_shortcut_id, short.date)
        }
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                if let tl_enums::Update::QuickReplyMessage(q) = upd
                    && let tl_enums::Message::Message(m) = &q.message
                {
                    return (
                        m.id,
                        m.quick_reply_shortcut_id.unwrap_or(fallback_shortcut_id),
                        m.date,
                    );
                }
                if let tl_enums::Update::NewMessage(nm) = upd
                    && let tl_enums::Message::Message(m) = &nm.message
                {
                    return (
                        m.id,
                        m.quick_reply_shortcut_id.unwrap_or(fallback_shortcut_id),
                        m.date,
                    );
                }
            }
            (0, fallback_shortcut_id, now)
        }
        tl_enums::Updates::UpdateShort(short) => {
            if let tl_enums::Update::QuickReplyMessage(q) = &short.update
                && let tl_enums::Message::Message(m) = &q.message
            {
                return (
                    m.id,
                    m.quick_reply_shortcut_id.unwrap_or(fallback_shortcut_id),
                    m.date,
                );
            }
            (0, fallback_shortcut_id, short.date)
        }
        _ => (0, fallback_shortcut_id, now),
    }
}
