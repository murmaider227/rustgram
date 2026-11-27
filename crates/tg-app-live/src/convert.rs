//! Conversions between `tg-data` / `tg-tl` models and `tg-app`'s screen view structs.
//!
//! Port origin: `Telegram/SourceFiles/data/data_peer_values.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_entry.cpp`,
//! `Telegram/SourceFiles/history/history_item.cpp`,
//! `Telegram/SourceFiles/api/api_user_privacy.cpp`, and
//! `Telegram/SourceFiles/api/api_authorizations.cpp`.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::Color32;
use tg_data::{
    BlockStatus, ChatFilter, ChatFilterFlags, DocumentData, DocumentKind, EntityKind,
    ExtendedMedia, History, Message, MessageContent, MessageFlags, MessageMedia,
    MsgId as DataMsgId, Peer, PeerId as DataPeerId, SendCheckContext,
    ServiceAction as DataServiceAction, Session, TextEntity, TextWithEntities as DataText,
    TypingAction as DataTypingAction, UserId as DataUserId,
};
use tg_history::{DialogBadgesState, DialogSendState, LocalDateTime};
use tg_text::{EntityInText, EntityType, TextWithEntities as UiText};
use tg_tl::{enums as tl_enums, types as tl_types};

use tg_app_core::calls::backend::CallPeerInfo;
use tg_app_core::dialogs::backend::{
    ChatFolder as DialogChatFolder, DialogDraft, DialogItem, DialogListQuery, DialogPeerKind,
    FolderIconKind, FolderTagBadge, FolderUnreadState, SpecialRowKind,
    TypingAction as DialogTyping, TypingState,
};
use tg_app_core::history::backend::{
    AlbumItem, ContactStatusBanner, DocumentAttachment, DraftMessage, ForwardHeader,
    GameAttachment, GiveawayAttachment, GiveawayResultsAttachment, HistoryMedia, HistoryMessage,
    HistoryPeerInfo, MessageExtra, PhotoAttachment, PollAttachment, PollOption, ReplyHeader,
    ServiceAction as UiServiceAction, StickerAttachment, StoryAttachment,
    TypingAction as HistoryTypingAction, VideoAttachment, VideoStreamAttachment, VoiceAttachment,
    WidgetAttachment,
};
use tg_app_core::history::sections_model::{
    ContactAttachment, LinkPreviewData, LocationAttachment, RoundVideoAttachment,
};
use tg_app_core::info::backend::{
    InfoPeerKind, MediaItem as InfoMediaItem, PeerProfile as InfoPeerProfile, PeerUsername,
    SharedMediaCounts, SharedMediaTab,
};
use tg_app_core::info::values::Birthday as InfoBirthday;
use tg_app_core::media::backend::{
    AudioTrackMetadata, MediaItem as ViewerMediaItem, MediaItemId, MediaItemKind,
};
use tg_app_core::notifications::backend::{
    IncomingMessageNotification, ItemNotificationType, NotificationMediaKind, NotifyPeerInfo,
    TextSpoilerRange,
};
use tg_app_core::notifications::settings::UnreadBadgeState;
use tg_app_core::settings::backend::{
    ActiveSession, Birthday as SettingsBirthday, ChatFolder as SettingsChatFolder, FolderIcon,
    NotifyScope, PeerRef, PrivacyKey, PrivacyOption, PrivacyRule, ProfileInfo, ScopeNotifySettings,
};
use tg_app_core::stand_in::types::{
    AuthUser, DialogEntry, FilterId, MessageItem, MsgId, PeerId, PeerKind, PeerSummary,
};
use tg_app_core::stories::backend::PeerProfile as StoryPeerProfile;

/// Standard 7-color Telegram peer avatar RGB palette.
const PEER_PALETTE_RGB: [[u8; 3]; 7] = [
    [0xE1, 0x70, 0x76], // 0: red
    [0xED, 0xA8, 0x6C], // 1: orange
    [0xA6, 0x95, 0xE7], // 2: violet
    [0x7B, 0xC8, 0x62], // 3: green
    [0x6E, 0xC9, 0xCB], // 4: cyan
    [0x4D, 0xA6, 0xE8], // 5: blue
    [0xEE, 0x7A, 0xAE], // 6: pink
];

/// Converts a `tg-app` [`PeerId`] into a `tg-data` [`DataPeerId`].
#[inline]
#[must_use]
pub const fn to_data_peer(peer: PeerId) -> DataPeerId {
    DataPeerId(peer.0)
}

/// Converts a `tg-data` [`DataPeerId`] into a `tg-app` [`PeerId`].
#[inline]
#[must_use]
pub const fn from_data_peer(peer: DataPeerId) -> PeerId {
    PeerId(peer.0)
}

/// Converts a `tg-app` [`MsgId`] into a `tg-data` [`DataMsgId`].
#[inline]
#[must_use]
pub const fn to_data_msg(msg: MsgId) -> DataMsgId {
    match msg.local_index() {
        Some(index) => match DataMsgId::client_by_index(index) {
            Some(id) => id,
            None => DataMsgId(msg.0 as i64),
        },
        None => DataMsgId(msg.0 as i64),
    }
}

/// Converts a `tg-data` [`DataMsgId`] into a `tg-app` [`MsgId`].
#[inline]
#[must_use]
pub const fn from_data_msg(msg: DataMsgId) -> MsgId {
    // Client (local) ids map one to one onto the UI's local range, so two messages being sent
    // never share an id; any other id out of `i32` range clamps.
    if let Some(index) = msg.client_index()
        && let Some(local) = MsgId::local(index)
    {
        return local;
    }
    let clamped = if msg.0 > i32::MAX as i64 {
        i32::MAX
    } else if msg.0 < i32::MIN as i64 {
        i32::MIN
    } else {
        msg.0 as i32
    };
    MsgId(clamped)
}

/// Returns current Unix timestamp in seconds (`i32` / `tg_data::TimeId`).
#[must_use]
pub fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(i32::MAX as u64) as i32)
}

/// Returns current Unix timestamp in seconds (`i64`).
#[must_use]
pub fn now_unix_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Converts a Unix timestamp (seconds) into `tg_history::LocalDateTime` in the local time zone
/// (`base::unixtime::parse` / `QDateTime::fromSecsSinceEpoch`).
#[must_use]
pub fn unix_to_local_datetime(ts: i32) -> LocalDateTime {
    if ts <= 0 {
        return LocalDateTime::new(1970, 1, 1, 0, 0, 0);
    }
    LocalDateTime::from_unix_local(i64::from(ts))
}

/// Formats a Unix timestamp in local time using tdesktop's short 24-hour format (`"H:mm"`,
/// e.g. `"3:07"`, `"16:29"`, matching `QLocale::ShortFormat`).
#[must_use]
pub fn format_short_time(ts: i32) -> String {
    tg_history::datetime::format_time_short(unix_to_local_datetime(ts))
}

/// Formats a local civil date as `DD.MM.YY` (`QLocale::ShortFormat` short date).
#[must_use]
fn format_short_calendar_date(dt: LocalDateTime) -> String {
    let yy = dt.date.year.rem_euclid(100);
    format!("{:02}.{:02}.{:02}", dt.date.day, dt.date.month, yy)
}

/// Formats a dialog list timestamp in local time following tdesktop's `Ui::FormatDialogsDate`
/// (`Telegram/SourceFiles/ui/text/format_values.cpp:724-740`):
/// - same calendar day OR within 20 hours of `now`: `"H:mm"` (e.g. `"3:07"`, `"16:29"`)
/// - within 6 calendar days: localized short weekday (`"Mon"`..`"Sun"`)
/// - 7 or more days away: short date (`"DD.MM.YY"`).
#[must_use]
pub fn format_dialogs_date_local(last_time: LocalDateTime, now: LocalDateTime) -> String {
    match tg_history::classify_dialogs_date(last_time, now) {
        tg_history::DialogsDateKind::Time => tg_history::datetime::format_time_short(last_time),
        tg_history::DialogsDateKind::Weekday => {
            let wd = last_time.date.weekday() as u8;
            tg_lang::with_current(|inst| tg_lang::keys::day_of_week(inst, wd))
        }
        tg_history::DialogsDateKind::Date => format_short_calendar_date(last_time),
    }
}

/// Formats a Unix timestamp as a dialog list timestamp relative to `now_ts` in local time
/// (`Ui::FormatDialogsDate`).
#[must_use]
pub fn format_dialogs_date_at(ts: i32, now_ts: i32) -> String {
    format_dialogs_date_local(unix_to_local_datetime(ts), unix_to_local_datetime(now_ts))
}

/// Returns all loaded messages for `peer_id` in chronological order (oldest to newest).
#[must_use]
pub fn history_messages_ordered(session: &Session, peer_id: DataPeerId) -> Vec<&Message> {
    let Some(history) = session.history(peer_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for slice in history.loaded_ranges.slices() {
        for &mid in &slice.messages {
            if let Some(msg) = session.message(tg_data::FullMsgId::new(peer_id, mid)) {
                out.push(msg);
            }
        }
    }
    out
}

/// Every stored message of `peer_id` oldest to newest: the loaded history plus messages stored
/// from other pages (search results, topic and comment pages, reply targets), like the items a
/// tdesktop `History` owns (`History::_items`). For lookups by id, media or file reference and
/// for changes that apply to every item (`History::unpinMessagesFor`).
pub fn stored_messages_ordered(session: &Session, peer_id: DataPeerId) -> Vec<&Message> {
    session
        .stored_message_ids(peer_id)
        .filter_map(|id| session.message(tg_data::FullMsgId::new(peer_id, id)))
        .collect()
}

/// The loaded messages of `peer_id` oldest to newest, like [`history_messages_ordered`] but
/// without collecting them: a page read walks only as far as it needs from either end.
///
/// Items that display nothing are skipped, as tdesktop's history skips them
/// (`History::isDisplayedEmpty`, `Element::recountDisplayDateInBlocks`).
pub fn history_messages_iter(
    session: &Session,
    peer_id: DataPeerId,
) -> impl DoubleEndedIterator<Item = &Message> {
    session
        .history(peer_id)
        .into_iter()
        .flat_map(|history| history.loaded_ranges.slices().iter())
        .flat_map(|slice| slice.messages.iter())
        .filter_map(move |&mid| session.message(tg_data::FullMsgId::new(peer_id, mid)))
        .filter(|msg| !displays_nothing(msg))
}

/// `true` for a message tdesktop draws as nothing: the "history cleared" service item
/// (`messageActionHistoryClear`) has empty text (`PrepareEmptyText`, `history_item.cpp`), so its
/// view has no bubble, no date and no height.
#[must_use]
pub fn displays_nothing(msg: &Message) -> bool {
    matches!(msg.service_action(), Some(DataServiceAction::HistoryClear))
}

/// Returns all unique peer IDs known via self, main dialogs, and archive dialogs.
#[must_use]
pub fn known_peer_ids(session: &Session) -> Vec<DataPeerId> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let self_id = session.self_peer_id();
    if session.peer(self_id).is_some() && seen.insert(self_id) {
        out.push(self_id);
    }
    for key in session
        .main_dialogs()
        .iter()
        .chain(session.archive_dialogs().iter())
    {
        if let tg_data::DialogKey::History(pid) = *key
            && seen.insert(pid)
        {
            out.push(pid);
        }
    }
    // Chats with messages whose folder is still being requested are out of both lists.
    for pid in session.histories_awaiting_folder() {
        if seen.insert(pid) {
            out.push(pid);
        }
    }
    out
}

/// Formats a Unix timestamp as `"Month YYYY"` for shared media grouping.
#[must_use]
pub fn format_month_year(ts: i32) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let dt = unix_to_local_datetime(ts);
    let m = MONTHS
        .get(usize::from(dt.date.month.saturating_sub(1)))
        .copied()
        .unwrap_or("January");
    format!("{m} {}", dt.date.year)
}

/// Formats a Unix timestamp relative to `now_ts` in local time following tdesktop's
/// `Ui::FormatDateTime` (`Telegram/SourceFiles/ui/text/format_values.cpp:82-102`):
/// - same local day: `lng_mediaview_today` (`"today at H:mm"`)
/// - yesterday: `lng_mediaview_yesterday` (`"yesterday at H:mm"`)
/// - older/other day: `lng_mediaview_date_time` (`"DD.MM.YY at H:mm"`).
#[must_use]
pub fn format_date_time_at(ts: i32, now_ts: i32) -> String {
    let dt = unix_to_local_datetime(ts);
    let now = unix_to_local_datetime(now_ts);
    let time_str = tg_history::datetime::format_time_short(dt);
    if dt.date == now.date {
        tg_lang::tr::lng_mediaview_today(&time_str)
    } else if dt.date.days_to(now.date) == 1 {
        tg_lang::tr::lng_mediaview_yesterday(&time_str)
    } else {
        let date_str = format_short_calendar_date(dt);
        tg_lang::tr::lng_mediaview_date_time(&date_str, &time_str)
    }
}

/// Formats a Unix timestamp in local time using tdesktop's `Ui::FormatDateTime` relative to
/// the current wall clock (`"today at H:mm"` / `"yesterday at H:mm"` / `"DD.MM.YY at H:mm"`).
#[must_use]
pub fn format_date_time(ts: i32) -> String {
    format_date_time_at(ts, now_unix_i32())
}

/// Formats a read-receipt Unix timestamp relative to `now_ts` in local time following tdesktop's
/// `Api::FormatReadDate` (`Telegram/SourceFiles/api/api_who_reacted.cpp:702-726`):
/// - same local day: `lng_mediaview_today` (`"today at H:mm"`)
/// - yesterday: `lng_mediaview_yesterday` (`"yesterday at H:mm"`)
/// - earlier: `lng_mediaview_date_time` with short month/day (`"Nov 20 at H:mm"`).
#[must_use]
pub fn format_read_date_at(ts: i32, now_ts: i32) -> String {
    if ts <= 0 {
        return String::new();
    }
    let dt = unix_to_local_datetime(ts);
    let now = unix_to_local_datetime(now_ts);
    let time_str = tg_history::datetime::format_time_short(dt);
    if dt.date == now.date {
        tg_lang::tr::lng_mediaview_today(&time_str)
    } else if dt.date.days_to(now.date) == 1 {
        tg_lang::tr::lng_mediaview_yesterday(&time_str)
    } else {
        let read_d = tg_lang::SimpleDate {
            year: dt.date.year,
            month: dt.date.month,
            day: dt.date.day,
        };
        let today_d = tg_lang::SimpleDate {
            year: now.date.year,
            month: now.date.month,
            day: now.date.day,
        };
        let date_str =
            tg_lang::with_current(|inst| tg_lang::keys::day_of_month(inst, read_d, today_d));
        tg_lang::tr::lng_mediaview_date_time(&date_str, &time_str)
    }
}

/// Computes 1-2 uppercase avatar monogram initials from a display name.
#[must_use]
pub fn initials_from_name(name: &str) -> String {
    let mut parts = name.split_whitespace();
    let first = parts.next().and_then(|w| w.chars().next());
    let second = parts.next().and_then(|w| w.chars().next());
    match (first, second) {
        (Some(a), Some(b)) => format!(
            "{}{}",
            a.to_uppercase().next().unwrap_or(a),
            b.to_uppercase().next().unwrap_or(b)
        ),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".to_owned(),
    }
}

/// Maps a Telegram peer color index into an RGB fallback avatar color.
#[must_use]
pub const fn avatar_rgb_for_color_index(color_index: u8) -> [u8; 3] {
    PEER_PALETTE_RGB[(color_index as usize) % PEER_PALETTE_RGB.len()]
}

/// Converts a `tg_data::TextWithEntities` into a `tg_text::TextWithEntities`.
#[must_use]
pub fn data_text_to_ui_text(src: &DataText) -> UiText {
    let entities = src
        .entities
        .iter()
        .map(|e| {
            let (ty, data) = match &e.kind {
                EntityKind::Unknown => (EntityType::Invalid, String::new()),
                EntityKind::Mention => (EntityType::Mention, String::new()),
                EntityKind::Hashtag => (EntityType::Hashtag, String::new()),
                EntityKind::BotCommand => (EntityType::BotCommand, String::new()),
                EntityKind::Url => (EntityType::Url, String::new()),
                EntityKind::Email => (EntityType::Email, String::new()),
                EntityKind::Bold => (EntityType::Bold, String::new()),
                EntityKind::Semibold => (EntityType::Semibold, String::new()),
                EntityKind::Italic => (EntityType::Italic, String::new()),
                EntityKind::Code => (EntityType::Code, String::new()),
                EntityKind::Pre { language } => (EntityType::Pre, language.clone()),
                EntityKind::TextUrl { url } => (EntityType::CustomUrl, url.clone()),
                EntityKind::MentionName { user_id } => {
                    (EntityType::MentionName, user_id.0.to_string())
                }
                EntityKind::Phone => (EntityType::Phone, String::new()),
                EntityKind::Cashtag => (EntityType::Cashtag, String::new()),
                EntityKind::Underline => (EntityType::Underline, String::new()),
                EntityKind::StrikeOut => (EntityType::StrikeOut, String::new()),
                EntityKind::BankCard => (EntityType::BankCard, String::new()),
                EntityKind::Spoiler => (EntityType::Spoiler, String::new()),
                EntityKind::CustomEmoji { document_id, .. } => {
                    (EntityType::CustomEmoji, document_id.0.to_string())
                }
                EntityKind::Blockquote { collapsed } => (
                    EntityType::Blockquote,
                    if *collapsed {
                        "1".to_owned()
                    } else {
                        String::new()
                    },
                ),
                EntityKind::FormattedDate { date, flags } => (
                    EntityType::FormattedDate,
                    format!("{date}:{}", flags.bits()),
                ),
                EntityKind::DiffInsert
                | EntityKind::DiffReplace { .. }
                | EntityKind::DiffDelete => (EntityType::Invalid, String::new()),
                EntityKind::TonAddress => (EntityType::TonAddress, String::new()),
                EntityKind::MediaTimestamp { seconds } => {
                    (EntityType::MediaTimestamp, seconds.to_string())
                }
                EntityKind::Colorized => (EntityType::Colorized, String::new()),
                EntityKind::Subscript => (EntityType::Subscript, String::new()),
                EntityKind::Superscript => (EntityType::Superscript, String::new()),
                EntityKind::Marked => (EntityType::Marked, String::new()),
            };
            EntityInText::with_data(ty, e.offset, e.length, data)
        })
        .filter(|e| e.entity_type() != EntityType::Invalid)
        .collect();
    UiText {
        text: src.text.clone(),
        entities,
    }
}

