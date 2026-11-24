//! Field autocomplete, query parsing, ranking/filtering, hashtag history,
//! recent inline bots, bot command wrapping, and insertion result calculation.
//!
//! Port of:
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`
//! - `Telegram/SourceFiles/chat_helpers/message_field.cpp` (`PrepareMentionTag`,
//!   `ParseMentionHashtagBotCommandQuery`, `ParseInlineBotQuery`, `PreventInlineBotQuery`)
//! - `Telegram/SourceFiles/chat_helpers/bot_command.cpp` (`WrapCommandInChat`)
//! - `Telegram/SourceFiles/storage/localstorage.cpp` (`incrementRecentHashtag`)
//! - `Telegram/SourceFiles/data/components/recent_inline_bots.cpp`
//! - `Telegram/lib_ui/ui/widgets/fields/input_field.cpp` (`InputField::insertTag`)

use std::collections::{BTreeMap, HashMap};

use tg_data::peer::BotStatus;
use tg_data::{BotCommand, TimeId, UserId};
use tg_text::{remove_accents, utf16_len};

use crate::types::{
    BusinessShortcut, ComposeFeatures, MentionNameEntity, UserInfo, sort_by_online_value,
};

/// Maximum number of recent hashtags stored, matching `incrementRecentHashtag` (`>= 64`).
pub const MAX_RECENT_HASHTAGS: usize = 64;

/// Rating threshold above which all recent hashtag ratings decay by half,
/// matching `i->second > 0x4000` in `storage/localstorage.cpp`.
pub const HASHTAG_DECAY_THRESHOLD: u16 = 0x4000;

/// Maximum number of recent inline bots stored, matching `kLimit = 10` in `recent_inline_bots.cpp`.
pub const MAX_RECENT_INLINE_BOTS: usize = 10;

/// Maximum query length for normal `@mention`, `#hashtag`, `/command` tokens (`> 63` breaks).
pub const MAX_AUTOCOMPLETE_QUERY_CHARS: usize = 64;

/// Maximum query length when `@bot` appears inside a `/command@bot` token (`> 127` breaks).
pub const MAX_COMMAND_WITH_BOT_QUERY_CHARS: usize = 128;

/// How an autocomplete item was chosen by the user, matching `FieldAutocompleteChooseMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooseMethod {
    /// Confirmed with Enter/Return.
    ByEnter,
    /// Confirmed with Tab.
    ByTab,
    /// Confirmed by mouse click.
    ByClick,
}

/// Kind of autocomplete popup currently active, matching `FieldAutocomplete::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteKind {
    /// `@mention` list.
    Mentions,
    /// `#hashtag` list.
    Hashtags,
    /// `/command` (or business quick-reply shortcut) list.
    BotCommands,
    /// `:emoji` keyword suggestion (handled in [`crate::emoji`]).
    EmojiKeyword,
    /// Sticker suggestions for a single emoji.
    Stickers,
}

/// Result of parsing the autocomplete token under the cursor.
///
/// Matches `AutocompleteQuery` in `chat_helpers/message_field.h`, enriched with the
/// exact byte range in the text so insertion can replace the token cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteQuery {
    /// Detected autocomplete category.
    pub kind: AutocompleteKind,
    /// Full raw query including the leading trigger character (`@`, `#`, `/`, or `:`).
    pub query: String,
    /// Whether the query starts at position 0 of the entire input field.
    pub from_start: bool,
    /// Start byte offset in the input text (at the trigger character).
    pub start_byte: usize,
    /// End byte offset in the input text (cursor position).
    pub end_byte: usize,
}

/// Result of parsing an inline bot query at the start of the input field.
///
/// Matches `InlineBotQuery` in `chat_helpers/message_field.h`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineBotQuery {
    /// Username after `@` (empty when not a valid inline bot query).
    pub username: String,
    /// Query text after `@username ` (empty when `@...bot` has no trailing space yet).
    pub query: String,
    /// Resolved bot user ID if found in cache and supporting inline queries.
    pub bot_id: Option<UserId>,
    /// True when the username was not in the local cache and must be looked up remotely.
    pub looking_up_bot: bool,
}

/// Parses `@mention`, `#hashtag`, or `/command` directly before `cursor_byte` in `text`.
///
/// Direct port of `ParseMentionHashtagBotCommandQuery` in
/// `Telegram/SourceFiles/chat_helpers/message_field.cpp`.
///
/// - `fragment_start_byte` is the byte offset of the current contiguous plain-text fragment
///   (pass `0` when the entire field is plain text; pass the offset after an embedded image/emoji
///   when checking a later fragment so `/` is only recognized at the very start of the field).
/// - Returns `None` if `has_selection` is true or no valid trigger precedes the cursor.
pub fn parse_mention_hashtag_bot_command_query(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    has_selection: bool,
    features: ComposeFeatures,
) -> Option<AutocompleteQuery> {
    if has_selection || cursor_byte > text.len() || fragment_start_byte > cursor_byte {
        return None;
    }
    if !text.is_char_boundary(cursor_byte) || !text.is_char_boundary(fragment_start_byte) {
        return None;
    }

    let fragment = &text[fragment_start_byte..cursor_byte];
    let chars: Vec<(usize, char)> = fragment.char_indices().collect();
    let len = chars.len();
    if len == 0 {
        return None;
    }

    let mut mention_in_command = false;
    let mut i = len;
    while i != 0 {
        let (byte_pos, ch) = chars[i - 1];
        if ch == '@' {
            if !features.autocomplete_mentions {
                return None;
            }
            let after_ok = (len - i) < 1 || chars[i].1.is_alphanumeric();
            let before_is_word =
                i >= 2 && (chars[i - 2].1.is_alphanumeric() || chars[i - 2].1 == '_');
            if after_ok && !before_is_word {
                let start_byte = fragment_start_byte + byte_pos;
                return Some(AutocompleteQuery {
                    kind: AutocompleteKind::Mentions,
                    query: text[start_byte..cursor_byte].to_string(),
                    from_start: i == 1 && fragment_start_byte == 0,
                    start_byte,
                    end_byte: cursor_byte,
                });
            } else if after_ok && i > 2 && before_is_word && !mention_in_command {
                mention_in_command = true;
                i -= 1;
                continue;
            }
            return None;
        } else if ch == '#' {
            if !features.autocomplete_hashtags {
                return None;
            }
            let before_is_word =
                i >= 2 && (chars[i - 2].1.is_alphanumeric() || chars[i - 2].1 == '_');
            if !before_is_word {
                let start_byte = fragment_start_byte + byte_pos;
                return Some(AutocompleteQuery {
                    kind: AutocompleteKind::Hashtags,
                    query: text[start_byte..cursor_byte].to_string(),
                    from_start: i == 1 && fragment_start_byte == 0,
                    start_byte,
                    end_byte: cursor_byte,
                });
            }
            return None;
        } else if ch == '/' {
            if !features.autocomplete_commands {
                return None;
            }
            if i < 2 && fragment_start_byte == 0 {
                let start_byte = fragment_start_byte + byte_pos;
                return Some(AutocompleteQuery {
                    kind: AutocompleteKind::BotCommands,
                    query: text[start_byte..cursor_byte].to_string(),
                    from_start: i == 1 && fragment_start_byte == 0,
                    start_byte,
                    end_byte: cursor_byte,
                });
            }
            return None;
        }

        let dist = len - i;
        if dist > MAX_COMMAND_WITH_BOT_QUERY_CHARS - 1
            || (!mention_in_command && dist > MAX_AUTOCOMPLETE_QUERY_CHARS - 1)
        {
            break;
        }
        if !ch.is_alphanumeric() && ch != '_' {
            break;
        }
        i -= 1;
    }

    None
}