/// Converts a `tg_text::TextWithEntities` into a `tg_data::TextWithEntities`.
#[must_use]
pub fn ui_text_to_data_text(src: &UiText) -> DataText {
    let entities = src
        .entities
        .iter()
        .filter_map(|e| {
            let kind = match e.entity_type() {
                EntityType::Url => EntityKind::Url,
                EntityType::CustomUrl => EntityKind::TextUrl {
                    url: e.data().to_owned(),
                },
                EntityType::Email => EntityKind::Email,
                EntityType::Hashtag => EntityKind::Hashtag,
                EntityType::Cashtag => EntityKind::Cashtag,
                EntityType::Mention => EntityKind::Mention,
                EntityType::MentionName => {
                    let uid = e
                        .data()
                        .split(['.', ':'])
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    EntityKind::MentionName {
                        user_id: DataUserId(uid),
                    }
                }
                EntityType::CustomEmoji => EntityKind::CustomEmoji {
                    document_id: tg_data::DocumentId(e.data().parse::<u64>().unwrap_or(0)),
                    force_static: false,
                },
                EntityType::BotCommand => EntityKind::BotCommand,
                EntityType::Phone => EntityKind::Phone,
                EntityType::BankCard => EntityKind::BankCard,
                EntityType::TonAddress => EntityKind::TonAddress,
                EntityType::Bold => EntityKind::Bold,
                EntityType::Italic => EntityKind::Italic,
                EntityType::Underline => EntityKind::Underline,
                EntityType::StrikeOut => EntityKind::StrikeOut,
                EntityType::Code => EntityKind::Code,
                EntityType::Pre => EntityKind::Pre {
                    language: e.data().to_owned(),
                },
                EntityType::Blockquote => EntityKind::Blockquote {
                    collapsed: !e.data().is_empty(),
                },
                EntityType::Spoiler => EntityKind::Spoiler,
                EntityType::FormattedDate => {
                    let (date, flags) = tg_text::deserialize_formatted_date_data(e.data());
                    EntityKind::FormattedDate {
                        date,
                        flags: tg_data::FormattedDateFlags::from_bits_truncate(flags.bits()),
                    }
                }
                EntityType::Invalid
                | EntityType::Semibold
                | EntityType::MediaTimestamp
                | EntityType::Colorized
                | EntityType::Subscript
                | EntityType::Superscript
                | EntityType::Marked => return None,
            };
            Some(TextEntity::new(e.offset(), e.length(), kind))
        })
        .collect();
    DataText::new(src.text.clone(), entities)
}

/// Converts a `tg_text::TextWithEntities` into outgoing TL `MessageEntity` items.
#[must_use]
pub fn ui_text_to_tl_entities(src: &UiText, session: &Session) -> Vec<tl_enums::MessageEntity> {
    let data = ui_text_to_data_text(src);
    tg_api::convert::entities_to_tl(&data.entities, session)
}

/// Converts a `tg-data` [`Peer`] representing the current account into [`AuthUser`].
#[must_use]
pub fn peer_to_auth_user(peer: &Peer) -> AuthUser {
    let common = peer.common();
    let name = if common.name.is_empty() {
        format!("User {}", common.id.bare())
    } else {
        common.name.clone()
    };
    let (username, phone) = match peer {
        Peer::User(u) => (u.usernames.username().to_owned(), u.phone.clone()),
        _ => (String::new(), String::new()),
    };
    AuthUser {
        id: from_data_peer(common.id),
        initials: initials_from_name(&name),
        name,
        username,
        phone,
        bio: common.about.clone(),
        avatar_rgb: avatar_rgb_for_color_index(common.color_index),
    }
}

/// Formats a peer's presence or member/subscriber subtitle.
#[must_use]
pub fn peer_status_text(peer: &Peer, now: i32) -> (String, bool) {
    match peer {
        Peer::User(u) => {
            if u.is_self() {
                ("cloud storage".to_owned(), false)
            } else {
                let online = u.is_online(now);
                (u.online_text(now, 0), online)
            }
        }
        Peer::Chat(c) => {
            let count = c.count.max(0);
            if count == 1 {
                ("1 member".to_owned(), false)
            } else {
                (format!("{count} members"), false)
            }
        }
        Peer::Channel(ch) => {
            let count = ch.members_count.max(0);
            if ch.is_broadcast() {
                if count == 1 {
                    ("1 subscriber".to_owned(), false)
                } else {
                    (format!("{count} subscribers"), false)
                }
            } else if count == 1 {
                ("1 member".to_owned(), false)
            } else {
                (format!("{count} members"), false)
            }
        }
    }
}

/// Returns a peer's coarse `PeerKind` (`User`, `Group`, or `Channel`).
#[must_use]
pub fn peer_coarse_kind(peer: &Peer) -> PeerKind {
    match peer {
        Peer::User(_) => PeerKind::User,
        Peer::Chat(_) => PeerKind::Group,
        Peer::Channel(ch) => {
            if ch.is_broadcast() {
                PeerKind::Channel
            } else {
                PeerKind::Group
            }
        }
    }
}

/// Returns a peer's detailed `DialogPeerKind`.
#[must_use]
pub fn peer_dialog_kind(peer: &Peer) -> DialogPeerKind {
    match peer {
        Peer::User(u) => {
            if u.is_bot() {
                DialogPeerKind::Bot
            } else {
                DialogPeerKind::User
            }
        }
        Peer::Chat(_) => DialogPeerKind::Group,
        Peer::Channel(ch) => {
            if ch.is_broadcast() {
                DialogPeerKind::Channel
            } else if ch.is_forum() {
                DialogPeerKind::Forum
            } else {
                DialogPeerKind::Group
            }
        }
    }
}

/// Returns a peer's `InfoPeerKind`.
#[must_use]
pub fn peer_info_kind(peer: &Peer, self_peer_id: DataPeerId) -> InfoPeerKind {
    if peer.id() == self_peer_id {
        return InfoPeerKind::SavedMessages;
    }
    match peer {
        Peer::User(u) => {
            if u.is_bot() {
                InfoPeerKind::Bot
            } else {
                InfoPeerKind::User
            }
        }
        Peer::Chat(_) => InfoPeerKind::Group,
        Peer::Channel(ch) => {
            if ch.is_broadcast() {
                InfoPeerKind::Channel
            } else {
                InfoPeerKind::Supergroup
            }
        }
    }
}

/// Returns a peer's primary active username without leading `@`, if any.
#[must_use]
pub fn peer_primary_username(peer: &Peer) -> Option<String> {
    let raw = match peer {
        Peer::User(u) => u.usernames.username(),
        Peer::Channel(ch) => ch.usernames.username(),
        Peer::Chat(_) => "",
    };
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_owned())
    }
}

/// Returns a human-readable display title for `peer_id` from `session`.
#[must_use]
pub fn peer_display_title(session: &Session, peer_id: DataPeerId) -> String {
    if peer_id == session.self_peer_id() {
        return "Saved Messages".to_owned();
    }
    if let Some(peer) = session.peer(peer_id) {
        let name = peer.name();
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    format!("Peer {}", peer_id.bare())
}

/// Converts a `tg-data` [`Peer`] into [`PeerSummary`].
#[must_use]
pub fn peer_to_peer_summary(
    peer: &Peer,
    history: Option<&History>,
    self_peer_id: DataPeerId,
    now: i32,
    send_ctx: SendCheckContext,
) -> PeerSummary {
    let is_saved_messages = peer.id() == self_peer_id;
    let title = if is_saved_messages {
        "Saved Messages".to_owned()
    } else if !peer.name().is_empty() {
        peer.name().to_owned()
    } else {
        format!("Peer {}", peer.id().bare())
    };
    let (status, _) = peer_status_text(peer, now);
    let username = peer_primary_username(peer);
    let phone_or_link = match peer {
        Peer::User(u) if !u.phone.is_empty() => Some(u.phone.clone()),
        _ => username.as_deref().map(|u| format!("t.me/{u}")),
    };
    let muted = history.map_or_else(|| peer.common().notify.is_muted(now), |h| h.muted);
    PeerSummary {
        id: from_data_peer(peer.id()),
        kind: peer_coarse_kind(peer),
        initials: initials_from_name(&title),
        title,
        status,
        username,
        phone_or_link,
        about: peer.common().about.clone(),
        avatar_rgb: avatar_rgb_for_color_index(peer.common().color_index),
        muted,
        can_send: peer.can_write(send_ctx),
        is_saved_messages,
    }
}

/// Maps `tg_data::TypingAction` into `dialogs::backend::TypingAction`.
#[must_use]
pub const fn typing_action_to_dialog(action: DataTypingAction) -> Option<DialogTyping> {
    match action {
        DataTypingAction::Cancel => None,
        DataTypingAction::RecordVoice | DataTypingAction::UploadVoice(_) => {
            Some(DialogTyping::RecordingVoice)
        }
        DataTypingAction::UploadPhoto(_)
        | DataTypingAction::UploadVideo(_)
        | DataTypingAction::UploadDocument(_)
        | DataTypingAction::UploadRound(_) => Some(DialogTyping::UploadingPhoto),
        DataTypingAction::ChooseSticker => Some(DialogTyping::ChoosingSticker),
        _ => Some(DialogTyping::Typing),
    }
}

/// Maps `history::backend::TypingAction` into `tg_api::SendProgressKind` (`None` for cancel).
#[must_use]
pub const fn history_typing_to_send_progress(
    action: HistoryTypingAction,
) -> Option<tg_api::SendProgressKind> {
    match action {
        HistoryTypingAction::Typing => Some(tg_api::SendProgressKind::Typing),
        HistoryTypingAction::RecordingVoice => Some(tg_api::SendProgressKind::RecordVoice),
        HistoryTypingAction::UploadingMedia => Some(tg_api::SendProgressKind::UploadFile),
        HistoryTypingAction::Cancel => None,
    }
}

/// Builds a [`TypingState`] for `peer_id` from active typing records in `session`.
#[must_use]
pub fn active_typing_state(session: &Session, peer_id: DataPeerId) -> Option<TypingState> {
    let active = session.active_typing(peer_id);
    for (from_id, _top_msg, raw_action) in active {
        if let Some(action) = typing_action_to_dialog(raw_action) {
            let actor = if peer_id.is_user() {
                None
            } else {
                session.peer(from_id).map(|p| p.name().to_owned())
            };
            return Some(TypingState { action, actor });
        }
    }
    None
}

/// Builds a [`HistoryPeerInfo`] from `tg_data::Session`.
#[must_use]
pub fn peer_to_history_peer_info(
    session: &Session,
    peer_id: DataPeerId,
    now: i32,
) -> Option<HistoryPeerInfo> {
    let peer = session.peer(peer_id)?;
    let history = session.history(peer_id);
    let summary = peer_to_peer_summary(
        peer,
        history,
        session.self_peer_id(),
        now,
        session.send_context(),
    );
    let typing_text = active_typing_state(session, peer_id).map(|t| format!("{}...", t.label()));
    let unread_count = history.and_then(|h| h.unread_count).unwrap_or(0).max(0) as u32;
    let link_base = summary
        .username
        .as_deref()
        .map(|u| format!("https://t.me/{u}"))
        .unwrap_or_else(|| format!("https://t.me/c/{}", summary.id.bare()));

    let (can_pin, can_delete, contact_status) = match peer {
        Peer::User(u) => {
            let bar = peer.common().bar_settings;
            let banner = if peer.common().block_status == BlockStatus::Blocked {
                Some(ContactStatusBanner::Blocked)
            } else if !u.is_self() && !u.is_service_user() {
                if u.is_contact() && bar.contains(tg_data::PeerBarSettings::SHARE_CONTACT) {
                    Some(ContactStatusBanner::SharePhoneNumber {
                        peer_name: summary.title.clone(),
                    })
                } else if !u.is_bot()
                    && !u.is_contact()
                    && (bar.contains(tg_data::PeerBarSettings::ADD_CONTACT)
                        || bar.contains(tg_data::PeerBarSettings::BLOCK_CONTACT))
                {
                    Some(ContactStatusBanner::AddOrBlock {
                        peer_name: summary.title.clone(),
                    })
                } else if bar.contains(tg_data::PeerBarSettings::REPORT_SPAM) {
                    Some(ContactStatusBanner::ReportSpam {
                        peer_name: summary.title.clone(),
                        is_channel: false,
                    })
                } else {
                    None
                }
            } else {
                None
            };
            (true, true, banner)
        }
        Peer::Chat(c) => {
            let banner = if peer
                .common()
                .bar_settings
                .contains(tg_data::PeerBarSettings::REPORT_SPAM)
            {
                Some(ContactStatusBanner::ReportSpam {
                    peer_name: summary.title.clone(),
                    is_channel: false,
                })
            } else {
                None
            };
            (
                c.am_creator()
                    || c.admin_rights
                        .contains(tg_data::ChatAdminRights::PIN_MESSAGES),
                c.can_delete_messages(),
                banner,
            )
        }
        Peer::Channel(ch) => {
            let banner = if peer
                .common()
                .bar_settings
                .contains(tg_data::PeerBarSettings::REPORT_SPAM)
            {
                Some(ContactStatusBanner::ReportSpam {
                    peer_name: summary.title.clone(),
                    is_channel: ch.is_broadcast(),
                })
            } else {
                None
            };
            (
                ch.am_creator()
                    || ch
                        .admin_rights
                        .contains(tg_data::ChatAdminRights::PIN_MESSAGES),
                ch.can_delete_messages(),
                banner,
            )
        }
    };

    Some(HistoryPeerInfo {
        summary,
        typing_text,
        contact_status,
        unread_count,
        can_pin,
        can_delete,
        link_base,
    })
}

/// Converts a `tg_data::Peer` and `History` into a dialogs row [`DialogItem`].
#[must_use]
pub fn dialog_item_from_session(
    session: &Session,
    peer_id: DataPeerId,
    list: DialogListQuery,
    now: i32,
) -> Option<DialogItem> {
    let history = session.history(peer_id)?;
    let peer = session.peer(peer_id);

    let (kind, special, title, username, color_index, verified, premium, online) =
        if let Some(p) = peer {
            let special = if peer_id == session.self_peer_id() {
                SpecialRowKind::SavedMessages
            } else if p.as_user().is_some_and(tg_data::User::is_verify_codes) {
                SpecialRowKind::VerificationCodes
            } else if p.as_user().is_some_and(|u| u.is_replies_chat(false)) {
                SpecialRowKind::Replies
            } else {
                SpecialRowKind::None
            };
            let title = if special == SpecialRowKind::SavedMessages {
                "Saved Messages".to_owned()
            } else if !p.name().is_empty() {
                p.name().to_owned()
            } else {
                format!("Peer {}", peer_id.bare())
            };
            let verified = match p {
                Peer::User(u) => u.is_verified(),
                Peer::Channel(ch) => ch.is_verified(),
                Peer::Chat(_) => false,
            };
            let premium = p.as_user().is_some_and(tg_data::User::is_premium);
            let online = special != SpecialRowKind::SavedMessages
                && p.as_user()
                    .is_some_and(|u| !u.is_self() && u.is_online(now));
            (
                peer_dialog_kind(p),
                special,
                title,
                peer_primary_username(p),
                p.common().color_index % 7,
                verified,
                premium,
                online,
            )
        } else {
            let special = if peer_id == session.self_peer_id() {
                SpecialRowKind::SavedMessages
            } else {
                SpecialRowKind::None
            };
            let title = if special == SpecialRowKind::SavedMessages {
                "Saved Messages".to_owned()
            } else {
                format!("Peer {}", peer_id.bare())
            };
            let kind = if peer_id.is_channel() {
                DialogPeerKind::Channel
            } else if peer_id.is_chat() {
                DialogPeerKind::Group
            } else {
                DialogPeerKind::User
            };
            (
                kind,
                special,
                title,
                None,
                tg_data::decide_color_index(peer_id) % 7,
                false,
                false,
                false,
            )
        };

    let filter_id_raw = match list {
        DialogListQuery::Filter(fid) => fid.0,
        DialogListQuery::Archive => 0,
    };
    let pinned_idx = history.pinned_index(filter_id_raw);
    let pinned = pinned_idx > 0;
    let pinned_order = if pinned {
        Some((pinned_idx - 1) as usize)
    } else {
        None
    };

    let mut folders = Vec::new();
    let mut folder_tags = Vec::new();
    for filter in session.filters().list() {
        if let Some(p) = peer
            && filter.contains(p, history, false)
        {
            folders.push(FilterId(filter.id));
            if session.filters().tags_enabled() {
                folder_tags.push(FolderTagBadge {
                    filter_id: FilterId(filter.id),
                    title: filter.title.text.clone(),
                    color_index: filter.color_index.unwrap_or(5) % 7,
                });
            }
        }
    }

    let loaded_top = history
        .top_message_id
        .and_then(|mid| session.message(tg_data::FullMsgId::new(peer_id, mid)));
    // An item that displays nothing (`item->isEmpty()`) paints no date, preview or ticks in
    // the row (`Dialogs::Ui::PaintRow`), like a chat with no last message.
    let top_is_empty = loaded_top.is_some_and(displays_nothing);
    let top_msg = loaded_top.filter(|msg| !displays_nothing(msg));
    let (last_message_id, last_message, last_sender, last_message_time, send_state) =
        if let Some(msg) = top_msg {
            let preview = crate::preview_text::message_preview(session, msg);
            let sender = crate::preview_text::preview_sender(session, msg);
            let send_state = message_send_state(session, msg);
            (
                from_data_msg(msg.id),
                preview,
                sender,
                unix_to_local_datetime(msg.date),
                send_state,
            )
        } else {
            (
                if top_is_empty {
                    MsgId(0)
                } else {
                    history.top_message_id.map_or(MsgId(0), from_data_msg)
                },
                crate::preview_text::PreviewText::default(),
                None,
                unix_to_local_datetime(history.chat_list_time_id),
                DialogSendState::None,
            )
        };

    let draft_key_local = tg_data::DraftKey::local(DataMsgId::ZERO, DataPeerId::ZERO);
    let draft_key_cloud = tg_data::DraftKey::cloud(DataMsgId::ZERO, DataPeerId::ZERO);
    let draft = history
        .draft(draft_key_local)
        .or_else(|| history.draft(draft_key_cloud))
        .filter(|d| !d.is_empty())
        .map(|d| DialogDraft {
            text: d.text.text.clone(),
            time: unix_to_local_datetime(d.date),
            is_reply: d.reply.is_some(),
            saving: false,
        });

    let data_badges = history.badges_state();
    let badges = DialogBadgesState {
        unread_counter: data_badges.unread_counter.max(0),
        unread: data_badges.unread,
        unread_muted: data_badges.unread_muted,
        mention: data_badges.mention,
        mention_muted: data_badges.mention_muted,
        reaction: data_badges.reaction,
        reaction_muted: data_badges.reaction_muted,
        poll: data_badges.poll,
        poll_muted: data_badges.poll_muted,
    };

    let has_bot_app_button = peer
        .and_then(Peer::as_user)
        .and_then(|u| u.bot_info.as_ref())
        .is_some_and(|b| b.has_main_app);
    let is_contact = peer
        .and_then(Peer::as_user)
        .is_some_and(|u| !u.is_self() && u.is_contact());
    let media_preview = top_msg.and_then(crate::preview_text::message_media_preview);

    Some(DialogItem {
        peer_id: from_data_peer(peer_id),
        kind,
        special,
        title,
        username,
        color_index,
        verified,
        premium,
        emoji_status_id: peer
            .filter(|_| special == SpecialRowKind::None)
            .and_then(|p| crate::peer_emoji_status::row_emoji_status_id(p, now)),
        online,
        active_call: peer.is_some_and(Peer::has_active_group_call),
        muted: history.muted,
        pinned,
        pinned_order,
        archived: history.is_archived(),
        folders,
        folder_tags,
        last_message_id,
        last_message: last_message.text,
        last_message_spoilers: last_message.spoilers,
        last_sender,
        last_message_time,
        send_state,
        draft,
        typing: active_typing_state(session, peer_id),
        badges,
        has_bot_app_button,
        is_contact,
        media_preview,
    })
}

/// Whether outgoing messages to `peer` are always shown as read (`HistoryItem::unread`): bots
/// (except support), broadcast channels and basic groups migrated to a supergroup.
fn outgoing_always_read(peer: Option<&Peer>) -> bool {
    match peer {
        Some(Peer::User(u)) => u.is_bot() && !u.is_support(),
        Some(Peer::Channel(ch)) => !ch.is_megagroup(),
        Some(Peer::Chat(chat)) => chat.is_migrated(),
        None => false,
    }
}

/// Delivery ticks of `msg` in chat-list and search rows: none for incoming messages, sent notes
/// in Saved Messages and items that display nothing, a clock while sending, one tick while the
/// peer has not read it
/// (`HistoryItem::unread`), two ticks otherwise.
#[must_use]
pub fn message_send_state(session: &Session, msg: &tg_data::Message) -> DialogSendState {
    let peer_id = msg.peer_id;
    // A failed send shows the sending clock too (`Dialogs::Ui::PaintRow`).
    let being_sent = msg
        .flags
        .intersects(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED);
    // `item && !item->isEmpty() && item->needCheck()` (`dialogs_layout.cpp`), where
    // `HistoryItem::needCheck` is `(out() && !isEmpty()) || (!isRegular() && peer->isSelf())`:
    // notes in Saved Messages are never `out`, so only a pending one shows the clock.
    if displays_nothing(msg) {
        DialogSendState::None
    } else if peer_id == session.self_peer_id() {
        if being_sent {
            DialogSendState::Sending
        } else {
            DialogSendState::None
        }
    } else if !msg.is_outgoing() {
        DialogSendState::None
    } else if being_sent {
        DialogSendState::Sending
    } else if session
        .history(peer_id)
        .is_some_and(|h| h.is_server_side_unread(msg))
        && !outgoing_always_read(session.peer(peer_id))
    {
        DialogSendState::Sent
    } else {
        DialogSendState::Received
    }
}

/// Converts a dialog in `session` into a legacy shell [`DialogEntry`].
#[must_use]
pub fn dialog_entry_from_session(
    session: &Session,
    peer_id: DataPeerId,
    now: i32,
) -> Option<DialogEntry> {
    let item = dialog_item_from_session(
        session,
        peer_id,
        DialogListQuery::Filter(FilterId::ALL),
        now,
    )?;
    let peer = session.peer(peer_id);
    let history = session.history(peer_id);
    let summary = if let Some(p) = peer {
        peer_to_peer_summary(
            p,
            history,
            session.self_peer_id(),
            now,
            session.send_context(),
        )
    } else {
        PeerSummary {
            id: item.peer_id,
            kind: match item.kind {
                DialogPeerKind::Channel => PeerKind::Channel,
                DialogPeerKind::Group | DialogPeerKind::Forum => PeerKind::Group,
                DialogPeerKind::User | DialogPeerKind::Bot => PeerKind::User,
            },
            initials: initials_from_name(&item.title),
            title: item.title.clone(),
            status: String::new(),
            username: item.username.clone(),
            phone_or_link: None,
            about: String::new(),
            avatar_rgb: avatar_rgb_for_color_index(item.color_index),
            muted: item.muted,
            can_send: true,
            is_saved_messages: item.special == SpecialRowKind::SavedMessages,
        }
    };
    let timestamp = format_dialogs_date_local(item.last_message_time, unix_to_local_datetime(now));
    Some(DialogEntry {
        peer: summary,
        last_message: item.last_message,
        last_sender: item.last_sender,
        timestamp,
        unread_count: item.badges.unread_counter.max(0) as u32,
        pinned: item.pinned,
        folders: item.folders,
    })
}

/// What is known about the message a reply points to (`HistoryMessageReply::resolvedMessage`
/// and `_unavailable` in `history/history_item_components.cpp`).
#[derive(Clone, Copy, Debug)]
pub enum ReplyTargetState<'a> {
    /// The replied message is available locally.
    Found(&'a Message),
    /// The replied message is not loaded yet and is being requested.
    Loading,
    /// The server says the replied message is gone (`messageEmpty`) or unavailable.
    Deleted,
}

impl<'a> ReplyTargetState<'a> {
    /// Looks up `msg`'s reply target in `session` only: a missing target is [`Self::Loading`].
    #[must_use]
    pub fn from_session(session: &'a Session, msg: &Message) -> Self {
        let target = msg
            .reply_to
            .as_ref()
            .and_then(|r| r.target_full_msg_id(msg.peer_id));
        match target.and_then(|id| session.message(id)) {
            Some(found) => Self::Found(found),
            None => Self::Loading,
        }
    }
}

/// What is known about the story a reply points to (`HistoryMessageReply::resolvedStory`).
#[derive(Clone, Copy, Debug)]
pub enum StoryReplyState<'a> {
    /// The story is being requested.
    Loading,
    /// The story exists; its caption may be empty.
    Found {
        /// Story caption text.
        caption: &'a str,
        /// The story's photo or video, shown as the header's preview.
        media: Option<&'a tg_data::MessageMedia>,
    },
    /// The story was deleted or is not available.
    Deleted,
}