/// Detects any autocomplete query under the cursor: `@mention`, `#hashtag`, `/command`,
/// or `:emoji` keyword (falling back to [`crate::emoji::parse_emoji_colon_query`]).
pub fn parse_cursor_autocomplete_query(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    has_selection: bool,
    features: ComposeFeatures,
    max_emoji_query_chars: usize,
) -> Option<AutocompleteQuery> {
    if let Some(q) = parse_mention_hashtag_bot_command_query(
        text,
        cursor_byte,
        fragment_start_byte,
        has_selection,
        features,
    ) {
        return Some(q);
    }
    if has_selection {
        return None;
    }
    crate::emoji::parse_emoji_colon_query(
        text,
        cursor_byte,
        fragment_start_byte,
        max_emoji_query_chars,
    )
    .map(|eq| AutocompleteQuery {
        kind: AutocompleteKind::EmojiKeyword,
        query: eq.query,
        from_start: eq.start_byte == 0,
        start_byte: eq.start_byte,
        end_byte: eq.end_byte,
    })
}

/// Parses an inline bot query (`@botusername query...`) from the start of the field.
///
/// Direct port of `ParseInlineBotQuery` in `Telegram/SourceFiles/chat_helpers/message_field.cpp`.
/// - `first_tag_offset_chars`: char offset of the first formatting tag in the field, if any.
/// - `lookup_user`: resolves a username (case-insensitively) to a cached `UserInfo`.
pub fn parse_inline_bot_query<'a, F>(
    text: &str,
    first_tag_offset_chars: Option<usize>,
    lookup_user: F,
) -> InlineBotQuery
where
    F: Fn(&str) -> Option<&'a UserInfo>,
{
    let mut result = InlineBotQuery::default();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let text_len = chars.len();
    let inline_username_start = 1usize;
    let mut inline_username_len = 0usize;

    if text_len > 2 && chars[0].1 == '@' && chars[1].1.is_alphabetic() {
        inline_username_len = 1;
        for &(_, ch) in &chars[(inline_username_start + 1)..] {
            if ch.is_alphanumeric() || ch == '_' {
                inline_username_len += 1;
                continue;
            } else if !ch.is_whitespace() {
                inline_username_len = 0;
            }
            break;
        }
        let inline_username_end = inline_username_start + inline_username_len;
        let equals_text = inline_username_end == text_len;
        let valid = if equals_text {
            text.ends_with("bot")
        } else if inline_username_end < text_len && inline_username_len > 0 {
            chars[inline_username_end].1.is_whitespace()
        } else {
            false
        };

        if valid {
            if let Some(tag_offset) = first_tag_offset_chars
                && tag_offset < inline_username_end
            {
                return InlineBotQuery::default();
            }
            let start_byte = chars[inline_username_start].0;
            let end_byte = if inline_username_end < text_len {
                chars[inline_username_end].0
            } else {
                text.len()
            };
            let username = &text[start_byte..end_byte];
            result.username = username.to_string();

            if let Some(user) = lookup_user(username) {
                result.looking_up_bot = false;
                if user.is_bot && !user.inline_placeholder.is_empty() {
                    result.bot_id = Some(user.id);
                    result.query = if equals_text {
                        String::new()
                    } else {
                        let query_start = if inline_username_end + 1 < text_len {
                            chars[inline_username_end + 1].0
                        } else {
                            text.len()
                        };
                        text[query_start..].to_string()
                    };
                    return result;
                }
                result.bot_id = None;
            } else {
                result.bot_id = None;
                result.looking_up_bot = true;
                result.query = if equals_text {
                    String::new()
                } else {
                    let query_start = if inline_username_end + 1 < text_len {
                        chars[inline_username_end + 1].0
                    } else {
                        text.len()
                    };
                    text[query_start..].to_string()
                };
                return result;
            }
        } else {
            inline_username_len = 0;
        }
    }

    if inline_username_len < 3 {
        result.bot_id = None;
        result.username.clear();
    }
    result.query.clear();
    result
}

/// Prepends a space if the first non-object-replacement character is `@`, preventing an inline bot
/// query from accidentally triggering.
///
/// Direct port of `PreventInlineBotQuery` in `chat_helpers/message_field.cpp`.
pub fn prevent_inline_bot_query(text: &str) -> String {
    let first = text.chars().find(|&c| c != '\u{FFFC}');
    if first == Some('@') {
        format!(" {text}")
    } else {
        text.to_string()
    }
}

/// Source of a mention autocomplete row, matching `FieldAutocomplete::MentionRow::Source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionSource {
    /// From the user's recent inline bots list.
    InlineRecent,
    /// From top guest chat bots.
    GuestChatTopPeer,
    /// Participant / admin / recent author in the chat.
    MentionCandidate,
}

/// Single mention autocomplete suggestion row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionRow {
    /// Suggested user ID.
    pub user_id: UserId,
    /// Primary username (may be empty for username-less users).
    pub username: String,
    /// Display name for rendering or username-less mention insertion.
    pub display_name: String,
    /// Origin of this suggestion.
    pub source: MentionSource,
}

impl MentionRow {
    /// Whether the user can click the delete (×) button on this row.
    ///
    /// Matches `MentionRow::removable()` in `chat_helpers/field_autocomplete.cpp`.
    pub const fn removable(&self) -> bool {
        matches!(
            self.source,
            MentionSource::InlineRecent | MentionSource::GuestChatTopPeer
        )
    }
}

/// Context describing the chat/megagroup/user for mention and bot command autocomplete.
#[derive(Debug, Clone, Default)]
pub enum AutocompletePeerContext<'a> {
    /// Private 1-on-1 chat with a user or bot.
    User {
        /// Target user.
        user: &'a UserInfo,
        /// Bot commands (only used when `user.is_bot`).
        bot_commands: &'a [BotCommand],
        /// Current user (self) for business shortcuts.
        self_user: Option<&'a UserInfo>,
        /// Business quick-reply shortcuts (only used when `!user.is_bot`).
        business_shortcuts: &'a [BusinessShortcut],
    },
    /// Legacy basic group (`ChatData`).
    BasicChat {
        /// Full participants list (may be empty if not loaded yet).
        participants: &'a [&'a UserInfo],
        /// Most recent message authors in the chat (most recent first).
        last_authors: &'a [&'a UserInfo],
        /// Bot presence status.
        bot_status: BotStatus,
        /// Per-bot commands map.
        bot_commands: &'a HashMap<UserId, Vec<BotCommand>>,
    },
    /// Supergroup / megagroup (`ChannelData` with `isMegagroup()`).
    Megagroup {
        /// Whether current user can view full members list.
        can_view_members: bool,
        /// Recent participants (used when `can_view_members` is true).
        last_participants: &'a [&'a UserInfo],
        /// Group admins (used when `can_view_members` is false).
        admins: &'a [&'a UserInfo],
        /// Bots present in the megagroup.
        bots: &'a [&'a UserInfo],
        /// Bot presence status.
        bot_status: BotStatus,
        /// Per-bot commands map.
        bot_commands: &'a HashMap<UserId, Vec<BotCommand>>,
    },
    /// Broadcast channel or peer without mention/command participants.
    #[default]
    BroadcastChannel,
}