/// Builds the reply header of a reply to `story` (`Reply::update` with `resolvedStory`): the
/// story owner's name and `lng_in_dlg_story` ("Story", or "Story, caption"), "Loading..." while
/// it is requested, or `lng_deleted_story`.
#[must_use]
pub fn story_reply_header(
    session: &Session,
    holder: &Message,
    story: tg_data::FullStoryId,
    state: StoryReplyState<'_>,
) -> ReplyHeader {
    let state_header = |text: String| ReplyHeader {
        reply_to_msg_id: MsgId(0),
        author_name: String::new(),
        author_color_id: 0,
        author_accent: None,
        preview_text: text,
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        preview_spoilers: Vec::new(),
        quote_text: None,
    };
    match state {
        StoryReplyState::Loading => state_header(tg_lang::tr::lng_profile_loading()),
        StoryReplyState::Deleted => state_header(tg_lang::tr::lng_deleted_story()),
        StoryReplyState::Found { caption, media } => {
            let author_name = session
                .peer(story.peer)
                .map(|p| p.name().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Peer {}", story.peer.bare()));
            let kind = tg_lang::tr::lng_in_dlg_story();
            let caption = caption.trim().replace(['\r', '\n'], " ");
            let preview_text = if caption.is_empty() {
                kind
            } else {
                tg_lang::tr::lng_dialogs_text_media(
                    &tg_lang::tr::lng_dialogs_text_media_wrapped(&kind),
                    &caption,
                )
            };
            let preview_thumb = crate::reply_preview::reply_thumb(
                holder,
                crate::reply_preview::ReplyTargets {
                    message: None,
                    story: media,
                },
            );
            ReplyHeader {
                reply_to_msg_id: MsgId(0),
                author_name,
                author_color_id: story.peer.bare(),
                author_accent: Some(crate::peer_accent::peer_accent(session, story.peer)),
                preview_text,
                preview_thumb_rgb: preview_thumb
                    .as_ref()
                    .map(|_| avatar_rgb_for_color_index((story.story.unsigned_abs() % 7) as u8)),
                preview_thumb: preview_thumb.flatten(),
                preview_colored_len: 0,
                preview_spoilers: Vec::new(),
                quote_text: None,
            }
        }
    }
}

/// Builds `msg`'s reply header for a known `target` state.
///
/// Port of `HistoryView::Reply::update` / `updateName` (`history_view_reply.cpp`): a resolved
/// target shows its author and a text/media preview; while it loads the header shows
/// `lng_profile_loading`, and a deleted target shows `lng_deleted_message` unless the reply
/// carries its own quote (then the quote is shown under the external sender name). A header
/// with an empty `author_name` is drawn as that one-line state (`Reply::_stateText`).
#[must_use]
pub fn reply_header_for(
    session: &Session,
    msg: &Message,
    target: ReplyTargetState<'_>,
) -> Option<ReplyHeader> {
    let r = msg.reply_to.as_ref()?;
    let Some(target_id) = r.reply_to_msg_id else {
        let story = r.reply_to_story?;
        return Some(story_reply_header(
            session,
            msg,
            story,
            StoryReplyState::Loading,
        ));
    };
    let quote = r
        .quote_text
        .as_ref()
        .map(|q| q.text.clone())
        .filter(|q| !q.trim().is_empty());
    let target = match target {
        ReplyTargetState::Found(orig) if matches!(orig.content, MessageContent::Empty) => {
            ReplyTargetState::Deleted
        }
        other => other,
    };
    let state_header = |text: String| ReplyHeader {
        reply_to_msg_id: from_data_msg(target_id),
        author_name: String::new(),
        author_color_id: 0,
        author_accent: None,
        preview_text: text,
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        preview_spoilers: Vec::new(),
        quote_text: None,
    };
    match target {
        ReplyTargetState::Found(orig) => {
            let author_pid = orig.from_id.unwrap_or(orig.peer_id);
            let author_name = session
                .peer(author_pid)
                .map(|p| p.name().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Peer {}", author_pid.bare()));
            let preview = crate::reply_preview::reply_preview_text(session, orig);
            let preview_thumb = crate::reply_preview::reply_thumb(
                msg,
                crate::reply_preview::ReplyTargets {
                    message: Some(orig),
                    story: None,
                },
            );
            Some(ReplyHeader {
                reply_to_msg_id: from_data_msg(target_id),
                author_name,
                author_color_id: author_pid.bare(),
                author_accent: Some(crate::peer_accent::peer_accent(
                    session,
                    crate::peer_accent::content_colors_from(orig),
                )),
                preview_text: preview.text,
                preview_thumb_rgb: preview_thumb
                    .as_ref()
                    .map(|_| avatar_rgb_for_color_index((target_id.0.unsigned_abs() % 7) as u8)),
                preview_thumb: preview_thumb.flatten(),
                preview_colored_len: preview.colored_len,
                preview_spoilers: preview.spoilers,
                quote_text: quote,
            })
        }
        ReplyTargetState::Loading => Some(state_header(tg_lang::tr::lng_profile_loading())),
        ReplyTargetState::Deleted => {
            let external = r.reply_from.as_ref().and_then(|f| {
                f.from_name.clone().or_else(|| {
                    f.from_id
                        .and_then(|pid| session.peer(pid))
                        .map(|p| p.name().to_owned())
                })
            });
            // `Reply::update`: a reply to a message of another chat shows the quote, or the
            // text of the media sent along with it (`externalMedia->toPreview`), and that
            // media's thumbnail.
            let external_media = r.reply_media.as_deref();
            let media_text = external_media
                .map(|media| crate::preview_text::regular_preview(media, "", false))
                .filter(|text| !text.is_empty());
            let colored_len = if quote.is_none() {
                media_text.as_ref().map_or(0, String::len)
            } else {
                0
            };
            match (
                quote.clone().or(media_text),
                external.filter(|n| !n.is_empty()),
            ) {
                (Some(text), Some(name)) => {
                    let external_sender = r.reply_from.as_ref().and_then(|f| f.from_id);
                    let color = external_sender.map_or(0, DataPeerId::bare);
                    let preview_thumb = crate::reply_preview::reply_thumb(
                        msg,
                        crate::reply_preview::ReplyTargets::default(),
                    );
                    Some(ReplyHeader {
                        reply_to_msg_id: from_data_msg(target_id),
                        author_name: name,
                        author_color_id: color,
                        author_accent: external_sender
                            .map(|pid| crate::peer_accent::peer_accent(session, pid)),
                        preview_text: text,
                        preview_thumb_rgb: preview_thumb.as_ref().map(|_| {
                            avatar_rgb_for_color_index((target_id.0.unsigned_abs() % 7) as u8)
                        }),
                        preview_thumb: preview_thumb.flatten(),
                        preview_colored_len: colored_len,
                        preview_spoilers: Vec::new(),
                        quote_text: quote,
                    })
                }
                _ => Some(state_header(tg_lang::tr::lng_deleted_message())),
            }
        }
    }
}

/// Converts a `tg_data::Message` into a rich [`HistoryMessage`].
#[must_use]
pub fn message_to_history_message(session: &Session, msg: &Message) -> HistoryMessage {
    message_to_history_message_with_stickers(session, msg, &|_| None)
}

/// Converts a `tg_data::Message` into a rich [`HistoryMessage`], resolving cached `.tgs`
/// sticker Lottie JSON via `resolve_lottie`.
#[must_use]
pub fn message_to_history_message_with_stickers(
    session: &Session,
    msg: &Message,
    resolve_lottie: &dyn Fn(&DocumentData) -> Option<String>,
) -> HistoryMessage {
    let default_dice = |emoji: &str, value: i32| {
        (
            None,
            crate::dice_pack::generate_local_dice_idle_lottie(emoji, value),
        )
    };
    message_to_history_message_with_resolvers(session, msg, resolve_lottie, &default_dice)
}

/// Resolver mapping `(dice_emoticon, outcome_value)` to `(optional_doc_id, lottie_json)`.
pub type DiceLottieResolver<'a> = &'a dyn Fn(&str, i32) -> (Option<u64>, String);

/// Converts a `tg_data::Message` into a rich [`HistoryMessage`], resolving both `.tgs`
/// sticker Lottie JSON and official dice sticker pack animations (`InputStickerSetDice`).
#[must_use]
pub fn message_to_history_message_with_resolvers(
    session: &Session,
    msg: &Message,
    resolve_lottie: &dyn Fn(&DocumentData) -> Option<String>,
    resolve_dice: DiceLottieResolver<'_>,
) -> HistoryMessage {
    let peer_id = from_data_peer(msg.peer_id);
    let from_data_id = msg.from_id.unwrap_or(msg.peer_id);
    let from_id = from_data_peer(from_data_id);
    let anonymous_admin = session
        .peer(from_data_id)
        .is_some_and(tg_data::Peer::is_megagroup);
    let sender_name = if let Some(author) = msg.post_author.as_ref().filter(|_| !anonymous_admin) {
        author.clone()
    } else if let Some(p) = session.peer(from_data_id) {
        if p.name().is_empty() {
            format!("Peer {}", from_data_id.bare())
        } else {
            p.name().to_owned()
        }
    } else if msg.is_outgoing() {
        "You".to_owned()
    } else {
        format!("Peer {}", from_data_id.bare())
    };

    let (text, media, service, link_preview) = match &msg.content {
        MessageContent::Empty => (UiText::default(), None, None, None),
        MessageContent::Regular { text, media } => {
            let mut ui_text = data_text_to_ui_text(text);
            let ctx = MediaConvertCtx {
                session,
                msg,
                resolve_lottie,
                resolve_dice,
            };
            let (ui_media, lp, fallback_text) = convert_message_media(media, &ctx);
            if ui_text.text.trim().is_empty()
                && let Some(fallback) = fallback_text
            {
                ui_text = UiText::simple(fallback);
            }
            (ui_text, ui_media, None, lp)
        }
        MessageContent::Service(action) => {
            let ui_service = history_service_action(session, msg, action);
            (UiText::default(), None, Some(ui_service), None)
        }
    };

    let reply = reply_header_for(session, msg, ReplyTargetState::from_session(session, msg));

    let forward = msg.forward_from.as_ref().map(|fwd| {
        let from_name = fwd
            .from_name
            .clone()
            .or_else(|| {
                fwd.from_id
                    .and_then(|pid| session.peer(pid))
                    .map(|p| p.name().to_owned())
            })
            .unwrap_or_else(|| "Forwarded".to_owned());
        ForwardHeader {
            from_name,
            from_peer: fwd.from_id.map(from_data_peer),
            original_date: format_date_time(fwd.date),
        }
    });

    let reactions = msg
        .reactions
        .as_ref()
        .map(crate::inline_reactions::reaction_counters)
        .unwrap_or_default();

    let is_read = session
        .history(msg.peer_id)
        .is_none_or(|h| !h.is_server_side_unread(msg));

    let via_bot = msg.via_bot_id.and_then(|uid| {
        session
            .user(uid)
            .map(|u| u.usernames.username().to_owned())
            .filter(|s| !s.is_empty())
    });

    let media_ttl_seconds = match &msg.content {
        MessageContent::Regular {
            media: MessageMedia::Photo(p),
            ..
        } => p.ttl_seconds,
        MessageContent::Regular {
            media: MessageMedia::Document(d),
            ..
        } => d.ttl_seconds,
        _ => None,
    };

    let poll_extras = match &msg.content {
        MessageContent::Regular {
            media: MessageMedia::Poll(poll),
            ..
        } => Some(tg_app_core::history::sections_model::PollMessageExtras {
            open_answers: false,
            shuffle_answers: false,
            revoting_disabled: false,
            hide_results_until_close: false,
            option_attachments: vec![None; poll.answers.len()],
            option_bytes: poll.answers.iter().map(|a| a.option.clone()).collect(),
            option_added_by: vec![None; poll.answers.len()],
            attached_media: None,
            solution_media: None,
        }),
        _ => None,
    };

    // `Message::hasOutLayout`: in Saved Messages the server's `out` flag does not say who
    // wrote a message (`NewMessageFlags` never sets it there), so only the Saved Messages
    // rule decides the side.
    let outgoing = if msg.peer_id == session.self_peer_id() {
        crate::preview_text::has_saved_messages_out_layout(session, msg)
    } else {
        msg.is_outgoing()
    };
    let extra = MessageExtra {
        thread_id: msg
            .reply_to
            .as_ref()
            .and_then(|r| r.top_msg_id)
            .map(from_data_msg),
        saved_from_peer: msg
            .forward_from
            .as_ref()
            .and_then(|f| f.saved_from_peer)
            .map(from_data_peer),
        scheduled_date: msg.is_scheduled().then_some(msg.date),
        silent: msg.is_silent(),
        effect_id: msg.effect_id.map(|e| e.0),
        via_bot,
        send_as_name: None,
        translated_text: None,
        link_preview,
        replies_count: msg.replies_count.map(|c| c.max(0) as u32),
        reply_markup: msg
            .reply_markup
            .as_ref()
            .map(tg_bots::tl_convert::reply_markup_from_data),
        rich_message: None,
        ttl_seconds: media_ttl_seconds,
        ttl_viewed: false,
        poll_extras,
        round_video_path: None,
        summary_from_language: msg
            .flags
            .contains(MessageFlags::CAN_BE_SUMMARIZED)
            .then(|| "auto".to_owned()),
        send_failed: msg.flags.contains(MessageFlags::SENDING_FAILED),
        sender_badge: crate::sender_badge::sender_badge(session, msg, outgoing && !msg.is_post()),
        sender_status: crate::sender_badge::sender_name_status(
            session,
            from_data_id,
            msg.peer_id,
            now_unix_i32(),
        ),
        sender_accent: Some(crate::peer_accent::peer_accent(session, from_data_id)),
        no_forwards: msg.no_forwards(),
    };

    HistoryMessage {
        id: from_data_msg(msg.id),
        peer_id,
        from_id,
        sender_name,
        date_unix: msg.date,
        outgoing,
        is_post: msg.is_post(),
        text,
        media,
        service,
        reply,
        forward,
        reactions,
        views: msg.views.map(|v| v.max(0) as u32),
        edited: msg.edit_date.is_some() && !msg.flags.contains(MessageFlags::HIDE_EDITED),
        is_read,
        // A failed send keeps the clock (`HistoryView::BottomInfo`, `Flag::Sending`).
        is_sending: msg
            .flags
            .intersects(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED),
        is_pinned: msg.is_pinned(),
        extra,
    }
}

/// Converts a `tg_data::Message` into a legacy shell [`MessageItem`].
#[must_use]
pub fn message_to_stand_in_item(session: &Session, msg: &Message) -> MessageItem {
    let rich = message_to_history_message(session, msg);
    let text = if rich.text.text.is_empty() {
        rich.preview_snippet()
    } else {
        rich.text.text.clone()
    };
    MessageItem {
        id: rich.id,
        peer_id: rich.peer_id,
        sender_name: rich.sender_name,
        outgoing: rich.outgoing,
        text,
        timestamp: format_short_time(msg.date),
        is_read: rich.is_read,
    }
}

struct MediaConvertCtx<'a> {
    session: &'a Session,
    msg: &'a Message,
    resolve_lottie: &'a dyn Fn(&DocumentData) -> Option<String>,
    resolve_dice: DiceLottieResolver<'a>,
}

fn format_currency_amount(currency: &str, amount: i64) -> String {
    if currency.eq_ignore_ascii_case("XTR") {
        return format!("{amount} ⭐");
    }
    let zero_decimal = matches!(
        currency.to_ascii_uppercase().as_str(),
        "JPY" | "KRW" | "VND" | "CLP" | "PYG" | "UGX" | "RWF" | "XOF" | "XAF" | "XPF"
    );
    if zero_decimal {
        format!("{currency} {amount}")
    } else {
        let sign = if amount < 0 { "-" } else { "" };
        let abs = amount.unsigned_abs();
        format!("{currency} {sign}{}.{:02}", abs / 100, abs % 100)
    }
}

fn format_dice_outcome(emoticon: &str, value: i32) -> String {
    match emoticon {
        "🎯" => match value {
            6 => "Bullseye! (6/6)".to_owned(),
            1 => "Missed (1/6)".to_owned(),
            v => format!("Dart hit ({v}/6)"),
        },
        "🏀" => match value {
            4 | 5 => format!("Basket! ({value}/5)"),
            v => format!("Basketball ({v}/5)"),
        },
        "⚽" => match value {
            3..=5 => format!("Goal! ({value}/5)"),
            v => format!("Soccer ({v}/5)"),
        },
        "🎳" => match value {
            6 => "Strike! (6/6)".to_owned(),
            v => format!("Bowling ({v}/6)"),
        },
        "🎰" => match value {
            64 => "Jackpot! (777)".to_owned(),
            1 | 22 | 43 => format!("Win! ({value}/64)"),
            v => format!("Slot machine ({v}/64)"),
        },
        _ => format!("Rolled {value}"),
    }
}

fn dice_sticker_id(emoticon: &str, value: i32) -> u64 {
    let mut hash: u64 = 0xD1CE_0000_0000_0000;
    for b in emoticon.as_bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u64::from(*b));
    }
    hash ^ (value as u64)
}

fn extended_media_to_album_item(
    item: &ExtendedMedia,
    msg_id: MsgId,
    index: usize,
    stars_amount: i64,
) -> AlbumItem {
    let rgb = avatar_rgb_for_color_index(((msg_id.0.unsigned_abs() as usize + index) % 7) as u8);
    let fallback_rgb = (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]);
    match item {
        ExtendedMedia::Preview {
            width,
            height,
            thumb,
            duration,
        } => AlbumItem {
            msg_id,
            width: width.unwrap_or(640).max(1),
            height: height.unwrap_or(480).max(1),
            stripped_thumb: thumb.clone(),
            fallback_rgb,
            video_duration_secs: duration.map(|d| d.max(0) as u32),
            label: format!("Paid media ({stars_amount} ⭐)"),
        },
        ExtendedMedia::Media(inner) => match inner.as_ref() {
            MessageMedia::Photo(photo) => {
                let best = photo
                    .sizes
                    .iter()
                    .max_by_key(|s| s.width.saturating_mul(s.height));
                AlbumItem {
                    msg_id,
                    width: best.map_or(800, |s| s.width.max(1)),
                    height: best.map_or(600, |s| s.height.max(1)),
                    stripped_thumb: photo.stripped_thumb.clone(),
                    fallback_rgb,
                    video_duration_secs: None,
                    label: format!("photo_{}.jpg", photo.id.0),
                }
            }
            MessageMedia::Document(doc) => AlbumItem {
                msg_id,
                width: doc.width.unwrap_or(640).max(1),
                height: doc.height.unwrap_or(360).max(1),
                stripped_thumb: doc.stripped_thumb.clone(),
                fallback_rgb,
                video_duration_secs: doc.duration.map(|d| d.max(0.0).round() as u32),
                label: if doc.file_name.is_empty() {
                    format!("document_{}", doc.id.0)
                } else {
                    doc.file_name.clone()
                },
            },
            _ => AlbumItem {
                msg_id,
                width: 640,
                height: 480,
                stripped_thumb: None,
                fallback_rgb,
                video_duration_secs: None,
                label: format!("Paid media item {}", index + 1),
            },
        },
    }
}