/// Filters and ranks `@mention` suggestions for `raw_query` (with or without leading `@`).
///
/// Direct port of `FieldAutocomplete::updateFiltered` (`Type::Mentions`) in
/// `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`.
pub fn filter_mentions(
    peer: &AutocompletePeerContext<'_>,
    raw_query: &str,
    add_inline_bots: bool,
    recent_inline_bots: &[&UserInfo],
    guest_chat_bots: &[&UserInfo],
    now: TimeId,
) -> Vec<MentionRow> {
    let plain = raw_query.strip_prefix('@').unwrap_or(raw_query);
    let filter = remove_accents(&plain.to_lowercase());
    let list_all = filter.is_empty();

    let filter_not_passed_by_username = |user: &UserInfo| -> bool {
        let username = user.primary_username();
        let lower_user = username.to_lowercase();
        if lower_user.starts_with(&filter) {
            // Exact match hides the suggestion, matching C++ `username.size() == _filter.size()`.
            return lower_user.chars().count() == filter.chars().count();
        }
        true
    };

    let filter_not_passed_by_name = |user: &UserInfo| -> bool {
        for word in &user.name_words {
            if remove_accents(&word.to_lowercase()).starts_with(&filter) {
                let exact_username = user.primary_username().eq_ignore_ascii_case(&filter)
                    || user.primary_username().to_lowercase() == filter;
                return exact_username;
            }
        }
        filter_not_passed_by_username(user)
    };

    let mut mrows: Vec<MentionRow> = Vec::new();

    let push_mention_row = |mrows: &mut Vec<MentionRow>, user: &UserInfo, source: MentionSource| {
        if mrows.iter().any(|r| r.user_id == user.id) {
            return;
        }
        mrows.push(MentionRow {
            user_id: user.id,
            username: user.primary_username().to_string(),
            display_name: user.mention_display_name().to_string(),
            source,
        });
    };

    let mark_mention_candidate_if_exists = |mrows: &mut Vec<MentionRow>, user: &UserInfo| -> bool {
        if let Some(row) = mrows.iter_mut().find(|r| r.user_id == user.id) {
            row.source = MentionSource::MentionCandidate;
            return true;
        }
        false
    };

    if add_inline_bots {
        for &user in recent_inline_bots {
            if user.is_inaccessible || (!list_all && filter_not_passed_by_username(user)) {
                continue;
            }
            push_mention_row(&mut mrows, user, MentionSource::InlineRecent);
        }
    }

    for &user in guest_chat_bots {
        if user.is_inaccessible
            || !user.is_bot
            || (!list_all && filter_not_passed_by_username(user))
            || mrows.iter().any(|r| r.user_id == user.id)
        {
            continue;
        }
        push_mention_row(&mut mrows, user, MentionSource::GuestChatTopPeer);
    }

    let skip_user = |user: &UserInfo| -> bool {
        user.is_inaccessible || (!list_all && filter_not_passed_by_name(user))
    };

    match peer {
        AutocompletePeerContext::BasicChat {
            participants,
            last_authors,
            ..
        } => {
            // `base::flat_multi_map<TimeId, User*>` iterated in reverse => highest online value first.
            let mut sorted: Vec<(TimeId, &UserInfo)> = Vec::new();
            for &user in *participants {
                if skip_user(user) || mark_mention_candidate_if_exists(&mut mrows, user) {
                    continue;
                }
                let key =
                    sort_by_online_value(user.is_service_user || user.is_bot, user.lastseen, now);
                sorted.push((key, user));
            }
            // Stable sort ascending by TimeId (matching flat_multi_map insertion order for ties),
            // then remove last_authors and iterate in reverse.
            sorted.sort_by_key(|(t, _)| *t);

            for &user in *last_authors {
                if skip_user(user) {
                    continue;
                }
                if mark_mention_candidate_if_exists(&mut mrows, user) {
                    sorted.retain(|(_, u)| u.id != user.id);
                    continue;
                }
                push_mention_row(&mut mrows, user, MentionSource::MentionCandidate);
                sorted.retain(|(_, u)| u.id != user.id);
            }

            for (_, user) in sorted.into_iter().rev() {
                push_mention_row(&mut mrows, user, MentionSource::MentionCandidate);
            }
        }
        AutocompletePeerContext::Megagroup {
            can_view_members,
            last_participants,
            admins,
            ..
        } => {
            let list = if *can_view_members {
                *last_participants
            } else {
                *admins
            };
            for &user in list {
                if skip_user(user) || mark_mention_candidate_if_exists(&mut mrows, user) {
                    continue;
                }
                push_mention_row(&mut mrows, user, MentionSource::MentionCandidate);
            }
        }
        AutocompletePeerContext::User { .. } | AutocompletePeerContext::BroadcastChannel => {}
    }

    mrows
}

/// Recent inline bots store with LRU bump and limit of 10.
///
/// Direct port of `Data::RecentInlineBots` in `data/components/recent_inline_bots.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentInlineBots {
    list: Vec<UserId>,
}

impl RecentInlineBots {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Creates a store initialized from persisted data.
    pub fn from_list(list: Vec<UserId>) -> Self {
        Self { list }
    }

    /// Returns the ordered list of recent inline bot IDs (most recent first).
    pub fn list(&self) -> &[UserId] {
        &self.list
    }

    /// Moves `user_id` to the front (inserting it and evicting the oldest if at limit 10).
    /// Returns `true` if the list changed.
    pub fn bump(&mut self, user_id: UserId) -> bool {
        if self.list.first() == Some(&user_id) {
            return false;
        }
        if let Some(pos) = self.list.iter().position(|&id| id == user_id) {
            self.list[..=pos].rotate_right(1);
        } else {
            if self.list.len() >= MAX_RECENT_INLINE_BOTS {
                self.list.pop();
            }
            self.list.insert(0, user_id);
        }
        true
    }

    /// Removes `user_id` from the recent inline bots list. Returns `true` if removed.
    pub fn remove(&mut self, user_id: UserId) -> bool {
        if let Some(pos) = self.list.iter().position(|&id| id == user_id) {
            self.list.remove(pos);
            return true;
        }
        false
    }
}

/// Recent hashtag history with frequency-based ordering and exponential decay.
///
/// Direct port of `Local::incrementRecentHashtag` in `storage/localstorage.cpp` and
/// hashtag filtering/removal in `chat_helpers/field_autocomplete.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentHashtags {
    /// `(tag_without_hash, usage_rating)` sorted descending by rating.
    entries: Vec<(String, u16)>,
}