fn convert_message_media(
    media: &MessageMedia,
    ctx: &MediaConvertCtx<'_>,
) -> (
    Option<HistoryMedia>,
    Option<LinkPreviewData>,
    Option<String>,
) {
    match media {
        MessageMedia::Empty | MessageMedia::Unsupported => (None, None, None),
        MessageMedia::Photo(photo) => {
            let best = photo
                .sizes
                .iter()
                .max_by_key(|s| s.width.saturating_mul(s.height));
            let width = best.map_or(800, |s| s.width.max(1));
            let height = best.map_or(600, |s| s.height.max(1));
            let size_bytes = best.map_or(0, |s| s.size.max(0) as u64);
            let rgb = avatar_rgb_for_color_index((photo.id.0 % 7) as u8);
            (
                Some(HistoryMedia::Photo(PhotoAttachment {
                    width,
                    height,
                    stripped_thumb: photo.stripped_thumb.clone(),
                    fallback_rgb: (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]),
                    file_name: format!("photo_{}.jpg", photo.id.0),
                    size_bytes,
                })),
                None,
                None,
            )
        }
        MessageMedia::Document(doc) => {
            let rgb = avatar_rgb_for_color_index((doc.id.0 % 7) as u8);
            let fallback_rgb = (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]);
            let file_name = if doc.file_name.is_empty() {
                format!("document_{}", doc.id.0)
            } else {
                doc.file_name.clone()
            };
            let duration_secs = doc.duration.unwrap_or(0.0).max(0.0).round() as u32;
            let hist_media = match doc.kind {
                DocumentKind::Video | DocumentKind::Gif => HistoryMedia::Video(VideoAttachment {
                    width: doc.width.unwrap_or(640).max(1),
                    height: doc.height.unwrap_or(360).max(1),
                    duration_secs,
                    size_bytes: doc.size.max(0) as u64,
                    is_gif: doc.kind == DocumentKind::Gif,
                    stripped_thumb: doc.stripped_thumb.clone(),
                    fallback_rgb,
                    file_name,
                }),
                DocumentKind::Voice => HistoryMedia::Voice(VoiceAttachment {
                    duration_secs,
                    waveform_5bit: doc.waveform.clone().unwrap_or_default(),
                    size_bytes: doc.size.max(0) as u64,
                    is_unread: ctx.msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD),
                    transcription: None,
                }),
                DocumentKind::RoundVideo => HistoryMedia::RoundVideo(RoundVideoAttachment {
                    duration_secs,
                    diameter_px: doc.width.unwrap_or(240).max(120),
                    accent_rgb: fallback_rgb,
                }),
                DocumentKind::Sticker
                | DocumentKind::AnimatedSticker
                | DocumentKind::VideoSticker => {
                    let emoji = doc
                        .sticker
                        .as_ref()
                        .map(|s| s.alt.clone())
                        .unwrap_or_default();
                    let lottie_json = if doc.kind == DocumentKind::AnimatedSticker {
                        (ctx.resolve_lottie)(doc).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    HistoryMedia::Sticker(StickerAttachment {
                        sticker_id: doc.id.0,
                        emoji,
                        set_title: String::new(),
                        width: doc.width.unwrap_or(512).max(1),
                        height: doc.height.unwrap_or(512).max(1),
                        lottie_json,
                        set: doc.sticker.as_ref().filter(|s| s.set_id != 0).map(|s| {
                            tg_app_core::history::backend::StickerSetId {
                                id: s.set_id,
                                access_hash: s.set_access_hash,
                            }
                        }),
                    })
                }
                DocumentKind::File | DocumentKind::Audio | DocumentKind::WallPaper => {
                    HistoryMedia::Document(DocumentAttachment {
                        file_name,
                        size_bytes: doc.size.max(0) as u64,
                        mime_type: doc.mime_type.clone(),
                        thumb_rgb: (doc.stripped_thumb.is_some() || !doc.thumbs.is_empty())
                            .then_some(rgb),
                    })
                }
            };
            (Some(hist_media), None, None)
        }
        MessageMedia::WebPage(wp) if !crate::web_page_view::web_page_is_drawn(wp) => {
            (None, None, None)
        }
        MessageMedia::WebPage(wp) => {
            let lp = LinkPreviewData {
                url: wp.url.clone(),
                site_name: crate::web_page_view::displayed_site_name(wp),
                title: crate::web_page_view::displayed_title(wp),
                description: wp.description.clone().unwrap_or_default(),
                large_media: wp
                    .flags
                    .contains(tg_data::MediaWebPageFlags::FORCE_LARGE_MEDIA)
                    && !wp
                        .flags
                        .contains(tg_data::MediaWebPageFlags::FORCE_SMALL_MEDIA),
                above_text: ctx.msg.flags.contains(MessageFlags::INVERT_MEDIA),
                thumb_rgb: Some(avatar_rgb_for_color_index((wp.id.0 % 7) as u8)),
            };
            (None, Some(lp), None)
        }
        MessageMedia::Poll(poll) => {
            let options = poll
                .answers
                .iter()
                .map(|ans| {
                    let tally = poll.results.results.iter().find(|r| r.option == ans.option);
                    PollOption {
                        text: ans.text.text.clone(),
                        votes: tally.map_or(0, |t| t.voters.max(0) as u32),
                        chosen: tally.is_some_and(|t| t.chosen),
                        correct: tally.is_some_and(|t| t.correct),
                    }
                })
                .collect();
            (
                Some(HistoryMedia::Poll(PollAttachment {
                    question: poll.question.text.clone(),
                    is_quiz: poll.quiz,
                    is_anonymous: !poll.public_voters,
                    multiple_choice: poll.multiple_choice,
                    closed: poll.closed,
                    options,
                    total_voters: poll.results.total_voters.unwrap_or(0).max(0) as u32,
                    explanation: poll.results.solution.as_ref().map(|s| s.text.clone()),
                })),
                None,
                None,
            )
        }
        MessageMedia::Location(pt) => (
            Some(HistoryMedia::Location(LocationAttachment {
                lat: pt.lat,
                lon: pt.lon,
                venue_title: None,
                venue_address: None,
                live_period_secs: None,
            })),
            None,
            None,
        ),
        MessageMedia::LiveLocation(live) => (
            Some(HistoryMedia::Location(LocationAttachment {
                lat: live.geo.lat,
                lon: live.geo.lon,
                venue_title: None,
                venue_address: None,
                live_period_secs: Some(live.period.max(0) as u32),
            })),
            None,
            None,
        ),
        MessageMedia::Venue(v) => (
            Some(HistoryMedia::Location(LocationAttachment {
                lat: v.geo.lat,
                lon: v.geo.lon,
                venue_title: Some(v.title.clone()),
                venue_address: Some(v.address.clone()),
                live_period_secs: None,
            })),
            None,
            None,
        ),
        MessageMedia::Contact(c) => (
            Some(HistoryMedia::Contact(ContactAttachment {
                first_name: c.first_name.clone(),
                last_name: c.last_name.clone(),
                phone_number: c.phone_number.clone(),
                user_id: (!c.user_id.is_zero())
                    .then(|| from_data_peer(DataPeerId::from_user(c.user_id))),
            })),
            None,
            None,
        ),
        MessageMedia::Game(g) => {
            let bot_username = ctx
                .msg
                .via_bot_id
                .and_then(|uid| ctx.session.user(uid))
                .map(|u| u.usernames.username().to_owned())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    ctx.msg
                        .from_id
                        .and_then(|pid| ctx.session.peer(pid))
                        .and_then(peer_primary_username)
                })
                .unwrap_or_default();
            let (media_width, media_height) = if let Some(ref doc) = g.document {
                (
                    doc.width.unwrap_or(640).max(1),
                    doc.height.unwrap_or(360).max(1),
                )
            } else if let Some(ref photo) = g.photo {
                let best = photo
                    .sizes
                    .iter()
                    .max_by_key(|s| s.width.saturating_mul(s.height));
                (
                    best.map_or(640, |s| s.width.max(1)),
                    best.map_or(360, |s| s.height.max(1)),
                )
            } else {
                (640, 360)
            };
            let stripped_thumb = g
                .document
                .as_ref()
                .and_then(|d| d.stripped_thumb.clone())
                .or_else(|| g.photo.as_ref().and_then(|p| p.stripped_thumb.clone()));
            let rgb = avatar_rgb_for_color_index((g.id.0 % 7) as u8);
            (
                Some(HistoryMedia::Game(GameAttachment {
                    game: tg_bots::mini_app::GameData {
                        id: tg_bots::stand_in::GameId(g.id.0),
                        access_hash: g.access_hash,
                        short_name: g.short_name.clone(),
                        title: g.title.clone(),
                        description: g.description.clone(),
                        photo_id: g.photo.as_ref().map(|p| tg_bots::stand_in::PhotoId(p.id.0)),
                        document_id: g
                            .document
                            .as_ref()
                            .map(|d| tg_bots::stand_in::DocumentId(d.id.0)),
                    },
                    bot_username,
                    media_width,
                    media_height,
                    is_animation: g.document.is_some(),
                    stripped_thumb,
                    fallback_rgb: (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]),
                    high_scores: Vec::new(),
                })),
                None,
                None,
            )
        }
        MessageMedia::Dice(d) => {
            let outcome = format_dice_outcome(&d.emoticon, d.value);
            let (doc_id, lottie_json) = (ctx.resolve_dice)(&d.emoticon, d.value);
            (
                Some(HistoryMedia::Sticker(StickerAttachment {
                    sticker_id: doc_id.unwrap_or_else(|| dice_sticker_id(&d.emoticon, d.value)),
                    emoji: d.emoticon.clone(),
                    set_title: outcome,
                    width: 160,
                    height: 160,
                    lottie_json,
                    set: None,
                })),
                None,
                None,
            )
        }
        MessageMedia::PaidMedia(paid) => {
            let msg_id = from_data_msg(ctx.msg.id);
            let paid_url = format!(
                "paid_media:{}:{}:{}",
                ctx.msg.peer_id.0, ctx.msg.id.0, paid.stars_amount
            );
            let has_locked = paid
                .extended_media
                .iter()
                .any(|m| matches!(m, ExtendedMedia::Preview { .. }));
            let locked_lp = has_locked.then(|| LinkPreviewData {
                url: paid_url.clone(),
                site_name: "Paid Media".to_owned(),
                title: format!("Unlock for {} ⭐", paid.stars_amount),
                description: format!("{} paid media item(s)", paid.extended_media.len()),
                thumb_rgb: Some(avatar_rgb_for_color_index(
                    (paid.stars_amount.unsigned_abs() % 7) as u8,
                )),
                large_media: true,
                above_text: false,
            });
            match paid.extended_media.as_slice() {
                [] => (
                    None,
                    Some(LinkPreviewData {
                        url: paid_url,
                        site_name: "Paid Media".to_owned(),
                        title: format!("Unlock for {} ⭐", paid.stars_amount),
                        description: "Paid media content".to_owned(),
                        thumb_rgb: Some(avatar_rgb_for_color_index(1)),
                        large_media: false,
                        above_text: false,
                    }),
                    Some(format!("⭐ Paid media ({} Stars)", paid.stars_amount)),
                ),
                [ExtendedMedia::Media(inner)] => {
                    let (m, lp, fb) = convert_message_media(inner, ctx);
                    (m, lp.or(locked_lp), fb)
                }
                [
                    ExtendedMedia::Preview {
                        width,
                        height,
                        thumb,
                        duration,
                    },
                ] => {
                    let rgb =
                        avatar_rgb_for_color_index((paid.stars_amount.unsigned_abs() % 7) as u8);
                    let fallback_rgb = (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]);
                    let m = if let Some(dur) = duration {
                        HistoryMedia::Video(VideoAttachment {
                            width: width.unwrap_or(640).max(1),
                            height: height.unwrap_or(360).max(1),
                            duration_secs: (*dur).max(0) as u32,
                            size_bytes: 0,
                            is_gif: false,
                            stripped_thumb: thumb.clone(),
                            fallback_rgb,
                            file_name: format!("Unlock for {} ⭐", paid.stars_amount),
                        })
                    } else {
                        HistoryMedia::Photo(PhotoAttachment {
                            width: width.unwrap_or(640).max(1),
                            height: height.unwrap_or(480).max(1),
                            stripped_thumb: thumb.clone(),
                            fallback_rgb,
                            file_name: format!("Unlock for {} ⭐", paid.stars_amount),
                            size_bytes: 0,
                        })
                    };
                    (Some(m), locked_lp, None)
                }
                many => {
                    let items: Vec<AlbumItem> = many
                        .iter()
                        .enumerate()
                        .map(|(idx, em)| {
                            extended_media_to_album_item(em, msg_id, idx, paid.stars_amount)
                        })
                        .collect();
                    (Some(HistoryMedia::Album(items)), locked_lp, None)
                }
            }
        }
        MessageMedia::Invoice(inv) => {
            let price = format_currency_amount(&inv.currency, inv.total_amount);
            let site_name = if inv.receipt_msg_id.is_some() {
                "Payment Receipt".to_owned()
            } else if inv.test {
                "Test Invoice".to_owned()
            } else {
                "Invoice".to_owned()
            };
            let title = if inv.title.is_empty() {
                price.clone()
            } else {
                format!("{} — {}", inv.title, price)
            };
            let url = if !inv.start_param.is_empty() {
                format!("tg://invoice?slug={}", inv.start_param)
            } else if let Some(receipt_id) = inv.receipt_msg_id {
                format!("receipt:{}:{}", ctx.msg.peer_id.0, receipt_id)
            } else {
                format!("msg:{}:{}", ctx.msg.peer_id.0, ctx.msg.id.0)
            };
            let lp = LinkPreviewData {
                url,
                site_name: site_name.clone(),
                title: title.clone(),
                description: inv.description.clone(),
                thumb_rgb: Some(avatar_rgb_for_color_index(3)),
                large_media: inv.extended_media.is_some(),
                above_text: false,
            };
            let attached_media = match inv.extended_media.as_deref() {
                Some(ExtendedMedia::Media(inner)) => convert_message_media(inner, ctx).0,
                Some(ExtendedMedia::Preview {
                    width,
                    height,
                    thumb,
                    ..
                }) => {
                    let rgb = avatar_rgb_for_color_index(3);
                    Some(HistoryMedia::Photo(PhotoAttachment {
                        width: width.unwrap_or(640).max(1),
                        height: height.unwrap_or(480).max(1),
                        stripped_thumb: thumb.clone(),
                        fallback_rgb: (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]),
                        file_name: title.clone(),
                        size_bytes: 0,
                    }))
                }
                None => None,
            };
            let summary = if inv.description.is_empty() {
                format!("💳 {site_name}: {title}")
            } else {
                format!("💳 {site_name}: {title}\n{}", inv.description)
            };
            (attached_media, Some(lp), Some(summary))
        }
        MessageMedia::Story(st) => {
            let owner_name = peer_display_title(ctx.session, st.peer_id);
            let accent_rgb = avatar_rgb_for_color_index((st.peer_id.bare() % 7) as u8);
            (
                Some(HistoryMedia::Story(StoryAttachment {
                    peer_id: from_data_peer(st.peer_id),
                    story_id: st.story_id,
                    owner_name,
                    via_mention: st.via_mention,
                    expired: false,
                    accent_rgb,
                })),
                None,
                None,
            )
        }
        MessageMedia::Giveaway(g) => {
            let channels: Vec<PeerId> = g
                .channels
                .iter()
                .map(|c| from_data_peer(DataPeerId::from_channel(*c)))
                .collect();
            let channel_names: Vec<String> = g
                .channels
                .iter()
                .map(|c| peer_display_title(ctx.session, DataPeerId::from_channel(*c)))
                .collect();
            let until_date_text = if g.until_date > 0 {
                format_date_time(g.until_date)
            } else {
                String::new()
            };
            (
                Some(HistoryMedia::Giveaway(GiveawayAttachment {
                    channels,
                    channel_names,
                    countries_iso2: g.countries_iso2.clone(),
                    prize_description: g.prize_description.clone(),
                    quantity: g.quantity,
                    months: g.months,
                    stars: g.stars,
                    until_date: g.until_date,
                    until_date_text,
                    only_new_subscribers: g.only_new_subscribers,
                    winners_are_visible: g.winners_are_visible,
                })),
                None,
                None,
            )
        }
        MessageMedia::GiveawayResults(r) => {
            let data_channel = DataPeerId::from_channel(r.channel_id);
            let channel_name = peer_display_title(ctx.session, data_channel);
            let winners: Vec<(PeerId, String)> = r
                .winners
                .iter()
                .map(|uid| {
                    let dp = DataPeerId::from_user(*uid);
                    (from_data_peer(dp), peer_display_title(ctx.session, dp))
                })
                .collect();
            (
                Some(HistoryMedia::GiveawayResults(GiveawayResultsAttachment {
                    channel_id: from_data_peer(data_channel),
                    channel_name,
                    launch_msg_id: from_data_msg(r.launch_msg_id),
                    additional_peers_count: r.additional_peers_count,
                    winners_count: r.winners_count,
                    unclaimed_count: r.unclaimed_count,
                    winners,
                    months: r.months,
                    stars: r.stars,
                    prize_description: r.prize_description.clone(),
                    until_date: r.until_date,
                    refunded: r.refunded,
                })),
                None,
                None,
            )
        }
        MessageMedia::TodoList(todo) => {
            let items = todo
                .items
                .iter()
                .map(|it| {
                    let completion = todo.completions.iter().find(|c| c.id == it.id);
                    tg_app_core::core_types::dto::todo::TodoTaskItem {
                        id: it.id,
                        text: data_text_to_ui_text(&it.title),
                        completed_by: completion.map(|c| from_data_peer(c.completed_by)),
                        completed_by_name: None,
                        completion_date: completion.map_or(0, |c| c.date),
                    }
                })
                .collect();
            (
                Some(HistoryMedia::TodoList(
                    tg_app_core::core_types::dto::todo::TodoListAttachment {
                        title: data_text_to_ui_text(&todo.title),
                        items,
                        others_can_append: todo.others_can_append,
                        others_can_complete: todo.others_can_complete,
                    },
                )),
                None,
                None,
            )
        }
        MessageMedia::Widget(w) => {
            let html = tg_tgw::unpack_payload(&w.payload)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            let title = html
                .as_deref()
                .and_then(tg_tgw::extract_html_title)
                .unwrap_or_else(|| "Interactive Widget".to_owned());
            let preview_text = html
                .as_deref()
                .map(|h| tg_tgw::extract_html_text_preview(h, 180))
                .unwrap_or_default();
            (
                Some(HistoryMedia::Widget(WidgetAttachment {
                    widget_id: w.widget_id,
                    width: w.width,
                    height: w.height,
                    payload: w.payload.clone(),
                    html,
                    title,
                    preview_text,
                    is_copy: w.is_copy,
                    can_query: !w.is_copy,
                    background_query: false,
                    public_data: None,
                    public_data_hash: None,
                    chat_data: None,
                })),
                None,
                None,
            )
        }
        MessageMedia::VideoStream(vs) => {
            let owner = peer_display_title(ctx.session, ctx.msg.peer_id);
            let title = if vs.rtmp_stream {
                format!("{owner} · Live RTMP Stream")
            } else {
                format!("{owner} · Live Stream")
            };
            (
                Some(HistoryMedia::VideoStream(VideoStreamAttachment {
                    call_id: vs.call_id.0 as i64,
                    access_hash: vs.access_hash,
                    rtmp_stream: vs.rtmp_stream,
                    title,
                    is_live: true,
                })),
                None,
                None,
            )
        }
    }
}

/// History service pill for `action`: tdesktop's text where `preview_text` knows it, otherwise the
/// structured [`UiServiceAction`].
fn history_service_action(
    session: &Session,
    msg: &Message,
    action: &DataServiceAction,
) -> UiServiceAction {
    if let DataServiceAction::PinMessage = action
        && let Some(target) = msg.reply_to.as_ref().and_then(|r| r.reply_to_msg_id)
    {
        let peer = msg
            .reply_to
            .as_ref()
            .and_then(|r| r.reply_to_peer_id)
            .unwrap_or(msg.peer_id);
        if let Some(pinned) = session.message(tg_data::FullMsgId::new(peer, target))
            && let MessageContent::Regular {
                media: MessageMedia::Empty | MessageMedia::WebPage(_),
                ..
            } = &pinned.content
        {
            let text = crate::preview_text::message_preview_text(session, pinned);
            let chars: Vec<char> = text.chars().collect();
            let preview = if chars.len() > 21 {
                let mut cut: String = chars[..16].iter().collect();
                cut.push('\u{2026}');
                cut
            } else {
                text
            };
            return UiServiceAction::PinnedMessage {
                msg_id: from_data_msg(target),
                preview,
            };
        }
    }
    match crate::preview_text::known_service_text(session, msg, action) {
        Some(text) => UiServiceAction::Custom(text),
        None => convert_service_action(action, session),
    }
}

/// Structured [`UiServiceAction`] for `action` (no message context).
pub(crate) fn service_action_to_ui(
    action: &DataServiceAction,
    session: &Session,
) -> UiServiceAction {
    convert_service_action(action, session)
}