impl RecentHashtags {
    /// Creates an empty hashtag history.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates a hashtag history from persisted `(tag, rating)` pairs.
    pub fn from_entries(entries: Vec<(String, u16)>) -> Self {
        Self { entries }
    }

    /// Returns the stored `(tag, rating)` pairs in display order.
    pub fn entries(&self) -> &[(String, u16)] {
        &self.entries
    }

    /// Records a usage of `tag` (without leading `#`), updating rating, applying decay
    /// if `rating > 0x4000`, enforcing the 64-item cap, and maintaining order.
    ///
    /// Exact port of `Local::incrementRecentHashtag` in `storage/localstorage.cpp`.
    pub fn increment(&mut self, tag: &str) {
        let tag = tag.strip_prefix('#').unwrap_or(tag);
        if let Some(mut idx) = self.entries.iter().position(|(t, _)| t == tag) {
            self.entries[idx].1 = self.entries[idx].1.saturating_add(1);
            if self.entries[idx].1 > HASHTAG_DECAY_THRESHOLD {
                for (_, rating) in &mut self.entries {
                    if *rating > 1 {
                        *rating /= 2;
                    } else if *rating > 0 {
                        *rating = 1;
                    }
                }
            }
            while idx > 0 {
                if self.entries[idx - 1].1 > self.entries[idx].1 {
                    break;
                }
                self.entries.swap(idx, idx - 1);
                idx -= 1;
            }
        } else {
            while self.entries.len() >= MAX_RECENT_HASHTAGS {
                self.entries.pop();
            }
            self.entries.push((tag.to_string(), 1));
            let mut idx = self.entries.len() - 1;
            while idx > 0 {
                if self.entries[idx - 1].1 > self.entries[idx].1 {
                    break;
                }
                self.entries.swap(idx, idx - 1);
                idx -= 1;
            }
        }
    }

    /// Removes `tag` (with or without leading `#`) from history. Returns `true` if any removed.
    pub fn remove(&mut self, tag: &str) -> bool {
        let tag = tag.strip_prefix('#').unwrap_or(tag);
        let before = self.entries.len();
        self.entries.retain(|(t, _)| t != tag);
        self.entries.len() != before
    }

    /// Filters hashtags matching `raw_query` (with or without leading `#`).
    ///
    /// Exact port of `FieldAutocomplete::updateFiltered` (`Type::Hashtags`):
    /// skips exact-length matches and compares accent-stripped, case-insensitive prefixes.
    pub fn filter(&self, raw_query: &str) -> Vec<String> {
        let plain = raw_query.strip_prefix('#').unwrap_or(raw_query);
        let filter = remove_accents(&plain.to_lowercase());
        let list_all = filter.is_empty();
        let filter_chars = filter.chars().count();

        let mut result = Vec::with_capacity(self.entries.len());
        for (tag, _) in &self.entries {
            if !list_all {
                let normalized_tag = remove_accents(&tag.to_lowercase());
                if tag.chars().count() == filter_chars || !normalized_tag.starts_with(&filter) {
                    continue;
                }
            }
            result.push(tag.clone());
        }
        result
    }
}

/// Single bot command (or business quick-reply shortcut) suggestion row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotCommandRow {
    /// Owning bot user ID (or self user ID for business quick-reply shortcuts).
    pub user_id: UserId,
    /// Bot's primary username (empty for shortcuts or username-less bots).
    pub bot_username: String,
    /// Command name without leading `/` (empty for the special "Edit shortcuts" header row).
    pub command: String,
    /// Command description (or message count string for shortcuts).
    pub description: String,
    /// Whether the command triggers an ephemeral bot reply.
    pub ephemeral: bool,
    /// True if this row represents a business quick-reply shortcut rather than a bot command.
    pub is_shortcut: bool,
    /// Whether the formatted command string should include `@bot_username`.
    pub with_username: bool,
}

impl BotCommandRow {
    /// Formats the command text to insert or send, matching `FieldAutocomplete::Inner::chooseAtIndex`.
    pub fn formatted_command(&self) -> String {
        if self.with_username && !self.bot_username.is_empty() {
            format!("/{}@{}", self.command, self.bot_username)
        } else {
            format!("/{}", self.command)
        }
    }
}

/// Filters and orders `/command` suggestions for `raw_query` (with or without leading `/`).
///
/// Direct port of `FieldAutocomplete::updateFiltered` (`Type::BotCommands`) in
/// `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`.
pub fn filter_bot_commands(
    peer: &AutocompletePeerContext<'_>,
    raw_query: &str,
) -> Vec<BotCommandRow> {
    let plain = raw_query.strip_prefix('/').unwrap_or(raw_query);
    let filter = remove_accents(&plain.to_lowercase());
    let list_all = filter.is_empty();
    let has_username = filter.find('@').is_some_and(|idx| idx > 0);

    let bot_status = match peer {
        AutocompletePeerContext::BasicChat { bot_status, .. }
        | AutocompletePeerContext::Megagroup { bot_status, .. } => *bot_status,
        _ => BotStatus::NoBots,
    };
    let with_username = has_username || bot_status != BotStatus::NoBots;

    let mut bots: BTreeMap<UserId, (&UserInfo, &[BotCommand])> = BTreeMap::new();
    match peer {
        AutocompletePeerContext::BasicChat {
            participants,
            bot_commands,
            ..
        } => {
            for &user in *participants {
                if !user.is_bot {
                    continue;
                }
                if let Some(cmds) = bot_commands.get(&user.id) {
                    bots.insert(user.id, (user, cmds.as_slice()));
                }
            }
        }
        AutocompletePeerContext::User {
            user, bot_commands, ..
        } if user.is_bot => {
            bots.insert(user.id, (*user, *bot_commands));
        }
        AutocompletePeerContext::Megagroup {
            bots: mg_bots,
            bot_commands,
            ..
        } => {
            for &user in *mg_bots {
                if !user.is_bot {
                    continue;
                }
                if let Some(cmds) = bot_commands.get(&user.id) {
                    bots.insert(user.id, (user, cmds.as_slice()));
                }
            }
        }
        _ => {}
    }

    let passes = |user: &UserInfo, cmd: &BotCommand| -> bool {
        if list_all {
            return true;
        }
        let target = if with_username {
            format!("{}@{}", cmd.command, user.primary_username()).to_lowercase()
        } else {
            cmd.command.to_lowercase()
        };
        target.starts_with(&filter)
    };

    let make_row = |user: &UserInfo, cmd: &BotCommand| -> BotCommandRow {
        BotCommandRow {
            user_id: user.id,
            bot_username: user.primary_username().to_string(),
            command: cmd.command.clone(),
            description: cmd.description.clone(),
            ephemeral: false,
            is_shortcut: false,
            with_username,
        }
    };

    let mut brows: Vec<BotCommandRow> = Vec::new();
    if !bots.is_empty() {
        if let AutocompletePeerContext::BasicChat { last_authors, .. } = peer {
            for &user in *last_authors {
                if !user.is_bot {
                    continue;
                }
                if let Some((u, cmds)) = bots.remove(&user.id) {
                    for cmd in cmds {
                        if passes(u, cmd) {
                            brows.push(make_row(u, cmd));
                        }
                    }
                }
            }
        }
        for (_, (user, cmds)) in bots {
            for cmd in cmds {
                if passes(user, cmd) {
                    brows.push(make_row(user, cmd));
                }
            }
        }
    }

    // Business quick-reply shortcuts in 1-on-1 non-bot chats (lines 776-800 & 2116-2123).
    if let AutocompletePeerContext::User {
        user,
        self_user: Some(self_user),
        business_shortcuts,
        ..
    } = peer
        && !user.is_bot
        && user.stars_per_message == 0
        && !has_username
        && brows.is_empty()
        && !business_shortcuts.is_empty()
    {
        for shortcut in *business_shortcuts {
            if shortcut.count < 1 {
                continue;
            }
            if !list_all && !shortcut.name.to_lowercase().starts_with(&filter) {
                continue;
            }
            brows.push(BotCommandRow {
                user_id: self_user.id,
                bot_username: String::new(),
                command: shortcut.name.clone(),
                description: format!("{} messages", shortcut.count),
                ephemeral: false,
                is_shortcut: true,
                with_username: false,
            });
        }
        if !brows.is_empty() {
            brows.insert(
                0,
                BotCommandRow {
                    user_id: self_user.id,
                    bot_username: String::new(),
                    command: String::new(),
                    description: String::new(),
                    ephemeral: false,
                    is_shortcut: true,
                    with_username: false,
                },
            );
        }
    }

    brows
}