fn convert_service_action(action: &DataServiceAction, session: &Session) -> UiServiceAction {
    match action {
        DataServiceAction::ChatCreate { title, .. }
        | DataServiceAction::ChannelCreate { title } => UiServiceAction::ChatCreated {
            title: title.clone(),
        },
        DataServiceAction::ChatEditTitle { title } => UiServiceAction::TitleChanged {
            title: title.clone(),
        },
        DataServiceAction::PinMessage => UiServiceAction::PinnedMessage {
            msg_id: MsgId(0),
            preview: "a message".to_owned(),
        },
        DataServiceAction::ChatJoinedByLink { .. } | DataServiceAction::ChatJoinedByRequest => {
            UiServiceAction::UserJoined {
                name: "A user".to_owned(),
            }
        }
        DataServiceAction::ChatAddUser { users } => {
            let names: Vec<String> = users
                .iter()
                .map(|uid| {
                    crate::preview_text::peer_real_name(session, DataPeerId::from_user(*uid))
                })
                .collect();
            if names.len() == 1 {
                UiServiceAction::UserJoined {
                    name: names[0].clone(),
                }
            } else {
                UiServiceAction::Custom(format!("Added {}", names.join(", ")))
            }
        }
        DataServiceAction::ContactSignUp => UiServiceAction::Custom("joined Telegram".to_owned()),
        DataServiceAction::PhoneCall {
            video, duration, ..
        } => {
            let kind = if *video { "Video call" } else { "Voice call" };
            match duration {
                Some(secs) => UiServiceAction::Custom(format!("{kind} ({secs}s)")),
                None => UiServiceAction::Custom(kind.to_owned()),
            }
        }
        DataServiceAction::TodoCompletions {
            completed,
            incompleted,
        } => UiServiceAction::TodoCompletions {
            completed: completed.clone(),
            incompleted: incompleted.clone(),
        },
        DataServiceAction::TodoAppendTasks { list } => UiServiceAction::TodoAppendTasks {
            titles: list.iter().map(|i| i.title.text.clone()).collect(),
        },
        DataServiceAction::SuggestedPostApproval {
            rejected,
            balance_too_low,
            reject_comment,
            schedule_date,
            price,
        } => UiServiceAction::SuggestedPostApproval {
            rejected: *rejected,
            balance_too_low: *balance_too_low,
            reject_comment: reject_comment.clone(),
            schedule_date: *schedule_date,
            price_label: price.as_ref().map(format_data_stars_amount),
        },
        DataServiceAction::SuggestedPostSuccess { price } => {
            UiServiceAction::SuggestedPostSuccess {
                price_label: format_data_stars_amount(price),
            }
        }
        DataServiceAction::SuggestedPostRefund { payer_initiated } => {
            UiServiceAction::SuggestedPostRefund {
                payer_initiated: *payer_initiated,
            }
        }
        DataServiceAction::NoForwardsToggle {
            prev_value,
            new_value,
        } => UiServiceAction::NoForwardsToggle {
            prev_value: *prev_value,
            new_value: *new_value,
        },
        DataServiceAction::NoForwardsRequest {
            expired,
            prev_value,
            new_value,
        } => UiServiceAction::NoForwardsRequest {
            expired: *expired,
            prev_value: *prev_value,
            new_value: *new_value,
        },
        DataServiceAction::SuggestBirthday { birthday } => UiServiceAction::SuggestBirthday {
            day: birthday.day.clamp(1, 31),
            month: birthday.month.clamp(1, 12),
            year: (birthday.year != 0).then_some(birthday.year),
        },
        DataServiceAction::SuggestProfilePhoto { photo } => {
            let (photo_id, access_hash, file_reference, is_video, stripped_thumb, fallback_rgb) =
                if let Some(p) = photo.as_deref() {
                    let rgb = avatar_rgb_for_color_index((p.id.0 % 7) as u8);
                    (
                        p.id.0 as i64,
                        p.access_hash as i64,
                        p.file_reference.clone(),
                        p.live_photo_video_id.is_some(),
                        p.stripped_thumb.clone(),
                        (rgb, [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]),
                    )
                } else {
                    (
                        0,
                        0,
                        Vec::new(),
                        false,
                        None,
                        ([0x4A, 0x8F, 0xC4], [0x2A, 0x50, 0x7A]),
                    )
                };
            UiServiceAction::SuggestProfilePhoto {
                photo_id,
                access_hash,
                file_reference,
                is_video,
                stripped_thumb,
                fallback_rgb,
            }
        }
        _ => UiServiceAction::Custom("Service message".to_owned()),
    }
}

fn format_data_stars_amount(price: &tg_data::StarsAmount) -> String {
    let sp = if price.is_ton {
        tg_app_core::core_types::dto::monoforum::SuggestedPrice::ton_nanos(
            price
                .amount
                .saturating_mul(tg_app_core::core_types::dto::monoforum::NANOS_PER_TON)
                .saturating_add(i64::from(price.nanos)),
        )
    } else {
        tg_app_core::core_types::dto::monoforum::SuggestedPrice::stars(price.amount)
    };
    sp.format_label()
}

/// Converts a `tg_data::Message` into an [`IncomingMessageNotification`] for `NotifyBackend`.
#[must_use]
pub fn message_to_incoming_notification(
    session: &Session,
    msg: &Message,
    session_id: u64,
) -> IncomingMessageNotification {
    let peer_id = from_data_peer(msg.peer_id);
    let peer = session.peer(msg.peer_id);
    let peer_kind = peer.map_or(PeerKind::User, peer_coarse_kind);
    let chat_title = peer_display_title(session, msg.peer_id);
    let sender_data_id = msg.from_id.unwrap_or(msg.peer_id);
    let sender_name = session
        .peer(sender_data_id)
        .map(|p| p.name().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| chat_title.clone());

    let (text, spoiler_ranges, media_kind, is_pinned, is_contact_signup, notification_type) =
        match &msg.content {
            MessageContent::Regular { text, media } => {
                let spoilers = text
                    .entities
                    .iter()
                    .filter(|e| matches!(e.kind, EntityKind::Spoiler))
                    .map(|e| TextSpoilerRange {
                        char_offset: e.offset.max(0) as usize,
                        char_len: e.length.max(0) as usize,
                    })
                    .collect();
                let mk = match media {
                    MessageMedia::Empty | MessageMedia::WebPage(_) | MessageMedia::Unsupported => {
                        NotificationMediaKind::Text
                    }
                    MessageMedia::Photo(_) => NotificationMediaKind::Photo,
                    MessageMedia::Document(doc) => match doc.kind {
                        DocumentKind::Voice => NotificationMediaKind::VoiceMessage,
                        DocumentKind::RoundVideo => NotificationMediaKind::VideoMessage,
                        DocumentKind::Gif => NotificationMediaKind::Gif,
                        DocumentKind::Video => NotificationMediaKind::Video,
                        DocumentKind::Sticker
                        | DocumentKind::AnimatedSticker
                        | DocumentKind::VideoSticker => NotificationMediaKind::Sticker(
                            doc.sticker
                                .as_ref()
                                .map(|s| s.alt.clone())
                                .unwrap_or_default(),
                        ),
                        _ => NotificationMediaKind::Document,
                    },
                    MessageMedia::Contact(c) => NotificationMediaKind::Contact(
                        format!("{} {}", c.first_name, c.last_name)
                            .trim()
                            .to_owned(),
                    ),
                    MessageMedia::Location(_)
                    | MessageMedia::LiveLocation(_)
                    | MessageMedia::Venue(_) => NotificationMediaKind::Location,
                    MessageMedia::Poll(p) => NotificationMediaKind::Poll {
                        question: p.question.text.clone(),
                        is_quiz: p.quiz,
                    },
                    MessageMedia::Game(_) => NotificationMediaKind::Game,
                    MessageMedia::Invoice(_) => NotificationMediaKind::Invoice,
                    _ => NotificationMediaKind::Document,
                };
                (
                    text.text.clone(),
                    spoilers,
                    mk,
                    false,
                    false,
                    ItemNotificationType::Message,
                )
            }
            MessageContent::Service(act) => {
                let pinned = matches!(**act, DataServiceAction::PinMessage);
                let signup = matches!(**act, DataServiceAction::ContactSignUp);
                let ntype = if pinned {
                    ItemNotificationType::PinnedMessage
                } else {
                    ItemNotificationType::Message
                };
                (
                    tg_session::runtime::format_message_preview(msg),
                    Vec::new(),
                    NotificationMediaKind::Text,
                    pinned,
                    signup,
                    ntype,
                )
            }
            MessageContent::Empty => (
                String::new(),
                Vec::new(),
                NotificationMediaKind::Text,
                false,
                false,
                ItemNotificationType::Message,
            ),
        };

    let is_self_chat = msg.peer_id == session.self_peer_id();
    let is_broadcast = peer
        .and_then(Peer::as_channel)
        .is_some_and(tg_data::Channel::is_broadcast);
    let is_notifications_service = peer
        .and_then(Peer::as_user)
        .is_some_and(|u| u.is_notifications_user() || u.is_verify_codes());
    let sender_blocked = session
        .peer(sender_data_id)
        .is_some_and(|p| p.common().block_status == BlockStatus::Blocked);
    let am_in_chat = match peer {
        Some(Peer::Chat(c)) => c.am_in(),
        Some(Peer::Channel(ch)) => ch.am_in(),
        _ => true,
    };

    IncomingMessageNotification {
        session_id,
        peer_id,
        peer_kind,
        topic_root_id: msg
            .reply_to
            .as_ref()
            .and_then(|r| r.top_msg_id)
            .map_or(MsgId(0), from_data_msg),
        monoforum_peer_id: PeerId::ZERO,
        msg_id: from_data_msg(msg.id),
        notification_type,
        date_unix: i64::from(msg.date),
        sender_id: from_data_peer(sender_data_id),
        sender_name,
        chat_title,
        topic_title: None,
        text,
        spoiler_ranges,
        media_kind,
        outgoing: msg.is_outgoing(),
        silent: msg.is_silent(),
        mentions_me: msg.mentions_me(),
        is_reply_to_me: msg.reply_to.as_ref().is_some_and(|r| {
            r.reply_to_msg_id
                .and_then(|id| session.message(tg_data::FullMsgId::new(msg.peer_id, id)))
                .is_some_and(Message::is_outgoing)
        }),
        is_pinned,
        is_from_scheduled: msg.is_from_scheduled(),
        is_contact_signup: is_contact_signup
            || msg.flags.contains(MessageFlags::IS_CONTACT_SIGN_UP),
        is_imported_forward: msg.forward_from.as_ref().is_some_and(|f| f.imported),
        is_forwarded: msg.forward_from.is_some(),
        group_id: msg.grouped_id.unwrap_or(0),
        is_self_chat,
        is_broadcast,
        is_notifications_service,
        can_send_texts: peer.is_none_or(|p| p.can_write(session.send_context())),
        slowmode_seconds_left: peer.and_then(Peer::as_channel).map_or(0, |ch| {
            ch.slowmode_seconds_left(now_unix_i32()).max(0) as u32
        }),
        stars_per_message: match peer {
            Some(Peer::User(u)) => u.stars_per_message.max(0) as u32,
            Some(Peer::Channel(ch)) => ch.stars_per_message.max(0) as u32,
            _ => 0,
        },
        reaction_emoji: None,
        poll_vote_option: None,
        sender_blocked,
        am_in_chat,
    }
}

/// Converts a `tg_data::Peer` into [`NotifyPeerInfo`].
#[must_use]
pub fn peer_to_notify_info(session: &Session, peer_id: DataPeerId) -> Option<NotifyPeerInfo> {
    let peer = session.peer(peer_id)?;
    let title = peer_display_title(session, peer_id);
    Some(NotifyPeerInfo {
        id: from_data_peer(peer_id),
        kind: peer_coarse_kind(peer),
        initials: initials_from_name(&title),
        title,
        avatar_rgb: avatar_rgb_for_color_index(peer.common().color_index),
        is_self: peer_id == session.self_peer_id(),
        is_replies_chat: peer.as_user().is_some_and(|u| u.is_replies_chat(false)),
        is_blocked: peer.common().block_status == BlockStatus::Blocked,
        community_id: match peer {
            Peer::User(u) => u
                .linked_community_id
                .map(|c| from_data_peer(DataPeerId::from_channel(c))),
            Peer::Channel(ch) => ch
                .linked_community_id
                .map(|c| from_data_peer(DataPeerId::from_channel(c))),
            Peer::Chat(_) => None,
        },
    })
}

/// Converts a `tg_data::Peer` into an `info::backend::PeerProfile`.
#[must_use]
pub fn peer_to_info_profile(
    session: &Session,
    peer_id: DataPeerId,
    now: i32,
) -> Option<InfoPeerProfile> {
    let peer = session.peer(peer_id)?;
    let kind = peer_info_kind(peer, session.self_peer_id());
    let title = peer_display_title(session, peer_id);
    let (status, is_online) = peer_status_text(peer, now);
    let history = session.history(peer_id);
    let muted = history.map_or_else(|| peer.common().notify.is_muted(now), |h| h.muted);

    let (
        verified,
        premium,
        phone,
        usernames,
        birthday,
        invite_link,
        is_contact,
        can_edit,
        can_add_members,
        can_manage_admins,
        can_manage_permissions,
        is_public,
        no_forwards,
        is_forum,
        members_count,
        admins_count,
        default_banned_rights,
        slowmode_seconds,
        linked_discussion_peer,
        bot_commands_count,
        bot_reads_all_messages,
        has_mini_app,
        common_groups,
    ) = match peer {
        Peer::User(u) => {
            let usernames = u
                .usernames
                .usernames()
                .iter()
                .map(|name| PeerUsername {
                    username: name.clone(),
                    is_editable: u.usernames.is_editable(name),
                    is_active: true,
                })
                .collect();
            let bday = u.birthday.map(|b| InfoBirthday {
                day: b.day,
                month: b.month,
                year: (b.year != 0).then_some(b.year),
            });
            let (bot_cmds, bot_reads, mini_app) = match &u.bot_info {
                Some(b) => (b.commands.len(), b.reads_all_history, b.has_main_app),
                None => (0, false, false),
            };
            (
                u.is_verified(),
                u.is_premium(),
                (!u.phone.is_empty()).then(|| u.phone.clone()),
                usernames,
                bday,
                None,
                u.is_contact(),
                u.is_self() || u.bot_info.as_ref().is_some_and(|b| b.can_edit_information),
                false,
                false,
                false,
                !u.usernames.username().is_empty(),
                !u.allows_forwarding(),
                false,
                0,
                0,
                tg_data::ChatBannedRights::empty(),
                0,
                None,
                bot_cmds,
                bot_reads,
                mini_app,
                u.common_chats_count.max(0) as u32,
            )
        }
        Peer::Chat(c) => (
            false,
            false,
            None,
            Vec::new(),
            None,
            (!c.invite_link.is_empty()).then(|| c.invite_link.clone()),
            false,
            c.am_creator()
                || c.admin_rights
                    .contains(tg_data::ChatAdminRights::CHANGE_INFO),
            c.anyone_can_add_members() || c.am_creator(),
            c.can_add_admins(),
            c.can_edit_permissions(),
            false,
            !c.allows_forwarding(),
            false,
            c.count.max(0) as u32,
            basic_group_admins_count(c),
            c.default_restrictions,
            0,
            None,
            0,
            false,
            false,
            0,
        ),
        Peer::Channel(ch) => {
            let usernames = ch
                .usernames
                .usernames()
                .iter()
                .map(|name| PeerUsername {
                    username: name.clone(),
                    is_editable: ch.usernames.is_editable(name),
                    is_active: true,
                })
                .collect();
            let linked = ch.discussion_link.map(|cid| {
                let dpid = DataPeerId::from_channel(cid);
                (from_data_peer(dpid), peer_display_title(session, dpid))
            });
            (
                ch.is_verified(),
                false,
                None,
                usernames,
                None,
                (!ch.invite_link.is_empty()).then(|| ch.invite_link.clone()),
                false,
                ch.am_creator()
                    || ch
                        .admin_rights
                        .contains(tg_data::ChatAdminRights::CHANGE_INFO),
                ch.am_creator()
                    || ch
                        .admin_rights
                        .contains(tg_data::ChatAdminRights::INVITE_USERS),
                ch.am_creator()
                    || ch
                        .admin_rights
                        .contains(tg_data::ChatAdminRights::ADD_ADMINS),
                ch.am_creator()
                    || ch
                        .admin_rights
                        .contains(tg_data::ChatAdminRights::BAN_USERS),
                ch.is_public(),
                !ch.allows_forwarding(),
                ch.is_forum(),
                ch.members_count.max(0) as u32,
                ch.admins_count.max(0) as u32,
                ch.default_restrictions,
                ch.slowmode_seconds().max(0) as u32,
                linked,
                0,
                false,
                false,
                0,
            )
        }
    };

    let media_counts = compute_shared_media_counts(session, peer_id, common_groups);
    let group_call = match peer {
        Peer::User(_) => tg_app_core::info::backend::ProfileGroupCallState::default(),
        Peer::Chat(_) | Peer::Channel(_) => tg_app_core::info::backend::ProfileGroupCallState {
            active: peer.has_active_group_call(),
            has_participants: peer.has_non_empty_group_call(),
            call_id: peer.group_call().map(|(id, hash)| (id.0 as i64, hash)),
            can_manage: peer.can_manage_group_call(),
        },
    };

    Some(InfoPeerProfile {
        id: from_data_peer(peer_id),
        kind,
        title,
        status,
        is_online,
        verified,
        premium,
        emoji_status_id: (kind != InfoPeerKind::SavedMessages)
            .then(|| crate::peer_emoji_status::profile_emoji_status_id(peer, now))
            .flatten(),
        color_index: peer.common().color_index % 7,
        phone,
        usernames,
        bio: peer.common().about.clone(),
        birthday,
        invite_link,
        muted,
        is_contact,
        is_blocked: peer.common().block_status == BlockStatus::Blocked,
        can_edit,
        can_add_members,
        can_manage_admins,
        can_manage_permissions,
        is_public,
        no_forwards,
        is_forum,
        members_count,
        online_count: 0,
        admins_count,
        default_banned_rights,
        slowmode_seconds,
        auto_delete_ttl_seconds: peer.common().messages_ttl.max(0) as u32,
        linked_discussion_peer,
        bot_commands_count,
        bot_reads_all_messages,
        has_mini_app,
        media_counts,
        group_call,
    })
}

/// `Info::Profile::AdminsCountValue` for a basic group: admins plus the creator, `0` until the
/// participant list is loaded.
fn basic_group_admins_count(chat: &tg_data::Chat) -> u32 {
    if chat.participants.is_empty() {
        0
    } else {
        let creator = u32::from(chat.creator.0 != 0);
        (chat.admins.len() as u32).saturating_add(creator)
    }
}