/// Wraps a bot command with `@bot_username` when sent in a group that has bots.
///
/// Direct port of `Bot::WrapCommandInChat` in `Telegram/SourceFiles/chat_helpers/bot_command.cpp`.
pub fn wrap_command_in_chat(bot_status: BotStatus, command: &str, bot: &UserInfo) -> String {
    let username = bot.primary_username();
    if !bot.is_bot || username.is_empty() {
        return command.to_string();
    }
    let at_pos = command.find('@');
    let needs_username = at_pos.is_none_or(|idx| idx < 2) && bot_status != BotStatus::NoBots;
    if needs_username {
        format!("{command}@{username}")
    } else {
        command.to_string()
    }
}

/// Builds the internal `mention://` tag string for a username-less mention.
///
/// Direct port of `PrepareMentionTag` in `Telegram/SourceFiles/chat_helpers/message_field.cpp`:
/// `mention://<user_id>.<access_hash>:<self_id>`.
pub fn prepare_mention_tag(user_id: UserId, access_hash: u64, self_id: UserId) -> String {
    tg_text::prepare_mention_tag(user_id.0, access_hash, self_id.0)
}

/// Result of applying an autocomplete insertion into the message field text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionResult {
    /// Complete updated field text.
    pub text: String,
    /// New cursor position in UTF-8 bytes (placed right after the inserted text + space).
    pub new_cursor_byte: usize,
    /// Byte offset where replacement started.
    pub replaced_start_byte: usize,
    /// Original byte offset where replacement ended (including any consumed trailing space).
    pub replaced_end_byte: usize,
    /// Optional `MentionName` entity when inserting a mention without a username (or via Ctrl+Click).
    pub mention_entity: Option<MentionNameEntity>,
}

/// Action to take when a bot command row is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommandChoiceAction {
    /// Insert the command text into the field (when chosen via `Tab` and `command.len() > 1`).
    Insert(InsertionResult),
    /// Send the bot command immediately (`Enter` or `Click` on a bot command).
    SendImmediately {
        /// Command string to send (e.g. `/help` or `/help@mybot`).
        command: String,
        /// Remaining text in the field after the cursor position.
        remaining_text: String,
    },
    /// Trigger a business quick-reply shortcut (when `is_shortcut` and not `Tab`).
    ProcessShortcut {
        /// Shortcut name without leading `/` (empty means "open shortcut editor").
        shortcut_name: String,
    },
}

/// Computes the exact byte span `[start, end)` that `InputField::insertTag` replaces around `cursor_byte`.
///
/// Direct port of the scan loop in `Ui::InputField::insertTag` (`lib_ui/ui/widgets/fields/input_field.cpp:2840-2888`).
pub fn find_insert_tag_span(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
) -> (usize, usize) {
    if cursor_byte > text.len()
        || fragment_start_byte > cursor_byte
        || !text.is_char_boundary(cursor_byte)
        || !text.is_char_boundary(fragment_start_byte)
    {
        return (cursor_byte.min(text.len()), cursor_byte.min(text.len()));
    }

    let fragment = &text[fragment_start_byte..];
    let chars: Vec<(usize, char)> = fragment.char_indices().collect();
    let cursor_rel_byte = cursor_byte - fragment_start_byte;
    let cursor_char_idx = chars
        .iter()
        .position(|&(b, _)| b == cursor_rel_byte)
        .unwrap_or(chars.len());

    let mut mention_in_command = false;
    let mut i = cursor_char_idx;
    while i > 0 {
        let (prev_byte, previous) = chars[i - 1];
        if previous == '@' || previous == '#' || previous == '/' {
            let after_ok = i == cursor_char_idx
                || previous == '#'
                || if previous == '/' || previous == '@' {
                    chars[i].1.is_alphanumeric()
                } else {
                    chars[i].1.is_alphabetic()
                };
            let before_is_word =
                i >= 2 && (chars[i - 2].1.is_alphanumeric() || chars[i - 2].1 == '_');

            if after_ok && !before_is_word {
                let start_byte = fragment_start_byte + prev_byte;
                let mut till_idx = i;
                while till_idx < chars.len() && till_idx < cursor_char_idx {
                    let ch = chars[till_idx].1;
                    if !ch.is_alphanumeric() && ch != '_' && ch != '@' {
                        break;
                    }
                    till_idx += 1;
                }
                if till_idx < chars.len() && chars[till_idx].1 == ' ' {
                    till_idx += 1;
                }
                let end_byte = if till_idx < chars.len() {
                    fragment_start_byte + chars[till_idx].0
                } else {
                    text.len()
                };
                return (start_byte, end_byte);
            } else if (i == cursor_char_idx || chars[i].1.is_alphanumeric())
                && previous == '@'
                && i > 2
                && before_is_word
                && !mention_in_command
            {
                mention_in_command = true;
                i -= 1;
                continue;
            }
            break;
        }
        let dist = cursor_char_idx - i;
        if dist > MAX_COMMAND_WITH_BOT_QUERY_CHARS - 1
            || (!mention_in_command && dist > MAX_AUTOCOMPLETE_QUERY_CHARS - 1)
            || (!previous.is_alphanumeric() && previous != '_')
        {
            break;
        }
        i -= 1;
    }

    (cursor_byte, cursor_byte)
}