fn compute_shared_media_counts(
    session: &Session,
    peer_id: DataPeerId,
    common_groups: u32,
) -> SharedMediaCounts {
    let mut counts = SharedMediaCounts {
        common_groups,
        ..Default::default()
    };
    let Some(history) = session.history(peer_id) else {
        return counts;
    };
    for slice in history.loaded_ranges.slices() {
        for &mid in &slice.messages {
            let Some(msg) = session.message(tg_data::FullMsgId::new(peer_id, mid)) else {
                continue;
            };
            if let MessageContent::Regular { media, .. } = &msg.content {
                match media {
                    MessageMedia::Photo(_) => counts.photos_videos += 1,
                    MessageMedia::WebPage(_) => counts.links += 1,
                    MessageMedia::Document(doc) => match doc.kind {
                        DocumentKind::Video => counts.photos_videos += 1,
                        DocumentKind::Voice | DocumentKind::RoundVideo => counts.voice += 1,
                        DocumentKind::Audio => counts.music += 1,
                        DocumentKind::Gif => counts.gifs += 1,
                        DocumentKind::File => counts.files += 1,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
    counts
}

/// Converts a `tg_data::Message` into an `info::backend::MediaItem` if it matches `tab`.
#[must_use]
pub fn message_to_info_media_item(
    session: &Session,
    msg: &Message,
    tab: SharedMediaTab,
) -> Option<InfoMediaItem> {
    let MessageContent::Regular { text, media } = &msg.content else {
        return None;
    };
    let id = from_data_msg(msg.id).0;
    let date_group = format_month_year(msg.date);
    let date_label = format_date_time(msg.date);
    match (tab, media) {
        (SharedMediaTab::PhotosVideos, MessageMedia::Photo(p)) => {
            let best = p
                .sizes
                .iter()
                .max_by_key(|s| s.width.saturating_mul(s.height));
            Some(InfoMediaItem::Photo {
                id,
                date_group,
                width: best.map_or(800, |s| s.width.max(1) as u32),
                height: best.map_or(600, |s| s.height.max(1) as u32),
                caption: text.text.clone(),
                preview_rgb: avatar_rgb_for_color_index((p.id.0 % 7) as u8),
            })
        }
        (SharedMediaTab::PhotosVideos, MessageMedia::Document(d))
            if d.kind == DocumentKind::Video =>
        {
            Some(InfoMediaItem::Video {
                id,
                date_group,
                duration_secs: d.duration.unwrap_or(0.0).max(0.0).round() as u32,
                width: d.width.unwrap_or(640).max(1) as u32,
                height: d.height.unwrap_or(360).max(1) as u32,
                caption: text.text.clone(),
                preview_rgb: avatar_rgb_for_color_index((d.id.0 % 7) as u8),
            })
        }
        (SharedMediaTab::Files, MessageMedia::Document(d)) if d.kind == DocumentKind::File => {
            let filename = if d.file_name.is_empty() {
                format!("file_{}", d.id.0)
            } else {
                d.file_name.clone()
            };
            let ext = filename
                .rsplit('.')
                .next()
                .filter(|s| s.len() <= 5 && *s != filename)
                .unwrap_or("FILE")
                .to_ascii_uppercase();
            Some(InfoMediaItem::File {
                id,
                date_group,
                filename,
                size_bytes: d.size.max(0) as u64,
                ext,
                date_label,
            })
        }
        (SharedMediaTab::Links, MessageMedia::WebPage(wp)) => Some(InfoMediaItem::Link {
            id,
            date_group,
            url: wp.url.clone(),
            title: wp.title.clone().unwrap_or_else(|| wp.url.clone()),
            description: wp.description.clone().unwrap_or_default(),
            domain: wp.display_url.clone(),
        }),
        (SharedMediaTab::Voice, MessageMedia::Document(d))
            if matches!(d.kind, DocumentKind::Voice | DocumentKind::RoundVideo) =>
        {
            let sender = msg
                .from_id
                .map(|pid| peer_display_title(session, pid))
                .unwrap_or_else(|| "Voice".to_owned());
            let waveform = d
                .waveform
                .as_deref()
                .map(tg_media::audio::waveform_decode_5bit)
                .unwrap_or_default();
            Some(InfoMediaItem::Voice {
                id,
                date_group,
                sender,
                duration_secs: d.duration.unwrap_or(0.0).max(0.0).round() as u32,
                waveform,
                listened: !msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD),
                date_label,
            })
        }
        (SharedMediaTab::Music, MessageMedia::Document(d)) if d.kind == DocumentKind::Audio => {
            Some(InfoMediaItem::Music {
                id,
                date_group,
                title: d.audio_title.clone().unwrap_or_else(|| d.file_name.clone()),
                performer: d.audio_performer.clone().unwrap_or_default(),
                duration_secs: d.duration.unwrap_or(0.0).max(0.0).round() as u32,
                size_bytes: d.size.max(0) as u64,
            })
        }
        (SharedMediaTab::Gifs, MessageMedia::Document(d)) if d.kind == DocumentKind::Gif => {
            Some(InfoMediaItem::Gif {
                id,
                date_group,
                label: if d.file_name.is_empty() {
                    "GIF".to_owned()
                } else {
                    d.file_name.clone()
                },
                duration_secs: d.duration.unwrap_or(0.0).max(0.0).round() as u32,
                preview_rgb: avatar_rgb_for_color_index((d.id.0 % 7) as u8),
            })
        }
        _ => None,
    }
}

/// `OverlayWidget::refreshFromLabel`: the original sender's own name (forward origin first),
/// never the "Saved Messages" label.
fn viewer_sender_name(session: &Session, msg: &Message) -> String {
    if let Some(fwd) = &msg.forward_from {
        if let Some(pid) = fwd.from_id {
            return crate::preview_text::peer_real_name(session, pid);
        }
        if let Some(name) = fwd.from_name.as_ref().filter(|n| !n.is_empty()) {
            return name.clone();
        }
    }
    let pid = msg.from_id.unwrap_or(msg.peer_id);
    crate::preview_text::peer_real_name(session, pid)
}

/// Converts a `tg_data::Message` with photo/document media into a viewer [`ViewerMediaItem`],
/// formatting `date_text` relative to the current wall clock (`Ui::FormatDateTime`).
#[must_use]
pub fn message_to_viewer_media_item(session: &Session, msg: &Message) -> Option<ViewerMediaItem> {
    message_to_viewer_media_item_at(session, msg, now_unix_i32())
}

/// Converts a `tg_data::Message` with photo/document media into a viewer [`ViewerMediaItem`],
/// formatting `date_text` relative to `now_ts` in local time (`Ui::FormatDateTime`).
#[must_use]
pub fn message_to_viewer_media_item_at(
    session: &Session,
    msg: &Message,
    now_ts: i32,
) -> Option<ViewerMediaItem> {
    let MessageContent::Regular { text, media } = &msg.content else {
        return None;
    };
    let sender_name = viewer_sender_name(session, msg);
    let date_text = format_date_time_at(msg.date, now_ts);
    let caption = data_text_to_ui_text(text);
    let allows_save = !msg.flags.contains(MessageFlags::NO_FORWARDS);
    let grouped_id = msg.grouped_id.filter(|&id| id != 0).map(|id| id as i64);

    match media {
        MessageMedia::Photo(p) => {
            let best = p
                .sizes
                .iter()
                .max_by_key(|s| s.width.saturating_mul(s.height));
            Some(ViewerMediaItem {
                id: MediaItemId(p.id.0),
                peer_id: msg.peer_id.0,
                msg_id: from_data_msg(msg.id).0,
                kind: MediaItemKind::Photo,
                width: best.map_or(800, |s| s.width.max(1) as u32),
                height: best.map_or(600, |s| s.height.max(1) as u32),
                duration_ms: 0,
                file_size: best.map_or(0, |s| s.size.max(0) as u64),
                file_name: format!("photo_{}.jpg", p.id.0),
                mime_type: "image/jpeg".to_owned(),
                sender_name,
                date_text,
                caption,
                rotation_degrees: 0,
                has_alpha: false,
                allows_forward: allows_save,
                allows_delete: msg.is_outgoing(),
                allows_save,
                local_path: None,
                attached_stickers_media: p.has_stickers.then(|| {
                    tg_app_core::chat_helpers::backend::AttachedStickersMedia::Photo {
                        id: p.id.0 as i64,
                        access_hash: p.access_hash as i64,
                        file_reference: p.file_reference.clone(),
                    }
                }),
                grouped_id,
            })
        }
        MessageMedia::Document(d) => {
            let file_name = if d.file_name.is_empty() {
                format!("document_{}", d.id.0)
            } else {
                d.file_name.clone()
            };
            let kind = match d.kind {
                DocumentKind::Video => MediaItemKind::Video,
                DocumentKind::Gif => MediaItemKind::Gif,
                DocumentKind::RoundVideo => MediaItemKind::RoundVideo,
                _ if tg_app_core::core_types::misc::is_theme_file(&file_name, &d.mime_type) => {
                    MediaItemKind::Theme
                }
                _ => MediaItemKind::Document,
            };
            Some(ViewerMediaItem {
                id: MediaItemId(d.id.0),
                peer_id: msg.peer_id.0,
                msg_id: from_data_msg(msg.id).0,
                kind,
                width: d.width.unwrap_or(640).max(1) as u32,
                height: d.height.unwrap_or(360).max(1) as u32,
                duration_ms: (d.duration.unwrap_or(0.0).max(0.0) * 1000.0).round() as i64,
                file_size: d.size.max(0) as u64,
                file_name,
                mime_type: d.mime_type.clone(),
                sender_name,
                date_text,
                caption,
                rotation_degrees: 0,
                has_alpha: d.mime_type.eq_ignore_ascii_case("image/png")
                    || d.mime_type.eq_ignore_ascii_case("image/webp"),
                allows_forward: allows_save,
                allows_delete: msg.is_outgoing(),
                allows_save,
                local_path: None,
                attached_stickers_media: None,
                grouped_id,
            })
        }
        MessageMedia::VideoStream(vs) => Some(ViewerMediaItem {
            id: MediaItemId(vs.call_id.0),
            peer_id: msg.peer_id.0,
            msg_id: from_data_msg(msg.id).0,
            kind: MediaItemKind::VideoStream,
            width: 1280,
            height: 720,
            duration_ms: 0,
            file_size: 0,
            file_name: if vs.rtmp_stream {
                format!("rtmp_stream_{}", vs.call_id.0)
            } else {
                format!("live_stream_{}", vs.call_id.0)
            },
            mime_type: "application/x-tg-video-stream".to_owned(),
            sender_name,
            date_text,
            caption,
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: allows_save,
            allows_delete: msg.is_outgoing(),
            allows_save: false,
            local_path: None,
            attached_stickers_media: None,
            grouped_id,
        }),
        _ => None,
    }
}

/// Converts a `tg_data::Message` containing audio/voice into [`AudioTrackMetadata`].
#[must_use]
pub fn message_to_audio_track_metadata(
    session: &Session,
    msg: &Message,
) -> Option<AudioTrackMetadata> {
    let MessageContent::Regular {
        media: MessageMedia::Document(doc),
        ..
    } = &msg.content
    else {
        return None;
    };
    if !matches!(doc.kind, DocumentKind::Voice | DocumentKind::Audio) {
        return None;
    }
    let sender = msg
        .from_id
        .map(|pid| peer_display_title(session, pid))
        .unwrap_or_else(|| peer_display_title(session, msg.peer_id));
    let (title, subtitle) = if doc.kind == DocumentKind::Audio {
        (
            doc.audio_title
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| doc.file_name.clone()),
            doc.audio_performer.clone().unwrap_or(sender),
        )
    } else {
        (sender, format_date_time(msg.date))
    };
    let media_kind = match doc.kind {
        DocumentKind::Voice => tg_audio::MediaKind::VoiceMessage,
        DocumentKind::RoundVideo => tg_audio::MediaKind::VideoMessage,
        DocumentKind::Audio => tg_audio::MediaKind::AudioFile,
        _ => tg_audio::MediaKind::Other,
    };
    let duration_ms = (doc.duration.unwrap_or(0.0).max(0.0) * 1000.0).round() as i64;
    Some(AudioTrackMetadata {
        id: tg_audio::AudioMsgId::new(doc.id, msg.full_id(), media_kind, duration_ms, 0),
        title,
        subtitle,
        duration_ms,
    })
}

/// Converts a `tg_data::Peer` into a `settings::backend::PeerRef`.
#[must_use]
pub fn peer_to_settings_ref(peer: &Peer, now: i32) -> PeerRef {
    let (subtitle, _) = peer_status_text(peer, now);
    PeerRef {
        id: peer.id().0,
        name: if peer.name().is_empty() {
            format!("Peer {}", peer.id().bare())
        } else {
            peer.name().to_owned()
        },
        username: peer_primary_username(peer).unwrap_or_default(),
        subtitle,
        is_group_or_channel: !matches!(peer, Peer::User(_)),
    }
}

/// Builds a `settings::backend::ProfileInfo` for the signed-in self user.
#[must_use]
pub fn self_profile_from_session(session: &Session) -> ProfileInfo {
    let self_id = session.self_user_id();
    if let Some(user) = session.user(self_id) {
        ProfileInfo {
            user_id: self_id.0,
            first_name: user.first_name.clone(),
            last_name: user.last_name.clone(),
            username: user.usernames.username().to_owned(),
            phone: user.phone.clone(),
            bio: user.common.about.clone(),
            birthday: user.birthday.map(|b| SettingsBirthday {
                day: b.day,
                month: b.month,
                year: (b.year != 0).then_some(b.year),
            }),
            photo_color_index: user.common.color_index % 7,
            photo_label: None,
            phone_hidden: false,
        }
    } else {
        ProfileInfo {
            user_id: self_id.0,
            first_name: format!("User {}", self_id.0),
            last_name: String::new(),
            username: String::new(),
            phone: String::new(),
            bio: String::new(),
            birthday: None,
            photo_color_index: tg_data::decide_color_index(session.self_peer_id()) % 7,
            photo_label: None,
            phone_hidden: false,
        }
    }
}

/// Converts a `tg_data::Peer` into a `stories::backend::PeerProfile`.
#[must_use]
pub fn peer_to_story_profile(session: &Session, peer_id: DataPeerId) -> StoryPeerProfile {
    let Some(peer) = session.peer(peer_id) else {
        return StoryPeerProfile {
            peer_id,
            ..Default::default()
        };
    };
    let name = peer_display_title(session, peer_id);
    let short_name = match peer {
        Peer::User(u) => u.short_name().to_owned(),
        _ => name.clone(),
    };
    let rgb = avatar_rgb_for_color_index(peer.common().color_index);
    StoryPeerProfile {
        peer_id,
        initials: initials_from_name(&name),
        name,
        short_name,
        username: peer_primary_username(peer).unwrap_or_default(),
        avatar_top_rgb: rgb,
        avatar_bottom_rgb: [
            rgb[0].saturating_sub(24),
            rgb[1].saturating_sub(24),
            rgb[2].saturating_sub(24),
        ],
        is_self: peer_id == session.self_peer_id(),
        is_verified: match peer {
            Peer::User(u) => u.is_verified(),
            Peer::Channel(ch) => ch.is_verified(),
            Peer::Chat(_) => false,
        },
        is_premium: peer.as_user().is_some_and(tg_data::User::is_premium),
        is_broadcast: peer
            .as_channel()
            .is_some_and(tg_data::Channel::is_broadcast),
        is_megagroup: peer
            .as_channel()
            .is_some_and(tg_data::Channel::is_megagroup),
        can_view_channel_reactions: false,
        requires_premium_to_write: peer
            .as_user()
            .is_some_and(tg_data::User::requires_premium_to_write),
        stars_per_message: peer.as_user().map_or(0, |u| u.stars_per_message),
    }
}

/// Converts a `tg_data::Peer` into a `calls::backend::CallPeerInfo`.
#[must_use]
pub fn peer_to_call_info(session: &Session, peer_id: DataPeerId) -> CallPeerInfo {
    let name = peer_display_title(session, peer_id);
    let peer = session.peer(peer_id);
    let rgb = peer.map_or_else(
        || avatar_rgb_for_color_index(tg_data::decide_color_index(peer_id)),
        |p| avatar_rgb_for_color_index(p.common().color_index),
    );
    let short_name = peer
        .and_then(Peer::as_user)
        .map(|u| u.short_name().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name.clone());
    let (about, is_channel, is_broadcast, members_count) = match peer {
        Some(Peer::Channel(ch)) => (
            ch.common.about.clone(),
            true,
            ch.is_broadcast(),
            ch.members_count.max(0) as u32,
        ),
        Some(Peer::Chat(c)) => (c.common.about.clone(), false, false, c.count.max(0) as u32),
        Some(Peer::User(u)) => (u.common.about.clone(), false, false, 0),
        None => (String::new(), false, false, 0),
    };
    CallPeerInfo {
        id: from_data_peer(peer_id),
        initials: initials_from_name(&name),
        name,
        short_name,
        avatar_color: Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
        about,
        is_channel,
        is_broadcast,
        members_count,
    }
}

/// Computes the default folder icon when `filter.icon_emoji` is empty or unrecognized
/// (`Ui::ComputeDefaultFilterIcon` in `Telegram/SourceFiles/ui/filter_icons.cpp:303-334`).
///
/// Any folder with explicit `always` / `never` peer lists (such as a `"Work"` folder pinned
/// to specific chats) or with no category inclusion flags returns [`FolderIcon::Custom`].
#[must_use]
fn default_filter_icon(filter: &ChatFilter) -> FolderIcon {
    let all = ChatFilterFlags::CONTACTS
        | ChatFilterFlags::NON_CONTACTS
        | ChatFilterFlags::GROUPS
        | ChatFilterFlags::CHANNELS
        | ChatFilterFlags::BOTS;
    let removed = ChatFilterFlags::NO_READ | ChatFilterFlags::NO_MUTED;
    let people = ChatFilterFlags::CONTACTS | ChatFilterFlags::NON_CONTACTS;
    let included = filter.flags & all;
    let excluded = filter.flags & removed;
    if !filter.always.is_empty() || !filter.never.is_empty() || included.is_empty() {
        FolderIcon::Custom
    } else if included == ChatFilterFlags::CONTACTS
        || included == ChatFilterFlags::NON_CONTACTS
        || included == people
    {
        FolderIcon::Private
    } else if included == ChatFilterFlags::GROUPS {
        FolderIcon::Groups
    } else if included == ChatFilterFlags::CHANNELS {
        FolderIcon::Channels
    } else if included == ChatFilterFlags::BOTS {
        FolderIcon::Bots
    } else if excluded == ChatFilterFlags::NO_READ {
        FolderIcon::Unread
    } else if excluded == ChatFilterFlags::NO_MUTED {
        FolderIcon::Unmuted
    } else {
        FolderIcon::Custom
    }
}

/// Converts a `tg_data::ChatFilter` into a `dialogs::backend::ChatFolder`.
#[must_use]
pub fn chat_filter_to_dialog_folder(filter: &ChatFilter, session: &Session) -> DialogChatFolder {
    let icon = FolderIconKind::from(
        FolderIcon::from_emoticon(&filter.icon_emoji)
            .unwrap_or_else(|| default_filter_icon(filter)),
    );

    let unread_state = session
        .filters()
        .chats_list(filter.id)
        .map(tg_data::MainList::unread_state)
        .unwrap_or_default();

    DialogChatFolder {
        id: FilterId(filter.id),
        title: filter.title.text.clone(),
        icon,
        color_index: filter.color_index.unwrap_or(5) % 7,
        unread: folder_unread_from_state(&unread_state),
        locked: false,
    }
}

/// A folder tab's unread chats: chats with unread messages plus chats marked unread, and how
/// many of them are muted (`ui/widgets/chat_filters_tabs_strip.cpp:244-249`,
/// `window/window_filters_menu.cpp:633-638`). Folder tabs count chats whatever the "count
/// unread messages" setting says.
#[must_use]
pub fn folder_unread_from_state(state: &tg_data::UnreadState) -> FolderUnreadState {
    FolderUnreadState {
        unread_chats: state.chats.saturating_add(state.marks).max(0) as u32,
        muted_chats: state.chats_muted.saturating_add(state.marks_muted).max(0) as u32,
    }
}

/// The "All chats" tab's unread chats: the main list without the archive
/// (`Data::UnreadStateValue` for filter 0 -> `MainListMapUnreadState`,
/// `data/data_unread_value.cpp:32-53`). Archived chats never enter Rustgram's main list
/// unread state, so nothing is subtracted.
#[must_use]
pub fn all_chats_folder_unread(session: &Session) -> FolderUnreadState {
    folder_unread_from_state(&session.main_list().unread_state())
}

/// The unread state behind the app's tray and taskbar badge: the main list with the archive
/// counted as muted chats (`Data::Session::unreadBadge` uses `_chatsList.unreadState()`, which
/// holds the archive folder entry, `data/data_session.cpp:3668-3687`;
/// `Folder::chatListUnreadState`, `data/data_folder.cpp:386-388`). `mentions` counts chats with
/// unread mentions, like `Dialogs::UnreadState::mentions`.
#[must_use]
pub fn app_unread_badge_state(session: &Session) -> UnreadBadgeState {
    let state = session.main_list().unread_state() + session.archive().unread_state();
    let count = |n: i32| n.max(0) as u32;
    UnreadBadgeState {
        messages: count(state.messages),
        messages_muted: count(state.messages_muted),
        chats: count(state.chats),
        chats_muted: count(state.chats_muted),
        marks: count(state.marks),
        marks_muted: count(state.marks_muted),
        mentions: count(state.mentions),
    }
}

/// The "All chats" tab of the folder strip and sidebar, with [`all_chats_folder_unread`].
#[must_use]
pub fn all_chats_dialog_folder(session: &Session) -> DialogChatFolder {
    DialogChatFolder {
        id: FilterId::ALL,
        title: "All Chats".to_owned(),
        icon: FolderIconKind::All,
        color_index: 5,
        unread: all_chats_folder_unread(session),
        locked: false,
    }
}

/// Converts a `tg_data::ChatFilter` into a `settings::backend::ChatFolder`.
#[must_use]
pub fn chat_filter_to_settings_folder(filter: &ChatFilter) -> SettingsChatFolder {
    let icon = FolderIcon::from_emoticon(&filter.icon_emoji)
        .unwrap_or_else(|| default_filter_icon(filter));
    SettingsChatFolder {
        id: filter.id,
        title: filter.title.text.clone(),
        icon,
        color_index: filter.color_index,
        include_contacts: filter.flags.contains(ChatFilterFlags::CONTACTS),
        include_non_contacts: filter.flags.contains(ChatFilterFlags::NON_CONTACTS),
        include_groups: filter.flags.contains(ChatFilterFlags::GROUPS),
        include_channels: filter.flags.contains(ChatFilterFlags::CHANNELS),
        include_bots: filter.flags.contains(ChatFilterFlags::BOTS),
        exclude_muted: filter.flags.contains(ChatFilterFlags::NO_MUTED),
        exclude_read: filter.flags.contains(ChatFilterFlags::NO_READ),
        exclude_archived: filter.flags.contains(ChatFilterFlags::NO_ARCHIVED),
        included_peers: filter.always.iter().map(|p| p.0).collect(),
        excluded_peers: filter.never.iter().map(|p| p.0).collect(),
        shareable: filter.is_chatlist(),
    }
}

/// Maps `settings::backend::PrivacyKey` to its Layer 233 `tg_tl::enums::InputPrivacyKey`.
#[must_use]
pub fn privacy_key_to_tl(key: PrivacyKey) -> tl_enums::InputPrivacyKey {
    match key {
        PrivacyKey::PhoneNumber => tl_enums::InputPrivacyKey::PhoneNumber,
        PrivacyKey::AddedByPhone => tl_enums::InputPrivacyKey::AddedByPhone,
        PrivacyKey::LastSeen => tl_enums::InputPrivacyKey::StatusTimestamp,
        PrivacyKey::ProfilePhoto => tl_enums::InputPrivacyKey::ProfilePhoto,
        PrivacyKey::Forwards => tl_enums::InputPrivacyKey::Forwards,
        PrivacyKey::Calls => tl_enums::InputPrivacyKey::PhoneCall,
        PrivacyKey::CallsPeer2Peer => tl_enums::InputPrivacyKey::PhoneP2P,
        PrivacyKey::GroupsInvite => tl_enums::InputPrivacyKey::ChatInvite,
        PrivacyKey::VoiceMessages => tl_enums::InputPrivacyKey::VoiceMessages,
        PrivacyKey::About => tl_enums::InputPrivacyKey::About,
        PrivacyKey::Birthday => tl_enums::InputPrivacyKey::Birthday,
        PrivacyKey::GiftsAutoSave => tl_enums::InputPrivacyKey::StarGiftsAutoSave,
        PrivacyKey::SavedMusic => tl_enums::InputPrivacyKey::SavedMusic,
    }
}

/// Converts a slice of TL `PrivacyRule` values into a `settings::backend::PrivacyRule`.
#[must_use]
pub fn privacy_rules_from_tl(rules: &[tl_enums::PrivacyRule]) -> PrivacyRule {
    let mut out = PrivacyRule {
        option: PrivacyOption::Nobody,
        ..Default::default()
    };
    for rule in rules {
        match rule {
            tl_enums::PrivacyRule::PrivacyValueAllowAll => out.option = PrivacyOption::Everybody,
            tl_enums::PrivacyRule::PrivacyValueAllowContacts => {
                if out.option != PrivacyOption::Everybody {
                    out.option = PrivacyOption::Contacts;
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowAll => {
                if out.option == PrivacyOption::Everybody {
                    // Explicit allow-all came earlier; keep allow-all.
                } else if out.option != PrivacyOption::Contacts {
                    out.option = PrivacyOption::Nobody;
                }
            }
            tl_enums::PrivacyRule::PrivacyValueAllowUsers(u) => {
                for &uid in &u.users {
                    out.always_allow
                        .push(DataPeerId::from_user(DataUserId::from_tl(uid)).0);
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowUsers(u) => {
                for &uid in &u.users {
                    out.never_allow
                        .push(DataPeerId::from_user(DataUserId::from_tl(uid)).0);
                }
            }
            tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(c) => {
                for &cid in &c.chats {
                    out.always_allow
                        .push(DataPeerId::from_chat(tg_data::ChatId::from_tl(cid)).0);
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowChatParticipants(c) => {
                for &cid in &c.chats {
                    out.never_allow
                        .push(DataPeerId::from_chat(tg_data::ChatId::from_tl(cid)).0);
                }
            }
            tl_enums::PrivacyRule::PrivacyValueAllowPremium => out.always_premiums = true,
            tl_enums::PrivacyRule::PrivacyValueAllowBots => out.always_miniapps = true,
            tl_enums::PrivacyRule::PrivacyValueDisallowBots => out.never_miniapps = true,
            tl_enums::PrivacyRule::PrivacyValueDisallowContacts
            | tl_enums::PrivacyRule::PrivacyValueAllowCloseFriends => {}
        }
    }
    out.normalize();
    out
}

/// Converts a `settings::backend::PrivacyRule` into outgoing TL `InputPrivacyRule`s.
#[must_use]
pub fn privacy_rule_to_tl(
    rule: &PrivacyRule,
    session: &Session,
) -> Vec<tl_enums::InputPrivacyRule> {
    let mut out = Vec::new();
    let allow_users: Vec<tl_enums::InputUser> = rule
        .always_allow
        .iter()
        .filter_map(|&raw| DataPeerId(raw).to_user())
        .map(|uid| tg_api::convert::input_user_for(session, uid))
        .collect();
    if !allow_users.is_empty() {
        out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(
            tl_types::InputPrivacyValueAllowUsers { users: allow_users },
        ));
    }
    let disallow_users: Vec<tl_enums::InputUser> = rule
        .never_allow
        .iter()
        .filter_map(|&raw| DataPeerId(raw).to_user())
        .map(|uid| tg_api::convert::input_user_for(session, uid))
        .collect();
    if !disallow_users.is_empty() {
        out.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(
            tl_types::InputPrivacyValueDisallowUsers {
                users: disallow_users,
            },
        ));
    }
    if rule.always_premiums {
        out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowPremium);
    }
    if rule.always_miniapps {
        out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowBots);
    }
    if rule.never_miniapps {
        out.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowBots);
    }
    match rule.option {
        PrivacyOption::Everybody => out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll),
        PrivacyOption::Contacts => {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts);
        }
        PrivacyOption::Nobody => out.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowAll),
    }
    out
}

/// Converts a TL `Authorization` into `settings::backend::ActiveSession`.
///
/// Uses `tg_api::account::AuthorizationEntry::from_tl` to match tdesktop's `ParseEntry`
/// (`api/api_authorizations.cpp:32-89`), including the `date_active == 0` -> `date_created`
/// fallback.
#[must_use]
pub fn authorization_from_tl(auth: &tl_types::Authorization) -> ActiveSession {
    let entry = tg_api::account::AuthorizationEntry::from_tl(auth);
    let location = match (entry.region.is_empty(), entry.country.is_empty()) {
        (false, false) => format!("{}, {}", entry.region, entry.country),
        (true, false) => entry.country.clone(),
        (false, true) => entry.region.clone(),
        (true, true) => entry.ip.clone(),
    };
    let active_time = if entry.hash == 0 {
        "online".to_owned()
    } else {
        format_date_time(entry.active_time)
    };
    ActiveSession {
        hash: entry.hash as u64,
        api_id: entry.api_id,
        device_model: entry.device_model,
        platform: entry.platform,
        system_version: entry.system_version,
        app_name_version: format!("{} {}", entry.app_name, entry.app_version)
            .trim()
            .to_owned(),
        location,
        ip: entry.ip,
        active_time,
        accept_calls: !entry.calls_disabled,
        accept_secret_chats: !entry.encrypted_disabled,
    }
}

/// Maps `settings::backend::NotifyScope` to TL `InputNotifyPeer`.
#[must_use]
pub fn notify_scope_to_tl(scope: NotifyScope) -> tl_enums::InputNotifyPeer {
    match scope {
        NotifyScope::PrivateChats => tl_enums::InputNotifyPeer::InputNotifyUsers,
        NotifyScope::Groups => tl_enums::InputNotifyPeer::InputNotifyChats,
        NotifyScope::Channels => tl_enums::InputNotifyPeer::InputNotifyBroadcasts,
    }
}

/// Parses a UI / `tg-data` notification sound name into `tg_api::account::NotifySound`
/// (`SerializeSound`, `data/notify/data_peer_notify_settings.cpp:42-55`).
///
/// Accepts `"Default"`, `"None"`, `"Ringtone <id>"`, `"ringtone:<id>"`, and local sound titles.
#[must_use]
pub fn sound_name_to_api_sound(name: &str) -> tg_api::account::NotifySound {
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("none") {
        tg_api::account::NotifySound::None
    } else if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        tg_api::account::NotifySound::Default
    } else if let Some(rest) = trimmed
        .strip_prefix("Ringtone ")
        .or_else(|| trimmed.strip_prefix("ringtone:"))
        .or_else(|| trimmed.strip_prefix("ringtone "))
        && let Ok(id) = rest.trim().parse::<i64>()
    {
        tg_api::account::NotifySound::Ringtone(id)
    } else {
        tg_api::account::NotifySound::Local {
            title: trimmed.to_owned(),
            data: trimmed.to_owned(),
        }
    }
}

/// Converts TL `PeerNotifySettings` into `settings::backend::ScopeNotifySettings`.
///
/// Matches `NotifyPeerSettingsValue::change` (`data/notify/data_peer_notify_settings.cpp:108-124`)
/// and `NotifySettings::isMuted` (`data/notify/data_notify_settings.cpp:547-561`):
/// - muted is determined solely by `mute_until > now` (never `silent == Some(true)`, which is
///   the silent-broadcast-posts setting);
/// - reads only `other_sound` and never falls back to `ios_sound` or `android_sound`.
#[must_use]
pub fn notify_settings_from_tl(settings: &tl_types::PeerNotifySettings) -> ScopeNotifySettings {
    let parsed = tg_api::account::FullNotifySettings::from_tl(
        &tl_enums::PeerNotifySettings::PeerNotifySettings(settings.clone()),
    );
    let now = now_unix_i32();
    let muted = parsed.mute_until.is_some_and(|u| u > now);
    let sound = match parsed.sound {
        None | Some(tg_api::account::NotifySound::Default) => "Default".to_owned(),
        Some(tg_api::account::NotifySound::None) => "None".to_owned(),
        Some(tg_api::account::NotifySound::Local { title, data }) => {
            if title.is_empty() {
                data
            } else {
                title
            }
        }
        Some(tg_api::account::NotifySound::Ringtone(id)) => format!("Ringtone {id}"),
    };
    ScopeNotifySettings {
        enabled: !muted,
        show_preview: parsed.show_previews.unwrap_or(true),
        sound,
        exceptions: Vec::new(),
    }
}

/// Converts a `settings::backend::ScopeNotifySettings` into TL `InputPeerNotifySettings`.
///
/// Matches `NotifyPeerSettingsValue::serialize` (`data/notify/data_peer_notify_settings.cpp:184-202`):
/// preserves `Ringtone` and `Local` sounds via `NotifySound::to_tl` and leaves `silent` unset
/// (silent-posts is distinct from mute).
#[must_use]
pub fn notify_settings_to_tl(settings: &ScopeNotifySettings) -> tl_enums::InputPeerNotifySettings {
    let full = tg_api::account::FullNotifySettings {
        show_previews: Some(settings.show_preview),
        silent: None,
        mute_until: Some(if settings.enabled { 0 } else { i32::MAX }),
        sound: Some(sound_name_to_api_sound(&settings.sound)),
        stories_muted: None,
        stories_hide_sender: None,
        stories_sound: None,
    };
    full.to_tl()
}

/// Extracts a `DraftMessage` from `History`.
#[must_use]
pub fn draft_from_history(history: &History) -> Option<DraftMessage> {
    let local_key = tg_data::DraftKey::local(DataMsgId::ZERO, DataPeerId::ZERO);
    let cloud_key = tg_data::DraftKey::cloud(DataMsgId::ZERO, DataPeerId::ZERO);
    let d = history
        .draft(local_key)
        .or_else(|| history.draft(cloud_key))?;
    if d.is_empty() {
        return None;
    }
    Some(DraftMessage {
        text: d.text.text.clone(),
        reply_to: d
            .reply
            .as_ref()
            .and_then(|r| r.reply_to_msg_id)
            .map(from_data_msg),
    })
}