/// Inserts a chosen mention into `text` at `cursor_byte`.
///
/// Exact port of `InitFieldAutocomplete`'s `mentionChosen` handler + `InputField::insertTag`:
/// - If `user.primary_username()` is non-empty and `!(ctrl_pressed && method == ChooseMethod::ByClick)`,
///   inserts `@<username> ` with no entity.
/// - Otherwise inserts `<mention_display_name> ` and returns a [`MentionNameEntity`] spanning
///   the inserted display name (excluding the trailing space) with tag `mention://<id>.<hash>:<self>`.
pub fn apply_mention_insertion(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    user: &UserInfo,
    self_id: UserId,
    method: ChooseMethod,
    ctrl_pressed: bool,
) -> InsertionResult {
    let (start_byte, end_byte) = find_insert_tag_span(text, cursor_byte, fragment_start_byte);
    let username = user.primary_username();
    let use_name_entity = username.is_empty() || (ctrl_pressed && method == ChooseMethod::ByClick);

    let (inserted_core, entity) = if use_name_entity {
        let display = user.mention_display_name();
        let utf16_offset = utf16_len(&text[..start_byte]);
        let utf16_length = utf16_len(display);
        let tag = prepare_mention_tag(user.id, user.access_hash, self_id);
        (
            display.to_string(),
            Some(MentionNameEntity {
                byte_offset: start_byte,
                byte_length: display.len(),
                utf16_offset,
                utf16_length,
                user_id: user.id,
                access_hash: user.access_hash,
                tag,
            }),
        )
    } else {
        (format!("@{username}"), None)
    };

    let mut new_text = String::with_capacity(text.len() + inserted_core.len() + 1);
    new_text.push_str(&text[..start_byte]);
    new_text.push_str(&inserted_core);
    new_text.push(' ');
    let new_cursor_byte = new_text.len();
    new_text.push_str(&text[end_byte..]);

    InsertionResult {
        text: new_text,
        new_cursor_byte,
        replaced_start_byte: start_byte,
        replaced_end_byte: end_byte,
        mention_entity: entity,
    }
}

/// Inserts a chosen hashtag into `text` at `cursor_byte` (appending a trailing space).
pub fn apply_hashtag_insertion(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    hashtag: &str,
) -> InsertionResult {
    let (start_byte, end_byte) = find_insert_tag_span(text, cursor_byte, fragment_start_byte);
    let with_hash = if hashtag.starts_with('#') {
        hashtag.to_string()
    } else {
        format!("#{hashtag}")
    };

    let mut new_text = String::with_capacity(text.len() + with_hash.len() + 1);
    new_text.push_str(&text[..start_byte]);
    new_text.push_str(&with_hash);
    new_text.push(' ');
    let new_cursor_byte = new_text.len();
    new_text.push_str(&text[end_byte..]);

    InsertionResult {
        text: new_text,
        new_cursor_byte,
        replaced_start_byte: start_byte,
        replaced_end_byte: end_byte,
        mention_entity: None,
    }
}

/// Resolves choosing a bot command row (inserting on Tab, sending immediately on Enter/Click,
/// or triggering a business shortcut).
///
/// Direct port of `InitFieldAutocomplete`'s `botCommandChosen` handler (`field_autocomplete.cpp:2061-2080`).
pub fn apply_bot_command_choice(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    row: &BotCommandRow,
    method: ChooseMethod,
) -> BotCommandChoiceAction {
    let formatted = row.formatted_command();
    let by_tab = method == ChooseMethod::ByTab;

    if by_tab && formatted.len() > 1 {
        let (start_byte, end_byte) = find_insert_tag_span(text, cursor_byte, fragment_start_byte);
        let mut new_text = String::with_capacity(text.len() + formatted.len() + 1);
        new_text.push_str(&text[..start_byte]);
        new_text.push_str(&formatted);
        new_text.push(' ');
        let new_cursor_byte = new_text.len();
        new_text.push_str(&text[end_byte..]);

        BotCommandChoiceAction::Insert(InsertionResult {
            text: new_text,
            new_cursor_byte,
            replaced_start_byte: start_byte,
            replaced_end_byte: end_byte,
            mention_entity: None,
        })
    } else if !row.is_shortcut {
        let tail = if cursor_byte <= text.len() && text.is_char_boundary(cursor_byte) {
            text[cursor_byte..].to_string()
        } else {
            String::new()
        };
        BotCommandChoiceAction::SendImmediately {
            command: formatted,
            remaining_text: tail,
        }
    } else {
        BotCommandChoiceAction::ProcessShortcut {
            shortcut_name: row.command.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SECONDS_IN_DAY;
    use tg_data::LastseenStatus;

    fn make_user(
        id: u64,
        first_name: &str,
        username: &str,
        lastseen: LastseenStatus,
        is_bot: bool,
    ) -> UserInfo {
        let name_words = first_name
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        UserInfo {
            id: UserId(id),
            access_hash: id * 1000 + 7,
            first_name: first_name.to_string(),
            display_name: first_name.to_string(),
            username: username.to_string(),
            usernames: if username.is_empty() {
                Vec::new()
            } else {
                vec![username.to_string()]
            },
            name_words,
            is_bot,
            lastseen,
            ..UserInfo::default()
        }
    }

    #[test]
    fn parse_queries_under_cursor() {
        let features = ComposeFeatures::default();

        // @mention at start
        let q = parse_mention_hashtag_bot_command_query("@ali", 4, 0, false, features).unwrap();
        assert_eq!(q.kind, AutocompleteKind::Mentions);
        assert_eq!(q.query, "@ali");
        assert!(q.from_start);
        assert_eq!((q.start_byte, q.end_byte), (0, 4));

        // @mention after space
        let q =
            parse_mention_hashtag_bot_command_query("hello @bo", 9, 0, false, features).unwrap();
        assert_eq!(q.kind, AutocompleteKind::Mentions);
        assert_eq!(q.query, "@bo");
        assert!(!q.from_start);
        assert_eq!((q.start_byte, q.end_byte), (6, 9));

        // Email address should NOT trigger mention
        assert!(
            parse_mention_hashtag_bot_command_query("user@domain", 11, 0, false, features)
                .is_none()
        );

        // #hashtag
        let q = parse_mention_hashtag_bot_command_query("see #rust_2024", 14, 0, false, features)
            .unwrap();
        assert_eq!(q.kind, AutocompleteKind::Hashtags);
        assert_eq!(q.query, "#rust_2024");
        assert!(!q.from_start);

        // /bot command only at start of field
        let q = parse_mention_hashtag_bot_command_query("/start", 6, 0, false, features).unwrap();
        assert_eq!(q.kind, AutocompleteKind::BotCommands);
        assert_eq!(q.query, "/start");
        assert!(q.from_start);

        // /command with @bot
        let q =
            parse_mention_hashtag_bot_command_query("/help@mybot", 11, 0, false, features).unwrap();
        assert_eq!(q.kind, AutocompleteKind::BotCommands);
        assert_eq!(q.query, "/help@mybot");
        assert!(q.from_start);

        // /command NOT at start must fail
        assert!(
            parse_mention_hashtag_bot_command_query("hi /start", 9, 0, false, features).is_none()
        );

        // Selection suppresses autocomplete
        assert!(parse_mention_hashtag_bot_command_query("@ali", 4, 0, true, features).is_none());

        // Length limit: 64 chars after trigger is allowed (dist=63), 65 breaks
        let ok_64 = format!("@{}", "a".repeat(64));
        assert!(
            parse_mention_hashtag_bot_command_query(&ok_64, ok_64.len(), 0, false, features)
                .is_some()
        );
        let too_long = format!("@{}", "a".repeat(65));
        assert!(
            parse_mention_hashtag_bot_command_query(&too_long, too_long.len(), 0, false, features)
                .is_none()
        );
    }

    #[test]
    fn parse_inline_bot_query_cases() {
        let mut gif_bot = make_user(10, "GIF", "gif", LastseenStatus::long_ago(false), true);
        gif_bot.inline_placeholder = "Search GIFs".to_string();
        let non_inline = make_user(
            11,
            "Helper",
            "helperbot",
            LastseenStatus::long_ago(false),
            true,
        );

        let lookup = |name: &str| -> Option<&UserInfo> {
            match name {
                "gif" => Some(&gif_bot),
                "helperbot" => Some(&non_inline),
                _ => None,
            }
        };

        let q = parse_inline_bot_query("@gif cats", None, lookup);
        assert_eq!(q.username, "gif");
        assert_eq!(q.query, "cats");
        assert_eq!(q.bot_id, Some(UserId(10)));
        assert!(!q.looking_up_bot);

        // Ends with "bot" without trailing space triggers lookup
        let q = parse_inline_bot_query("@unknownbot", None, lookup);
        assert_eq!(q.username, "unknownbot");
        assert_eq!(q.query, "");
        assert_eq!(q.bot_id, None);
        assert!(q.looking_up_bot);

        // Known bot without inline placeholder yields no bot
        let q = parse_inline_bot_query("@helperbot test", None, lookup);
        assert_eq!(q.bot_id, None);
        assert_eq!(q.query, "");

        // Tag intersecting username cancels inline query
        let q = parse_inline_bot_query("@gif cats", Some(2), lookup);
        assert!(q.username.is_empty());

        // Prevent inline bot query
        assert_eq!(prevent_inline_bot_query("@gif"), " @gif");
        assert_eq!(prevent_inline_bot_query("\u{FFFC}@gif"), " \u{FFFC}@gif");
        assert_eq!(prevent_inline_bot_query("hello"), "hello");
    }

    #[test]
    fn mention_ranking_and_deduplication() {
        let now: TimeId = 1_700_000_000;
        let inline_bot = make_user(1, "InlineBot", "gif", LastseenStatus::long_ago(false), true);
        let guest_bot = make_user(2, "GuestBot", "gbot", LastseenStatus::long_ago(false), true);
        let alice = make_user(
            3,
            "Alice",
            "alice_a",
            LastseenStatus::online_till_status(now + 300, false, false),
            false,
        );
        let bob = make_user(4, "Bob", "", LastseenStatus::recently(false), false);
        let charlie = make_user(
            5,
            "Charlie",
            "charlie",
            LastseenStatus::within_month(false),
            false,
        );
        let dave = make_user(6, "Dave", "dave", LastseenStatus::within_week(false), false);
        let deleted = UserInfo {
            is_inaccessible: true,
            ..make_user(7, "Deleted", "del", LastseenStatus::recently(false), false)
        };

        let participants = [&charlie, &bob, &alice, &dave, &deleted, &inline_bot];
        // Bob is the most recent author even though Alice has a higher online timestamp.
        let last_authors = [&bob];
        let peer = AutocompletePeerContext::BasicChat {
            participants: &participants,
            last_authors: &last_authors,
            bot_status: BotStatus::HasBots,
            bot_commands: &HashMap::new(),
        };

        let rows = filter_mentions(&peer, "@", true, &[&inline_bot], &[&guest_bot], now);
        let ids: Vec<u64> = rows.iter().map(|r| r.user_id.0).collect();
        // Order: inline_bot (upgraded to MentionCandidate because also in participants!),
        // guest_bot (GuestChatTopPeer), bob (last_author),
        // then remaining participants sorted descending by online value:
        // alice (now+300) > dave (now - 7d) > charlie (now - 30d).
        assert_eq!(ids, vec![1, 2, 4, 3, 6, 5]);
        assert_eq!(rows[0].source, MentionSource::MentionCandidate);
        assert!(!rows[0].removable());
        assert_eq!(rows[1].source, MentionSource::GuestChatTopPeer);
        assert!(rows[1].removable());

        // Verify sort_by_online_value exact values
        assert_eq!(
            sort_by_online_value(false, LastseenStatus::recently(false), now),
            now - 3 * SECONDS_IN_DAY
        );
        assert_eq!(
            sort_by_online_value(
                true,
                LastseenStatus::online_till_status(now, false, false),
                now
            ),
            -1
        );

        // Exact username match is hidden in filter
        let rows_exact = filter_mentions(&peer, "@dave", false, &[], &[], now);
        assert!(rows_exact.is_empty());

        // Prefix match by name finds username-less Bob
        let rows_bob = filter_mentions(&peer, "@bo", false, &[], &[], now);
        assert_eq!(rows_bob.len(), 1);
        assert_eq!(rows_bob[0].user_id, UserId(4));
        assert!(rows_bob[0].username.is_empty());
    }

    #[test]
    fn recent_hashtags_ordering_decay_and_limit() {
        let mut tags = RecentHashtags::new();
        tags.increment("rust");
        tags.increment("telegram");
        // Equal rating (1): newer bubbles ahead of older!
        assert_eq!(
            tags.entries(),
            &[("telegram".to_string(), 1), ("rust".to_string(), 1)]
        );

        tags.increment("rust");
        assert_eq!(
            tags.entries(),
            &[("rust".to_string(), 2), ("telegram".to_string(), 1)]
        );

        // Filtering skips exact-length match and is accent-insensitive
        tags.increment("café");
        assert_eq!(tags.filter("#caf"), vec!["café".to_string()]);
        assert!(tags.filter("#cafe").is_empty()); // same length (4) => skipped

        // Cap at 64
        for i in 0..70 {
            tags.increment(&format!("tag{i}"));
        }
        assert_eq!(tags.entries().len(), MAX_RECENT_HASHTAGS);

        // Decay when rating exceeds 0x4000 (16384)
        let mut decay_tags = RecentHashtags::from_entries(vec![
            ("hot".to_string(), HASHTAG_DECAY_THRESHOLD),
            ("warm".to_string(), 10),
            ("cold".to_string(), 1),
        ]);
        decay_tags.increment("hot");
        // hot: 16384 -> 16385 (> 0x4000) -> 16385 / 2 = 8192
        // warm: 10 / 2 = 5
        // cold: stays 1
        assert_eq!(
            decay_tags.entries(),
            &[
                ("hot".to_string(), 8192),
                ("warm".to_string(), 5),
                ("cold".to_string(), 1),
            ]
        );
    }

    #[test]
    fn recent_inline_bots_lru_and_limit() {
        let mut store = RecentInlineBots::new();
        for i in 1..=12 {
            assert!(store.bump(UserId(i)));
        }
        assert_eq!(store.list().len(), MAX_RECENT_INLINE_BOTS);
        assert_eq!(store.list()[0], UserId(12));
        assert_eq!(store.list()[9], UserId(3));

        // Bumping front is a no-op
        assert!(!store.bump(UserId(12)));
        // Bumping an existing element rotates it to front
        assert!(store.bump(UserId(5)));
        assert_eq!(store.list()[0], UserId(5));
        assert_eq!(store.list()[1], UserId(12));

        assert!(store.remove(UserId(5)));
        assert_eq!(store.list()[0], UserId(12));
    }

    #[test]
    fn bot_commands_in_group_and_wrapping() {
        let bot_a = make_user(
            10,
            "BotA",
            "alpha_bot",
            LastseenStatus::long_ago(false),
            true,
        );
        let bot_b = make_user(
            20,
            "BotB",
            "beta_bot",
            LastseenStatus::long_ago(false),
            true,
        );
        let mut cmds = HashMap::new();
        cmds.insert(
            bot_a.id,
            vec![BotCommand {
                command: "start".into(),
                description: "Start A".into(),
            }],
        );
        cmds.insert(
            bot_b.id,
            vec![BotCommand {
                command: "start".into(),
                description: "Start B".into(),
            }],
        );

        let participants = [&bot_a, &bot_b];
        // bot_b spoke most recently, so its commands appear before bot_a
        let last_authors = [&bot_b];
        let peer = AutocompletePeerContext::BasicChat {
            participants: &participants,
            last_authors: &last_authors,
            bot_status: BotStatus::HasBots,
            bot_commands: &cmds,
        };

        let rows = filter_bot_commands(&peer, "/st");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].user_id, bot_b.id);
        assert_eq!(rows[0].formatted_command(), "/start@beta_bot");
        assert_eq!(rows[1].user_id, bot_a.id);
        assert_eq!(rows[1].formatted_command(), "/start@alpha_bot");

        // Filtering with @bot username narrows to that bot
        let rows_b = filter_bot_commands(&peer, "/start@be");
        assert_eq!(rows_b.len(), 1);
        assert_eq!(rows_b[0].user_id, bot_b.id);

        // WrapCommandInChat
        assert_eq!(
            wrap_command_in_chat(BotStatus::HasBots, "/help", &bot_a),
            "/help@alpha_bot"
        );
        assert_eq!(
            wrap_command_in_chat(BotStatus::NoBots, "/help", &bot_a),
            "/help"
        );
        assert_eq!(
            wrap_command_in_chat(BotStatus::HasBots, "/help@other", &bot_a),
            "/help@other"
        );
    }

    #[test]
    fn mention_insertion_with_and_without_username() {
        let alice = make_user(42, "Alice", "alice", LastseenStatus::recently(false), false);
        let bob = make_user(99, "Боб", "", LastseenStatus::recently(false), false);
        let self_id = UserId(7);

        // With username -> @alice + space, no entity
        let res = apply_mention_insertion(
            "Hi @al",
            6,
            0,
            &alice,
            self_id,
            ChooseMethod::ByEnter,
            false,
        );
        assert_eq!(res.text, "Hi @alice ");
        assert_eq!(res.new_cursor_byte, 10);
        assert!(res.mention_entity.is_none());

        // Without username -> display name + space + MentionNameEntity
        let res = apply_mention_insertion(
            "Привет @бо",
            "Привет @бо".len(),
            0,
            &bob,
            self_id,
            ChooseMethod::ByEnter,
            false,
        );
        assert_eq!(res.text, "Привет Боб ");
        let entity = res.mention_entity.expect("must emit MentionNameEntity");
        assert_eq!(entity.user_id, UserId(99));
        assert_eq!(entity.utf16_offset, 7); // "Привет " is 7 UTF-16 code units
        assert_eq!(entity.utf16_length, 3); // "Боб" is 3 UTF-16 code units
        assert_eq!(entity.tag, "mention://99.99007:7");

        // Ctrl+Click on user WITH username still inserts name + entity
        let res =
            apply_mention_insertion("@al", 3, 0, &alice, self_id, ChooseMethod::ByClick, true);
        assert_eq!(res.text, "Alice ");
        assert!(res.mention_entity.is_some());

        // Existing trailing space after cursor is consumed rather than doubled
        let res = apply_mention_insertion(
            "Hi @al there",
            6,
            0,
            &alice,
            self_id,
            ChooseMethod::ByTab,
            false,
        );
        assert_eq!(res.text, "Hi @alice there");
        assert_eq!(res.new_cursor_byte, 10);
    }

    #[test]
    fn bot_command_choice_tab_vs_enter() {
        let row = BotCommandRow {
            user_id: UserId(10),
            bot_username: "alpha_bot".into(),
            command: "help".into(),
            description: "Help".into(),
            ephemeral: false,
            is_shortcut: false,
            with_username: true,
        };

        // Tab inserts into field
        match apply_bot_command_choice("/he", 3, 0, &row, ChooseMethod::ByTab) {
            BotCommandChoiceAction::Insert(ins) => {
                assert_eq!(ins.text, "/help@alpha_bot ");
                assert_eq!(ins.new_cursor_byte, 16);
            }
            other => panic!("expected Insert, got {other:?}"),
        }

        // Enter sends immediately
        match apply_bot_command_choice("/he rest", 3, 0, &row, ChooseMethod::ByEnter) {
            BotCommandChoiceAction::SendImmediately {
                command,
                remaining_text,
            } => {
                assert_eq!(command, "/help@alpha_bot");
                assert_eq!(remaining_text, " rest");
            }
            other => panic!("expected SendImmediately, got {other:?}"),
        }
    }

    #[test]
    fn megagroup_admins_business_shortcuts_and_unified_emoji_query() {
        let now: TimeId = 1_700_000_000;
        let admin = make_user(1, "Admin", "adm", LastseenStatus::recently(false), false);
        let member = make_user(2, "Member", "mem", LastseenStatus::recently(false), false);
        let mg = AutocompletePeerContext::Megagroup {
            can_view_members: false,
            last_participants: &[&member],
            admins: &[&admin],
            bots: &[],
            bot_status: BotStatus::NoBots,
            bot_commands: &HashMap::new(),
        };
        let rows = filter_mentions(&mg, "@", false, &[], &[], now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].user_id, admin.id);

        // Business shortcuts in 1-on-1 non-bot free chat prepend Edit row ("")
        let mut self_user = make_user(100, "Me", "me", LastseenStatus::recently(false), false);
        self_user.is_self = true;
        let shortcuts = [
            BusinessShortcut {
                id: 1,
                name: "hello".into(),
                count: 2,
            },
            BusinessShortcut {
                id: 2,
                name: "empty".into(),
                count: 0, // skipped because count < 1
            },
        ];
        let peer = AutocompletePeerContext::User {
            user: &member,
            bot_commands: &[],
            self_user: Some(&self_user),
            business_shortcuts: &shortcuts,
        };
        let brows = filter_bot_commands(&peer, "/he");
        assert_eq!(brows.len(), 2);
        assert!(brows[0].is_shortcut && brows[0].command.is_empty());
        assert_eq!(brows[1].command, "hello");

        // Unified cursor autocomplete detects :emoji keyword
        let eq =
            parse_cursor_autocomplete_query("hi :roc", 7, 0, false, ComposeFeatures::default(), 32)
                .expect("should detect :roc");
        assert_eq!(eq.kind, AutocompleteKind::EmojiKeyword);
        assert_eq!(eq.query, ":roc");
    }
}
