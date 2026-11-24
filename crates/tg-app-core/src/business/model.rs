//! Data model and algorithms for Telegram Business and Telegram Premium screens.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/business/data_business_common.{h,cpp}`
//! - `Telegram/SourceFiles/data/business/data_business_info.{h,cpp}`
//! - `Telegram/SourceFiles/data/business/data_business_chatbots.{h,cpp}`
//! - `Telegram/SourceFiles/data/business/data_shortcut_messages.{h,cpp}`
//! - `Telegram/SourceFiles/api/api_chat_links.h`
//! - `Telegram/SourceFiles/data/data_premium_limits.cpp`
//! - `Telegram/SourceFiles/data/data_emoji_statuses.h`

use std::collections::BTreeMap;

/// Seconds in one day (`WorkingInterval::kDay`).
pub const SECONDS_PER_DAY: i32 = 24 * 3600;
/// Seconds in one week (`WorkingInterval::kWeek`).
pub const SECONDS_PER_WEEK: i32 = 7 * SECONDS_PER_DAY;
/// Maximum spillover into the next day displayed as part of the current day
/// (`WorkingInterval::kInNextDayMax` = 6 hours).
pub const IN_NEXT_DAY_MAX: i32 = 6 * 3600;

/// Minimum allowed custom away-schedule start timestamp (2013-08-01 00:00:00 UTC,
/// matching `StartTimeMin()` in `settings_away_message.cpp`).
pub const AWAY_START_TIME_MIN: i32 = 1_375_315_200;
/// Minimum allowed custom away-schedule end timestamp (`EndTimeMin()` = `StartTimeMin() + 3600`).
pub const AWAY_END_TIME_MIN: i32 = AWAY_START_TIME_MIN + 3600;

/// Default inactivity threshold in days for Greeting messages (`kDefaultNoActivityDays`).
pub const DEFAULT_NO_ACTIVITY_DAYS: i32 = 7;
/// Standard inactivity day presets in `settings_greeting.cpp` (`EditPeriodBox`).
pub const GREETING_PERIOD_CHOICES: [i32; 4] = [7, 14, 21, 28];

/// Maximum characters allowed in a quick reply shortcut name (`kShortcutLimit`).
pub const SHORTCUT_NAME_LIMIT: usize = 32;
/// Default maximum number of quick reply shortcuts (`quick_replies_limit`).
pub const QUICK_REPLIES_LIMIT: usize = 100;
/// Default maximum number of messages inside a single shortcut (`quick_reply_messages_limit`).
pub const SHORTCUT_MESSAGES_LIMIT: usize = 20;
/// Default maximum number of business chat links (`business_chat_links_limit`).
pub const CHAT_LINKS_LIMIT: usize = 100;
/// Default maximum character length of a Chat Intro title (`intro_title_length_limit`).
pub const CHAT_INTRO_TITLE_LIMIT: usize = 32;
/// Default maximum character length of a Chat Intro description (`intro_description_length_limit`).
pub const CHAT_INTRO_DESCRIPTION_LIMIT: usize = 70;
/// Maximum bot search results displayed in `settings_chatbots.cpp` (`kMaxSearchBots`).
pub const MAX_SEARCH_BOTS: usize = 3;

/// Identifier of a Telegram Business quick-reply shortcut.
pub type BusinessShortcutId = i32;

// -----------------------------------------------------------------------------
// Working Hours & Timezones (`data_business_common.cpp`, `data_business_info.cpp`)
// -----------------------------------------------------------------------------

/// A half-open time interval `[start, end)` measured in seconds from Monday 00:00
/// (or from 00:00 of an individual day after extraction), or Unix seconds for custom
/// away schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkingInterval {
    /// Interval start (seconds).
    pub start: i32,
    /// Interval end (seconds).
    pub end: i32,
}

impl WorkingInterval {
    /// Creates a new [`WorkingInterval`].
    #[must_use]
    pub const fn new(start: i32, end: i32) -> Self {
        Self { start, end }
    }

    /// Returns `true` when the interval is non-empty (`start < end`), matching
    /// `WorkingInterval::operator bool()`.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.start < self.end
    }

    /// Shifts both endpoints by `offset` seconds.
    #[must_use]
    pub const fn shifted(self, offset: i32) -> Self {
        Self {
            start: self.start + offset,
            end: self.end + offset,
        }
    }

    /// Returns the bounding union of two intervals (handling empty operands).
    #[must_use]
    pub fn united(self, other: Self) -> Self {
        if !self.is_valid() {
            return other;
        }
        if !other.is_valid() {
            return self;
        }
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Returns the intersection of two intervals, or an empty default interval if disjoint.
    #[must_use]
    pub fn intersected(self, other: Self) -> Self {
        let candidate = Self {
            start: self.start.max(other.start),
            end: self.end.min(other.end),
        };
        if candidate.is_valid() {
            candidate
        } else {
            Self::default()
        }
    }
}

/// Ordered collection of [`WorkingInterval`] entries with tdesktop normalization and
/// per-day slicing algorithms.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkingIntervals {
    /// Underlying list of intervals.
    pub list: Vec<WorkingInterval>,
}

impl WorkingIntervals {
    /// Creates a [`WorkingIntervals`] from a vector of intervals.
    #[must_use]
    pub fn new(list: Vec<WorkingInterval>) -> Self {
        Self { list }
    }

    /// Returns `true` if at least one interval in `list` is valid (`start < end`).
    #[must_use]
    pub fn is_non_empty(&self) -> bool {
        self.list.iter().any(|iv| iv.is_valid())
    }

    /// Sorts, merges overlapping/adjacent intervals, wraps spillover past `kWeek` back
    /// to Monday morning, and re-merges — exact port of `WorkingIntervals::normalized()`.
    #[must_use]
    pub fn normalized(&self) -> Self {
        sort_and_merge(move_tail_to_front(sort_and_merge(self.clone())))
    }
}

fn sort_and_merge(mut intervals: WorkingIntervals) -> WorkingIntervals {
    let list = &mut intervals.list;
    list.sort_by_key(|iv| iv.start);
    let mut i = 0usize;
    while i < list.len() {
        if i > 0 && list[i].is_valid() && list[i - 1].is_valid() && list[i].start <= list[i - 1].end
        {
            list[i - 1] = list[i - 1].united(list[i]);
            list[i] = WorkingInterval::default();
        }
        if !list[i].is_valid() {
            list.remove(i);
        } else {
            i += 1;
        }
    }
    intervals
}

fn move_tail_to_front(mut intervals: WorkingIntervals) -> WorkingIntervals {
    let list = &mut intervals.list;
    let after = WorkingInterval::new(SECONDS_PER_WEEK, SECONDS_PER_WEEK + SECONDS_PER_DAY);
    while let Some(last) = list.last_mut() {
        let tail = last.intersected(after);
        if tail.is_valid() {
            last.end = tail.start;
            if !last.is_valid() {
                list.pop();
            }
            list.insert(0, tail.shifted(-SECONDS_PER_WEEK));
        } else {
            break;
        }
    }
    intervals
}

/// Extracts intervals belonging to `day_index` (`0` = Monday .. `6` = Sunday),
/// preserving next-day spillover up to [`IN_NEXT_DAY_MAX`] (6 hours) and omitting
/// previous-day spillover into early morning (`<= 6h`).
///
/// Exact port of `Data::ExtractDayIntervals` in `data_business_common.cpp`.
#[must_use]
pub fn extract_day_intervals(intervals: &WorkingIntervals, day_index: i32) -> WorkingIntervals {
    debug_assert!((0..7).contains(&day_index));
    let day_index = day_index.clamp(0, 6);

    let mut result = WorkingIntervals::default();
    for &interval in &intervals.list {
        let now = interval.intersected(WorkingInterval::new(
            (day_index - 1) * SECONDS_PER_DAY,
            (day_index + 2) * SECONDS_PER_DAY,
        ));
        let after = interval.intersected(WorkingInterval::new(
            (day_index + 6) * SECONDS_PER_DAY,
            (day_index + 9) * SECONDS_PER_DAY,
        ));
        let before = interval.intersected(WorkingInterval::new(
            (day_index - 8) * SECONDS_PER_DAY,
            (day_index - 5) * SECONDS_PER_DAY,
        ));
        if now.is_valid() {
            result.list.push(now.shifted(-day_index * SECONDS_PER_DAY));
        }
        if after.is_valid() {
            result
                .list
                .push(after.shifted(-(day_index + 7) * SECONDS_PER_DAY));
        }
        if before.is_valid() {
            result
                .list
                .push(before.shifted(-(day_index - 7) * SECONDS_PER_DAY));
        }
    }
    result = result.normalized();

    result
        .list
        .retain(|iv| iv.end > 0 && iv.start < SECONDS_PER_DAY);

    if let Some(last) = result.list.last_mut() {
        if last.start <= 0 && last.end >= SECONDS_PER_DAY {
            *last = WorkingInterval::new(0, SECONDS_PER_DAY);
        } else if last.end > SECONDS_PER_DAY + IN_NEXT_DAY_MAX {
            *last = last.intersected(WorkingInterval::new(0, SECONDS_PER_DAY));
        }
    }
    if let Some(&first) = result.list.first()
        && first.start <= 0
    {
        if first.start < 0 && first.end <= IN_NEXT_DAY_MAX && first.start > -SECONDS_PER_DAY {
            result.list.remove(0);
        } else {
            let clipped = first.intersected(WorkingInterval::new(0, SECONDS_PER_DAY));
            if clipped.is_valid() {
                result.list[0] = clipped;
            } else {
                result.list.remove(0);
            }
        }
    }

    result
}

/// Returns `true` when an extracted day covers the full 24 hours (`00:00-23:59` or `00:00-24:00`).
///
/// Exact port of `Data::IsFullOpen` in `data_business_common.cpp`.
#[must_use]
pub fn is_full_open(extracted_day: &WorkingIntervals) -> bool {
    extracted_day.is_non_empty()
        && extracted_day.list.first().is_some_and(|&front| {
            front == WorkingInterval::new(0, SECONDS_PER_DAY - 60)
                || front == WorkingInterval::new(0, SECONDS_PER_DAY)
        })
}

/// Removes intervals belonging to `day_index` (`0..=6`) while preserving any spillover
/// owned by the previous day (finishing at `<= 06:00`), including Sunday wrapping
/// into Monday morning.
///
/// Exact port of `Data::RemoveDayIntervals` in `data_business_common.cpp`.
#[must_use]
pub fn remove_day_intervals(intervals: &WorkingIntervals, day_index: i32) -> WorkingIntervals {
    debug_assert!((0..7).contains(&day_index));
    let day_index = day_index.clamp(0, 6);

    let mut result = intervals.normalized();
    let shifted = WorkingInterval::new(0, SECONDS_PER_DAY).shifted(day_index * SECONDS_PER_DAY);
    let before = WorkingInterval::new(0, shifted.start);
    let after = WorkingInterval::new(shifted.end, SECONDS_PER_WEEK);

    let mut i = 0usize;
    let mut count = result.list.len();
    while i < count {
        let cur = result.list[i];
        let last_end = result.list.last().map_or(0, |iv| iv.end);
        if cur.end <= shifted.start || cur.start >= shifted.end {
            i += 1;
            continue;
        }
        if cur.end <= shifted.start + IN_NEXT_DAY_MAX
            && (cur.start < shifted.start
                || (day_index == 0 && cur.start == shifted.start && last_end >= SECONDS_PER_WEEK))
        {
            i += 1;
            continue;
        }
        let first = cur.intersected(before);
        let second = cur.intersected(after);
        if first.is_valid() {
            result.list[i] = first;
            if second.is_valid() {
                result.list.push(second);
            }
            i += 1;
        } else if second.is_valid() {
            result.list[i] = second;
            i += 1;
        } else {
            result.list.remove(i);
            count -= 1;
        }
    }

    result.normalized()
}

/// Replaces `day_index`'s intervals with `replacement` (day-local `[0, kDay + kInNextDayMax]`)
/// and returns the normalized weekly schedule.
///
/// Exact port of `Data::ReplaceDayIntervals` in `data_business_common.cpp`.
#[must_use]
pub fn replace_day_intervals(
    intervals: &WorkingIntervals,
    day_index: i32,
    replacement: WorkingIntervals,
) -> WorkingIntervals {
    let mut result = remove_day_intervals(intervals, day_index);
    let shift = day_index.clamp(0, 6) * SECONDS_PER_DAY;
    for iv in replacement.list {
        result.list.push(iv.shifted(shift));
    }
    result.normalized()
}

/// Complete business opening hours configuration (`Data::WorkingHours`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkingHours {
    /// Weekly open intervals.
    pub intervals: WorkingIntervals,
    /// IANA-like timezone identifier (e.g. `"America/New_York"`).
    pub timezone_id: String,
}

impl WorkingHours {
    /// Returns a copy with normalized intervals.
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            intervals: self.intervals.normalized(),
            timezone_id: self.timezone_id.clone(),
        }
    }

    /// Returns `true` when both timezone and at least one interval are configured.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.timezone_id.is_empty() && !self.intervals.list.is_empty()
    }
}

/// Timezone metadata (`Data::Timezone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timezone {
    /// Server timezone identifier.
    pub id: String,
    /// Human-readable display name (e.g. `"New York"`).
    pub name: String,
    /// Offset from UTC in seconds (e.g. `-18000` for UTC-5:00).
    pub utc_offset: i32,
}

impl Timezone {
    /// Formats `(UTC±H:MM) Name`, matching `TimezoneFullName` in `settings_working_hours.cpp`.
    #[must_use]
    pub fn full_name(&self) -> String {
        let abs = self.utc_offset.abs();
        let hours = abs / 3600;
        let minutes = (abs % 3600) / 60;
        let sign = if self.utc_offset < 0 { '-' } else { '+' };
        format!("(UTC{sign}{hours}:{minutes:02}) {}", self.name)
    }
}

/// Finds the timezone in `list` whose UTC offset is closest to `local_utc_offset_secs`
/// (with 24-hour wrap-around), matching `Data::FindClosestTimezoneId`.
#[must_use]
pub fn find_closest_timezone_id(list: &[Timezone], local_utc_offset_secs: i32) -> Option<String> {
    list.iter()
        .min_by_key(|tz| {
            let mut distance = tz.utc_offset - local_utc_offset_secs;
            while distance > 12 * 3600 {
                distance -= 24 * 3600;
            }
            while distance < -12 * 3600 {
                distance += 24 * 3600;
            }
            distance.abs()
        })
        .map(|tz| tz.id.clone())
}

/// Formats a day-local time in seconds as `HH:MM` or `HH:MM (next day)`,
/// matching `FormatDayTime` in `settings_working_hours.cpp`.
#[must_use]
pub fn format_day_time(time: i32, show_end_as_next_day: bool) -> String {
    let wrap = |value: i32| {
        let clamped = value.max(0);
        let hours = clamped / 3600;
        let minutes = (clamped % 3600) / 60;
        format!("{hours:02}:{minutes:02}")
    };
    if time > SECONDS_PER_DAY || (show_end_as_next_day && time == SECONDS_PER_DAY) {
        format!("{} (next day)", wrap(time - SECONDS_PER_DAY))
    } else if time == SECONDS_PER_DAY {
        wrap(0)
    } else {
        wrap(time)
    }
}

/// Formats a day's extracted [`WorkingIntervals`] as a comma-separated string,
/// matching `JoinIntervals` in `settings_working_hours.cpp`.
#[must_use]
pub fn join_intervals(data: &WorkingIntervals) -> String {
    data.list
        .iter()
        .map(|iv| {
            format!(
                "{} - {}",
                format_day_time(iv.start, false),
                format_day_time(iv.end, false)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Weekday display names indexed `0 = Monday .. 6 = Sunday`.
pub const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

// -----------------------------------------------------------------------------
// Recipients (`data_business_common.h`, `settings_recipients_helper.cpp`)
// -----------------------------------------------------------------------------

/// Bitflags for broad chat categories in [`BusinessChats`] (`Data::BusinessChatTypes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BusinessChatTypes(u8);

impl BusinessChatTypes {
    /// Empty category set.
    pub const EMPTY: Self = Self(0);
    /// New chats (`BusinessChatType::NewChats`).
    pub const NEW_CHATS: Self = Self(1 << 0);
    /// Existing chats (`BusinessChatType::ExistingChats`).
    pub const EXISTING_CHATS: Self = Self(1 << 1);
    /// Contacts (`BusinessChatType::Contacts`).
    pub const CONTACTS: Self = Self(1 << 2);
    /// Non-contacts (`BusinessChatType::NonContacts`).
    pub const NON_CONTACTS: Self = Self(1 << 3);

    /// Returns `true` when no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when all bits in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Adds the bits in `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Removes the bits in `other`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Toggles `flag` on or off.
    pub fn toggle(&mut self, flag: Self) {
        self.0 ^= flag.0;
    }

    /// Raw bitmask value.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns display labels for all enabled chat type flags.
    #[must_use]
    pub fn labels(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.contains(Self::NEW_CHATS) {
            out.push("New Chats");
        }
        if self.contains(Self::EXISTING_CHATS) {
            out.push("Existing Chats");
        }
        if self.contains(Self::CONTACTS) {
            out.push("Contacts");
        }
        if self.contains(Self::NON_CONTACTS) {
            out.push("Non-Contacts");
        }
        out
    }
}

/// Lightweight user descriptor used in recipient lists and bot cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessUserRef {
    /// Numeric peer/user ID.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Optional username without leading `@`.
    pub username: String,
    /// Avatar tint RGB.
    pub avatar_rgb: [u8; 3],
    /// Whether this user is in contacts.
    pub is_contact: bool,
}

impl BusinessUserRef {
    /// Two-letter initials for avatar circles.
    #[must_use]
    pub fn initials(&self) -> String {
        let mut chars = self
            .name
            .split_whitespace()
            .filter_map(|word| word.chars().next());
        let first = chars.next().unwrap_or('?');
        let second = chars.next();
        match second {
            Some(s) => format!("{}{}", first.to_uppercase(), s.to_uppercase()),
            None => first.to_uppercase().to_string(),
        }
    }
}

/// Set of chat categories plus explicit user IDs (`Data::BusinessChats`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BusinessChats {
    /// Selected chat type categories.
    pub types: BusinessChatTypes,
    /// Explicitly chosen user IDs.
    pub user_ids: Vec<i64>,
}

impl BusinessChats {
    /// Returns `true` when neither categories nor explicit users are selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.user_ids.is_empty()
    }

    /// Total count of selected categories + individual users.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.types.labels().len() + self.user_ids.len()
    }
}

/// Recipient selector mode (`Data::BusinessRecipientsType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusinessRecipientsType {
    /// Greeting / Away messages (either all-but-excluded OR selected-only).
    Messages,
    /// Connected chatbots (in selected-only mode, specific users can still be excluded).
    Bots,
}

/// Inclusion/exclusion recipient rules (`Data::BusinessRecipients`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BusinessRecipients {
    /// Included categories/users (when `all_but_excluded` is `false`).
    pub included: BusinessChats,
    /// Excluded categories/users (or excluded users in `Bots` mode).
    pub excluded: BusinessChats,
    /// When `true`, applies to all private chats except `excluded`.
    pub all_but_excluded: bool,
}

impl BusinessRecipients {
    /// Ensures recipient rules are valid (`BusinessRecipients::MakeValid`):
    /// if `included` is empty, switches to `all_but_excluded = true`.
    #[must_use]
    pub fn make_valid(mut self) -> Self {
        if self.included.is_empty() {
            self.all_but_excluded = true;
        }
        self
    }

    /// Returns a concise human-readable summary for hub status chips.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.all_but_excluded {
            let ex = self.excluded.entry_count();
            if ex == 0 {
                "All private chats".to_owned()
            } else {
                format!("All except {ex}")
            }
        } else {
            let inc = self.included.entry_count();
            if inc == 0 {
                "All private chats".to_owned()
            } else {
                format!("{inc} selected")
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Business Location, Chat Intro, Business Details (`data_business_common.h`)
// -----------------------------------------------------------------------------

/// Geographic coordinates (`Data::LocationPoint`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    /// Latitude in degrees (`-90.0..=90.0`).
    pub lat: f64,
    /// Longitude in degrees (`-180.0..=180.0`).
    pub lon: f64,
}

/// Business physical location (`Data::BusinessLocation`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BusinessLocation {
    /// Human-readable street address.
    pub address: String,
    /// Optional map pin coordinates.
    pub point: Option<GeoPoint>,
}

impl BusinessLocation {
    /// Returns `true` when an address is set.
    #[must_use]
    pub fn is_set(&self) -> bool {
        !self.address.trim().is_empty()
    }
}

/// Preset greeting sticker descriptor for the Chat Intro screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntroStickerOption {
    /// Synthetic document ID (`0` represents "Random").
    pub id: u64,
    /// Display title.
    pub name: String,
    /// Short symbol / art kind rendered inside the sticker preview.
    pub art_kind: String,
    /// Accent color RGB.
    pub accent_rgb: [u8; 3],
}

/// Business Start Page / Chat Intro (`Data::ChatIntro`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatIntro {
    /// Short headline shown in empty chats (max 32 chars).
    pub title: String,
    /// Short description shown below the title (max 70 chars).
    pub description: String,
    /// Optional chosen sticker ID (`None` means random greeting sticker).
    pub sticker_id: Option<u64>,
}

impl ChatIntro {
    /// Returns `true` when either title or description is non-empty.
    #[must_use]
    pub fn custom_phrases(&self) -> bool {
        !self.title.is_empty() || !self.description.is_empty()
    }

    /// Returns `true` when any field (title, description, or sticker) is configured.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.custom_phrases() || self.sticker_id.is_some()
    }
}

/// Aggregated profile business details (`Data::BusinessDetails`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BusinessDetails {
    /// Opening hours & timezone.
    pub hours: WorkingHours,
    /// Address & map pin.
    pub location: BusinessLocation,
    /// Empty-chat intro card.
    pub intro: ChatIntro,
}

// -----------------------------------------------------------------------------
// Greeting & Away Messages (`data_business_common.h`, `settings_away_message.cpp`)
// -----------------------------------------------------------------------------

/// Away message schedule mode (`Data::AwayScheduleType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AwayScheduleType {
    /// Disabled (`AwayScheduleType::Never`).
    #[default]
    Never,
    /// Always send away message (`AwayScheduleType::Always`).
    Always,
    /// Send outside configured opening hours (`AwayScheduleType::OutsideWorkingHours`).
    OutsideWorkingHours,
    /// Send during an explicit Unix timestamp range (`AwayScheduleType::Custom`).
    Custom,
}

/// Schedule configuration for away messages (`Data::AwaySchedule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AwaySchedule {
    /// Schedule type.
    pub schedule_type: AwayScheduleType,
    /// Custom Unix timestamp interval `[start, end)` (used when `Custom`).
    pub custom_interval: WorkingInterval,
}

impl AwaySchedule {
    /// Validates a custom interval against tdesktop's `BadCustomInterval` rules
    /// (`settings_away_message.cpp`: valid `start < end`, `start >= StartTimeMin`, `end >= EndTimeMin`).
    #[must_use]
    pub fn is_bad_custom_interval(interval: WorkingInterval) -> bool {
        !interval.is_valid()
            || interval.start < AWAY_START_TIME_MIN
            || interval.end < AWAY_END_TIME_MIN
    }
}

/// Complete Away Message configuration (`Data::AwaySettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AwaySettings {
    /// Target private chat recipients.
    pub recipients: BusinessRecipients,
    /// Active schedule.
    pub schedule: AwaySchedule,
    /// Linked `/away` shortcut ID.
    pub shortcut_id: BusinessShortcutId,
    /// Only send when the account owner is offline (`offline_only`).
    pub offline_only: bool,
}

impl AwaySettings {
    /// Returns `true` when away messages are enabled (`schedule_type != Never`).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.schedule.schedule_type != AwayScheduleType::Never
    }
}

/// Complete Greeting Message configuration (`Data::GreetingSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GreetingSettings {
    /// Target private chat recipients.
    pub recipients: BusinessRecipients,
    /// Re-send greeting after this many days of inactivity (`0` = disabled).
    pub no_activity_days: i32,
    /// Linked `/hello` shortcut ID.
    pub shortcut_id: BusinessShortcutId,
}

impl GreetingSettings {
    /// Returns `true` when greeting messages are enabled (`no_activity_days > 0`).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.no_activity_days > 0
    }
}

// -----------------------------------------------------------------------------
// Chatbots (`data_business_chatbots.{h,cpp}`, `settings_chatbots.cpp`)
// -----------------------------------------------------------------------------

/// Bitflags for connected business chatbot permissions (`Data::ChatbotsPermissions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatbotsPermissions(u16);

impl Default for ChatbotsPermissions {
    fn default() -> Self {
        Self::VIEW_MESSAGES
    }
}

impl ChatbotsPermissions {
    /// Always-on base permission (`0x0001`).
    pub const VIEW_MESSAGES: Self = Self(0x0001);
    /// Reply to messages (`0x0002`).
    pub const REPLY_TO_MESSAGES: Self = Self(0x0002);
    /// Mark messages as read (`0x0004`).
    pub const MARK_AS_READ: Self = Self(0x0004);
    /// Delete sent messages (`0x0008`).
    pub const DELETE_SENT: Self = Self(0x0008);
    /// Delete received messages (`0x0010`).
    pub const DELETE_RECEIVED: Self = Self(0x0010);
    /// Edit name (`0x0020`).
    pub const EDIT_NAME: Self = Self(0x0020);
    /// Edit bio (`0x0040`).
    pub const EDIT_BIO: Self = Self(0x0040);
    /// Edit profile photo (`0x0080`).
    pub const EDIT_USERPIC: Self = Self(0x0100);
    /// Edit username (`0x0100` in C++ is `EditUsername` after `EditUserpic=0x0080`).
    pub const EDIT_USERNAME: Self = Self(0x0200);
    /// View gifts & stars (`0x0200` in C++).
    pub const VIEW_GIFTS: Self = Self(0x0400);
    /// Sell gifts (`0x0400` in C++).
    pub const SELL_GIFTS: Self = Self(0x0800);
    /// Change gift settings (`0x0800` in C++).
    pub const GIFT_SETTINGS: Self = Self(0x1000);
    /// Transfer & upgrade gifts (`0x1000` in C++).
    pub const TRANSFER_GIFTS: Self = Self(0x2000);
    /// Transfer Stars (`0x2000` in C++).
    pub const TRANSFER_STARS: Self = Self(0x4000);
    /// Manage stories (`0x4000` in C++).
    pub const MANAGE_STORIES: Self = Self(0x8000);

    /// Returns `true` if `flag` is enabled.
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// Combines two permission sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Sets or clears `flag` (keeping `VIEW_MESSAGES` locked on).
    pub fn set(&mut self, flag: Self, enabled: bool) {
        if flag == Self::VIEW_MESSAGES {
            self.0 |= Self::VIEW_MESSAGES.0;
            return;
        }
        if enabled {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    /// Counts how many permissions are currently enabled.
    #[must_use]
    pub const fn enabled_count(self) -> u32 {
        self.0.count_ones()
    }
}

/// Group of chatbot permission entries matching `Data::ChatbotsPermissionsLabels()`.
#[derive(Debug, Clone, Copy)]
pub struct ChatbotsPermissionGroup {
    /// Optional section header (`None` for standalone Stories row).
    pub title: Option<&'static str>,
    /// Ordered `(flag, label, locked)` entries.
    pub items: &'static [(ChatbotsPermissions, &'static str, bool)],
}

/// Returns the 4 permission groups matching `Data::ChatbotsPermissionsLabels()` in `data_business_chatbots.cpp`.
#[must_use]
pub const fn chatbots_permission_groups() -> &'static [ChatbotsPermissionGroup] {
    &[
        ChatbotsPermissionGroup {
            title: Some("Messages"),
            items: &[
                (ChatbotsPermissions::VIEW_MESSAGES, "Read Messages", true),
                (
                    ChatbotsPermissions::REPLY_TO_MESSAGES,
                    "Reply to Messages",
                    false,
                ),
                (ChatbotsPermissions::MARK_AS_READ, "Mark as Read", false),
                (
                    ChatbotsPermissions::DELETE_SENT,
                    "Delete Sent Messages",
                    false,
                ),
                (
                    ChatbotsPermissions::DELETE_RECEIVED,
                    "Delete Received Messages",
                    false,
                ),
            ],
        },
        ChatbotsPermissionGroup {
            title: Some("Manage Profile"),
            items: &[
                (ChatbotsPermissions::EDIT_NAME, "Edit Name", false),
                (ChatbotsPermissions::EDIT_BIO, "Edit Bio", false),
                (
                    ChatbotsPermissions::EDIT_USERPIC,
                    "Edit Profile Photo",
                    false,
                ),
                (ChatbotsPermissions::EDIT_USERNAME, "Edit Username", false),
            ],
        },
        ChatbotsPermissionGroup {
            title: Some("Gifts and Stars"),
            items: &[
                (
                    ChatbotsPermissions::VIEW_GIFTS,
                    "View Gifts and Stars",
                    false,
                ),
                (
                    ChatbotsPermissions::SELL_GIFTS,
                    "Convert Gifts to Stars",
                    false,
                ),
                (
                    ChatbotsPermissions::GIFT_SETTINGS,
                    "Manage Gift Settings",
                    false,
                ),
                (
                    ChatbotsPermissions::TRANSFER_GIFTS,
                    "Transfer and Upgrade Gifts",
                    false,
                ),
                (ChatbotsPermissions::TRANSFER_STARS, "Transfer Stars", false),
            ],
        },
        ChatbotsPermissionGroup {
            title: None,
            items: &[(ChatbotsPermissions::MANAGE_STORIES, "Manage Stories", false)],
        },
    ]
}

/// Candidate bot returned by username search (`settings_chatbots.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessBotCandidate {
    /// Bot user ID.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Bot username without `@`.
    pub username: String,
    /// Short description.
    pub about: String,
    /// Whether the bot has `supportsBusiness = true`.
    pub supports_business: bool,
    /// Avatar RGB.
    pub avatar_rgb: [u8; 3],
}

/// Result of parsing a bot username/link query (`ParseUsernameQuery` in `settings_chatbots.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUsernameQuery {
    /// Extracted username fragment.
    pub value: String,
    /// Whether the extracted string matches `^[a-zA-Z0-9_.]+$`.
    pub valid: bool,
}

/// Parses `@bot`, `https://t.me/bot`, `t.me/bot`, or bare `bot` into a normalized query,
/// matching `ParseUsernameQuery` in `settings_chatbots.cpp`.
#[must_use]
pub fn parse_bot_username_query(raw: &str) -> ParsedUsernameQuery {
    let mut text = raw.trim();
    if let Some(stripped) = text.strip_prefix('@') {
        text = stripped;
    } else {
        let without_scheme = text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))
            .unwrap_or(text);
        if let Some((host, rest)) = without_scheme.split_once('/')
            && !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            text = rest.split(['/', '?', '#']).next().unwrap_or("");
        }
    }
    let valid = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    ParsedUsernameQuery {
        value: text.to_owned(),
        valid,
    }
}

/// Connected chatbot lookup state (`LookupState` in `settings_chatbots.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BotLookupState {
    /// Input empty or invalid.
    #[default]
    Empty,
    /// Exact match is a bot that does not support Telegram Business.
    Unsupported,
    /// Search completed (check candidates list; empty means not found).
    Ready,
}

/// Result of searching for a business bot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BotSearchResult {
    /// Matching business-capable bots (at most [`MAX_SEARCH_BOTS`]).
    pub bots: Vec<BusinessBotCandidate>,
    /// Lookup status.
    pub state: BotLookupState,
}

/// Connected business chatbot configuration (`Data::ChatbotsSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatbotsSettings {
    /// Currently connected bot, if any.
    pub bot: Option<BusinessBotCandidate>,
    /// Chat access rules (`BusinessRecipientsType::Bots`).
    pub recipients: BusinessRecipients,
    /// Granted bot permissions.
    pub permissions: ChatbotsPermissions,
}

// -----------------------------------------------------------------------------
// Quick Replies & Shortcut Messages (`data_shortcut_messages.{h,cpp}`)
// -----------------------------------------------------------------------------

/// Optional attachment kind on a quick reply message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuickReplyAttachment {
    /// Plain text message.
    #[default]
    None,
    /// Photo attachment placeholder.
    Photo,
    /// Document/PDF attachment placeholder.
    Document,
    /// Voice note attachment placeholder.
    Voice,
}

impl QuickReplyAttachment {
    /// Short badge label for message bubbles.
    #[must_use]
    pub const fn badge(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Photo => Some("[Photo]"),
            Self::Document => Some("[Document]"),
            Self::Voice => Some("[Voice 0:14]"),
        }
    }
}

/// One message inside a quick-reply shortcut (`Data::ShortcutMessages`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickReplyMessage {
    /// Message ID within the shortcut.
    pub id: i32,
    /// Message text body.
    pub text: String,
    /// Optional attachment placeholder.
    pub attachment: QuickReplyAttachment,
    /// Timestamp label for bubble display (e.g. `"10:15"`).
    pub time_label: String,
}

/// A named quick-reply shortcut (e.g. `/hello`, `/away`, `/pricing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickReplyShortcut {
    /// Unique shortcut ID.
    pub id: BusinessShortcutId,
    /// Shortcut command name without leading `/`.
    pub name: String,
    /// Ordered messages sent when this shortcut is triggered.
    pub messages: Vec<QuickReplyMessage>,
}

impl QuickReplyShortcut {
    /// Number of messages stored in this shortcut.
    #[must_use]
    pub fn count(&self) -> usize {
        self.messages.len()
    }

    /// Preview snippet of the first message for lists and `/` autocomplete.
    #[must_use]
    pub fn top_message_preview(&self) -> String {
        match self.messages.first() {
            Some(msg) => {
                let prefix = msg.attachment.badge().map_or("", |b| b);
                if prefix.is_empty() {
                    msg.text.clone()
                } else if msg.text.is_empty() {
                    prefix.to_owned()
                } else {
                    format!("{prefix} {}", msg.text)
                }
            }
            None => "No messages yet".to_owned(),
        }
    }
}

/// Validates a quick-reply shortcut name using the exact character rules from
/// `ValidShortcutName` in `settings_quick_replies.cpp`.
///
/// Allowed: length `1..=32`, Unicode letters/numbers, `_`, `U+200C` (ZWNJ),
/// `U+00B7` (middle dot), and Sinhala range `U+0D80..=U+0DFF`.
#[must_use]
pub fn valid_shortcut_name(name: &str) -> bool {
    let char_count = name.chars().count();
    if char_count == 0 || char_count > SHORTCUT_NAME_LIMIT {
        return false;
    }
    name.chars().all(|ch| {
        let cp = ch as u32;
        ch.is_alphanumeric()
            || ch == '_'
            || cp == 0x200C
            || cp == 0x00B7
            || (0x0D80..=0x0DFF).contains(&cp)
    })
}

/// Matches non-empty quick replies when a user types `/...` in a compose field.
///
/// Returns `None` when `compose_text` does not start with `/` (or contains whitespace
/// before matching), otherwise returns matching shortcuts ordered with prefix matches first.
#[must_use]
pub fn match_slash_quick_replies<'a>(
    shortcuts: &'a [QuickReplyShortcut],
    compose_text: &str,
) -> Option<Vec<&'a QuickReplyShortcut>> {
    let trimmed = compose_text.trim_start();
    let query = trimmed.strip_prefix('/')?;
    if query.contains(char::is_whitespace) {
        return None;
    }
    let q_lower = query.to_lowercase();
    let mut prefix_hits = Vec::new();
    let mut contains_hits = Vec::new();
    for sc in shortcuts {
        if sc.messages.is_empty() {
            continue;
        }
        let name_lower = sc.name.to_lowercase();
        if q_lower.is_empty() || name_lower.starts_with(&q_lower) {
            prefix_hits.push(sc);
        } else if name_lower.contains(&q_lower)
            || sc
                .messages
                .iter()
                .any(|m| m.text.to_lowercase().contains(&q_lower))
        {
            contains_hits.push(sc);
        }
    }
    prefix_hits.extend(contains_hits);
    Some(prefix_hits)
}

// -----------------------------------------------------------------------------
// Business Chat Links (`api/api_chat_links.h`, `settings_chat_links.cpp`)
// -----------------------------------------------------------------------------

/// A shareable `t.me/m/...` business chat link with optional prefilled message and click counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessChatLink {
    /// Full URL (e.g. `"https://t.me/m/nWr8Kx2Q"`).
    pub link: String,
    /// Optional private label shown only to the account owner.
    pub title: String,
    /// Prefilled text populated in the customer's compose box when opened.
    pub message: String,
    /// Total click count reported by the server.
    pub clicks: u32,
}

impl BusinessChatLink {
    /// Display name matching `Row::generateName()` in `settings_chat_links.cpp`.
    #[must_use]
    pub fn display_name(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else {
            self.link
                .strip_prefix("https://")
                .unwrap_or(&self.link)
                .to_owned()
        }
    }

    /// Click count label matching `ComputeClicks()` in `settings_chat_links.cpp`.
    #[must_use]
    pub fn clicks_label(&self) -> String {
        match self.clicks {
            0 => "No clicks".to_owned(),
            1 => "1 click".to_owned(),
            n => format!("{n} clicks"),
        }
    }
}

// -----------------------------------------------------------------------------
// Business Hub Feature Order (`settings/sections/settings_business.cpp`)
// -----------------------------------------------------------------------------

/// Business feature identifier matching `FallbackOrder()` in `settings_business.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusinessFeatureKey {
    /// Greeting Messages (`greeting_message`).
    GreetingMessage,
    /// Away Messages (`away_message`).
    AwayMessage,
    /// Quick Replies (`quick_replies`).
    QuickReplies,
    /// Opening Hours (`business_hours`).
    BusinessHours,
    /// Location (`business_location`).
    BusinessLocation,
    /// Links to Chat (`business_links`).
    ChatLinks,
    /// Start Page / Chat Intro (`business_intro`).
    ChatIntro,
    /// Chatbots (`business_bots`).
    BusinessBots,
    /// Folder Tags (`folder_tags`).
    FolderTags,
}

/// Default feature order matching `FallbackOrder()` in `settings_business.cpp`.
pub const BUSINESS_PROMO_ORDER: [BusinessFeatureKey; 9] = [
    BusinessFeatureKey::GreetingMessage,
    BusinessFeatureKey::AwayMessage,
    BusinessFeatureKey::QuickReplies,
    BusinessFeatureKey::BusinessHours,
    BusinessFeatureKey::BusinessLocation,
    BusinessFeatureKey::ChatLinks,
    BusinessFeatureKey::ChatIntro,
    BusinessFeatureKey::BusinessBots,
    BusinessFeatureKey::FolderTags,
];

/// Metadata for one row on the Business Hub screen.
#[derive(Debug, Clone, Copy)]
pub struct BusinessFeatureMeta {
    /// Feature identifier.
    pub key: BusinessFeatureKey,
    /// Display title.
    pub title: &'static str,
    /// Subtitle description.
    pub description: &'static str,
    /// Icon badge text.
    pub icon_glyph: &'static str,
    /// Icon badge color RGB.
    pub icon_rgb: [u8; 3],
}

/// Returns display metadata for a [`BusinessFeatureKey`].
#[must_use]
pub const fn business_feature_meta(key: BusinessFeatureKey) -> BusinessFeatureMeta {
    match key {
        BusinessFeatureKey::GreetingMessage => BusinessFeatureMeta {
            key,
            title: "Greeting Messages",
            description: "Automatically greet customers who message you for the first time.",
            icon_glyph: "HI",
            icon_rgb: [0x2A, 0x9D, 0xF4],
        },
        BusinessFeatureKey::AwayMessage => BusinessFeatureMeta {
            key,
            title: "Away Messages",
            description: "Reply automatically when you are offline or closed.",
            icon_glyph: "ZZ",
            icon_rgb: [0x58, 0x65, 0xF2],
        },
        BusinessFeatureKey::QuickReplies => BusinessFeatureMeta {
            key,
            title: "Quick Replies",
            description: "Create /shortcut presets with multiple messages and attachments.",
            icon_glyph: "/R",
            icon_rgb: [0x3B, 0xB5, 0x66],
        },
        BusinessFeatureKey::BusinessHours => BusinessFeatureMeta {
            key,
            title: "Opening Hours",
            description: "Show your weekly business schedule and local time zone.",
            icon_glyph: "HR",
            icon_rgb: [0xF5, 0x9E, 0x0B],
        },
        BusinessFeatureKey::BusinessLocation => BusinessFeatureMeta {
            key,
            title: "Location",
            description: "Display your store address and map pin on your profile.",
            icon_glyph: "PIN",
            icon_rgb: [0xE5, 0x53, 0x4B],
        },
        BusinessFeatureKey::ChatLinks => BusinessFeatureMeta {
            key,
            title: "Links to Chat",
            description: "Create shareable t.me/m links with prefilled messages and click stats.",
            icon_glyph: "URL",
            icon_rgb: [0x14, 0xB8, 0xA6],
        },
        BusinessFeatureKey::ChatIntro => BusinessFeatureMeta {
            key,
            title: "Start Page",
            description: "Customize the sticker, headline, and text shown in empty chats.",
            icon_glyph: "ST",
            icon_rgb: [0x9D, 0x5B, 0xE5],
        },
        BusinessFeatureKey::BusinessBots => BusinessFeatureMeta {
            key,
            title: "Chatbots",
            description: "Connect an automation bot with granular chat access and rights.",
            icon_glyph: "BOT",
            icon_rgb: [0x0E, 0xA5, 0xE9],
        },
        BusinessFeatureKey::FolderTags => BusinessFeatureMeta {
            key,
            title: "Folder Tags",
            description: "Display colorful chat folder tags next to conversations in your chat list.",
            icon_glyph: "TAG",
            icon_rgb: [0xEC, 0x48, 0x99],
        },
    }
}

// -----------------------------------------------------------------------------
// Telegram Premium, Limits & Emoji Status (`settings_premium.cpp`,
// `boxes/premium_limits_box.cpp`, `data_premium_limits.cpp`, `data_emoji_statuses.h`)
// -----------------------------------------------------------------------------

/// Exact default vs Telegram Premium numeric limits from `data/data_premium_limits.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumLimitsData {
    /// Joined channels & supergroups (`500` / `1000`).
    pub channels: (i32, i32),
    /// Public channel/group usernames (`10` / `20`).
    pub channels_public: (i32, i32),
    /// Similar channels shown (`10` / `100`).
    pub similar_channels: (i32, i32),
    /// Saved GIFs (`200` / `400`).
    pub saved_gifs: (i32, i32),
    /// Favorite stickers (`5` / `10`).
    pub stickers_faved: (i32, i32),
    /// Custom chat folders (`10` / `30`).
    pub dialog_filters: (i32, i32),
    /// Chats per folder (`100` / `200`).
    pub dialog_filters_chats: (i32, i32),
    /// Joined shareable folders (`2` / `20`).
    pub shareable_filters: (i32, i32),
    /// Invite links per shareable folder (`3` / `20`).
    pub filter_links: (i32, i32),
    /// Pinned chats in main list (`5` / `10`).
    pub dialogs_pinned: (i32, i32),
    /// Pinned chats in archived folder (`100` / `200`).
    pub folder_pinned: (i32, i32),
    /// Pinned chats in Saved Messages (`5` / `100`).
    pub saved_sublists_pinned: (i32, i32),
    /// Media caption characters (`1024` / `2048`).
    pub caption_length: (i32, i32),
    /// Max message characters (`4096` / `8192`).
    pub message_length: (i32, i32),
    /// Max file upload size in GB (`2` / `4`, derived from `4000` / `8000` 512KB parts).
    pub upload_max_gb: (i32, i32),
    /// Bio length (`70` / `140`).
    pub about_length: (i32, i32),
    /// Connected accounts (`3` / `4`, `Main::Domain::kMaxAccounts` / `kPremiumMaxAccounts`).
    pub accounts: (i32, i32),
    /// Expiring active stories (`50` / `100`).
    pub stories_expiring: (i32, i32),
    /// Story caption length (`200` / `2048`).
    pub story_caption_length: (i32, i32),
}

impl Default for PremiumLimitsData {
    fn default() -> Self {
        Self {
            channels: (500, 1_000),
            channels_public: (10, 20),
            similar_channels: (10, 100),
            saved_gifs: (200, 400),
            stickers_faved: (5, 10),
            dialog_filters: (10, 30),
            dialog_filters_chats: (100, 200),
            shareable_filters: (2, 20),
            filter_links: (3, 20),
            dialogs_pinned: (5, 10),
            folder_pinned: (100, 200),
            saved_sublists_pinned: (5, 100),
            caption_length: (1_024, 2_048),
            message_length: (4_096, 8_192),
            upload_max_gb: (2, 4),
            about_length: (70, 140),
            accounts: (3, 4),
            stories_expiring: (50, 100),
            story_caption_length: (200, 2_048),
        }
    }
}

/// Limit-exceeded modal variants ported from `boxes/premium_limits_box.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PremiumLimitKind {
    /// Custom chat folders (`FiltersLimitBox`).
    Folders,
    /// Chats inside a folder (`FilterChatsLimitBox`).
    FolderChats,
    /// Shareable folders (`ShareableFiltersLimitBox`).
    ShareableFolders,
    /// Invite links per folder (`FilterLinksLimitBox`).
    FolderLinks,
    /// Pinned chats in main list (`PinsLimitBox`).
    PinnedChats,
    /// Pinned chats in Saved Messages (`SublistsPinsLimitBox`).
    SavedPins,
    /// Channels & supergroups (`ChannelsLimitBox`, with inactive channel cleanup list).
    Channels,
    /// Public channel/group usernames (`PublicLinksLimitBox`, with revoke list).
    PublicLinks,
    /// Saved GIFs (`gifsDefault` / `gifsPremium`).
    SavedGifs,
    /// Favorite stickers (`stickersFavedDefault` / `stickersFavedPremium`).
    FavoriteStickers,
    /// Media caption length (`CaptionLimitBox`).
    CaptionLength,
    /// Upload file size (`FileSizeLimitBox`).
    FileSize,
    /// Profile bio length (`aboutLengthDefault` / `aboutLengthPremium`).
    BioLength,
    /// Connected desktop accounts (`AccountsLimitBox`).
    Accounts,
}

impl PremiumLimitKind {
    /// All limit box variants in display order.
    pub const ALL: &'static [Self] = &[
        Self::Folders,
        Self::FolderChats,
        Self::ShareableFolders,
        Self::FolderLinks,
        Self::PinnedChats,
        Self::SavedPins,
        Self::Channels,
        Self::PublicLinks,
        Self::SavedGifs,
        Self::FavoriteStickers,
        Self::CaptionLength,
        Self::FileSize,
        Self::BioLength,
        Self::Accounts,
    ];

    /// Short selector label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Folders => "Folders",
            Self::FolderChats => "Chats / Folder",
            Self::ShareableFolders => "Shared Folders",
            Self::FolderLinks => "Folder Links",
            Self::PinnedChats => "Pinned Chats",
            Self::SavedPins => "Saved Pins",
            Self::Channels => "Channels",
            Self::PublicLinks => "Public Links",
            Self::SavedGifs => "Saved GIFs",
            Self::FavoriteStickers => "Fav Stickers",
            Self::CaptionLength => "Caption Length",
            Self::FileSize => "File Size",
            Self::BioLength => "Bio Length",
            Self::Accounts => "Accounts",
        }
    }

    /// Modal box title matching `boxes/premium_limits_box.cpp`.
    #[must_use]
    pub const fn box_title(self) -> &'static str {
        match self {
            Self::Folders => "Folder Limit Reached",
            Self::FolderChats => "Chat Limit Reached",
            Self::ShareableFolders => "Shared Folders Limit",
            Self::FolderLinks => "Invite Links Limit",
            Self::PinnedChats | Self::SavedPins => "Pin Limit Reached",
            Self::Channels => "Channels Limit Reached",
            Self::PublicLinks => "Public Links Limit",
            Self::SavedGifs => "Saved GIFs Limit",
            Self::FavoriteStickers => "Favorite Stickers Limit",
            Self::CaptionLength => "Caption Limit Reached",
            Self::FileSize => "File Size Limit",
            Self::BioLength => "Bio Limit Reached",
            Self::Accounts => "Accounts Limit Reached",
        }
    }

    /// Reference addition string matching `LimitsPremiumRef` (`double_limits__<ref>`).
    #[must_use]
    pub const fn ref_addition(self) -> &'static str {
        match self {
            Self::Folders => "dialog_filters",
            Self::FolderChats => "dialog_filters_chats",
            Self::ShareableFolders => "chatlists_joined",
            Self::FolderLinks => "chatlist_invites",
            Self::PinnedChats => "dialog_pinned",
            Self::SavedPins => "saved_dialog_pinned",
            Self::Channels => "channels",
            Self::PublicLinks => "channels_public",
            Self::SavedGifs => "saved_gifs",
            Self::FavoriteStickers => "stickers_faved",
            Self::CaptionLength => "caption_length",
            Self::FileSize => "upload_max_fileparts",
            Self::BioLength => "about_length",
            Self::Accounts => "accounts",
        }
    }

    /// Returns `(default_limit, premium_limit, unit_suffix)` from [`PremiumLimitsData`].
    #[must_use]
    pub const fn bounds(self, limits: &PremiumLimitsData) -> (i32, i32, &'static str) {
        match self {
            Self::Folders => (limits.dialog_filters.0, limits.dialog_filters.1, ""),
            Self::FolderChats => (
                limits.dialog_filters_chats.0,
                limits.dialog_filters_chats.1,
                "",
            ),
            Self::ShareableFolders => (limits.shareable_filters.0, limits.shareable_filters.1, ""),
            Self::FolderLinks => (limits.filter_links.0, limits.filter_links.1, ""),
            Self::PinnedChats => (limits.dialogs_pinned.0, limits.dialogs_pinned.1, ""),
            Self::SavedPins => (
                limits.saved_sublists_pinned.0,
                limits.saved_sublists_pinned.1,
                "",
            ),
            Self::Channels => (limits.channels.0, limits.channels.1, ""),
            Self::PublicLinks => (limits.channels_public.0, limits.channels_public.1, ""),
            Self::SavedGifs => (limits.saved_gifs.0, limits.saved_gifs.1, ""),
            Self::FavoriteStickers => (limits.stickers_faved.0, limits.stickers_faved.1, ""),
            Self::CaptionLength => (limits.caption_length.0, limits.caption_length.1, " chars"),
            Self::FileSize => (limits.upload_max_gb.0, limits.upload_max_gb.1, " GB"),
            Self::BioLength => (limits.about_length.0, limits.about_length.1, " chars"),
            Self::Accounts => (limits.accounts.0, limits.accounts.1, ""),
        }
    }

    /// Formats the explanatory message shown inside the limit box.
    #[must_use]
    pub fn description(self, limits: &PremiumLimitsData, is_premium: bool) -> String {
        let (def, prem, unit) = self.bounds(limits);
        let active = if is_premium { prem } else { def };
        match self {
            Self::Folders => {
                if is_premium {
                    format!("You can create up to {prem} chat folders.")
                } else {
                    format!(
                        "You can create up to {def} chat folders. Subscribe to Telegram Premium to increase the limit to {prem} folders."
                    )
                }
            }
            Self::FolderChats => format!(
                "You can include up to {active} chats in one folder.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium raises this to 200 chats per folder."
                }
            ),
            Self::ShareableFolders => format!(
                "You can join up to {active} shareable folders.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium increases this limit to 20."
                }
            ),
            Self::FolderLinks => format!(
                "You can create up to {active} invite links per folder.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium unlocks up to 20 links per folder."
                }
            ),
            Self::PinnedChats => format!(
                "You can pin up to {active} chats in your main list.{}",
                if is_premium {
                    ""
                } else {
                    " Upgrade to Telegram Premium to pin up to 10 chats."
                }
            ),
            Self::SavedPins => format!(
                "You can pin up to {active} chats in Saved Messages.{}",
                if is_premium {
                    ""
                } else {
                    " Upgrade to Telegram Premium to pin up to 100 saved chats."
                }
            ),
            Self::Channels => format!(
                "You have reached the limit of {active} channels and supergroups. Leave inactive channels below{}",
                if is_premium {
                    "."
                } else {
                    " or subscribe to Telegram Premium to join up to 1,000."
                }
            ),
            Self::PublicLinks => format!(
                "You can own up to {active} public channels or groups. Revoke a public link below{}",
                if is_premium {
                    "."
                } else {
                    " or upgrade to Telegram Premium for up to 20 public links."
                }
            ),
            Self::SavedGifs => format!(
                "You can save up to {active} GIFs.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium doubles your saved GIFs limit to 400."
                }
            ),
            Self::FavoriteStickers => format!(
                "You can mark up to {active} stickers as favorites.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium doubles this limit to 10."
                }
            ),
            Self::CaptionLength => format!(
                "Media captions are limited to {active}{unit}.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium doubles captions to 2,048 characters."
                }
            ),
            Self::FileSize => format!(
                "Maximum upload size is {active}{unit} per file.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium lets you upload files up to 4 GB each."
                }
            ),
            Self::BioLength => format!(
                "Your profile bio can contain up to {active}{unit}.{}",
                if is_premium {
                    ""
                } else {
                    " Telegram Premium extends bios to 140 characters with links."
                }
            ),
            Self::Accounts => format!(
                "You can connect up to {active} accounts on this device.{}",
                if is_premium {
                    ""
                } else {
                    " Add Telegram Premium on one account to unlock a 4th account slot."
                }
            ),
        }
    }
}

/// Inactive channel entry shown inside `ChannelsLimitBox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InactiveChannelEntry {
    /// Synthetic channel ID.
    pub id: i64,
    /// Channel title.
    pub title: String,
    /// Whether this is a broadcast channel (`true`) or supergroup (`false`).
    pub is_broadcast: bool,
    /// Inactivity text (e.g. `"inactive for 7 months"`).
    pub inactive_since: String,
    /// Avatar tint RGB.
    pub avatar_rgb: [u8; 3],
}

/// Public channel entry shown inside `PublicLinksLimitBox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicLinkChannelEntry {
    /// Synthetic channel ID.
    pub id: i64,
    /// Channel title.
    pub title: String,
    /// Public username link (e.g. `"t.me/northwind_news"`).
    pub public_link: String,
    /// Avatar tint RGB.
    pub avatar_rgb: [u8; 3],
}

/// Subscription billing period option (`settings_premium.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumPlanOption {
    /// Duration in months (`12`, `6`, `1`).
    pub months: i32,
    /// Display tier label (e.g. `"Annual"`).
    pub label: String,
    /// Discount badge text (e.g. `"-33%"`, empty for monthly).
    pub discount_badge: String,
    /// Formatted monthly equivalent price (e.g. `"$2.99"`).
    pub price_per_month: String,
    /// Formatted total billed amount (e.g. `"$35.88"`).
    pub total_price: String,
    /// Struck-through undiscounted total (empty for monthly).
    pub original_total: String,
}

/// One Telegram Premium feature entry matching `EntryMap()` in `settings_premium.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumFeatureEntry {
    /// Feature key matching `FallbackOrder()` in `settings_premium.cpp`.
    pub key: String,
    /// Feature title.
    pub title: String,
    /// Short summary description.
    pub description: String,
    /// Static animation preview caption (describes the visual animation shown in tdesktop).
    pub preview_caption: String,
    /// Preview illustration tag (`"stories"`, `"upload"`, `"limits"`, `"badge"`, etc.).
    pub preview_kind: String,
    /// Short badge text inside the colored square icon.
    pub icon_glyph: &'static str,
    /// Icon tile color RGB.
    pub icon_rgb: [u8; 3],
    /// Whether the `NEW` pill is displayed (`ai_compose`, `rich_formatting`).
    pub is_new: bool,
}

/// One row in the Free vs Telegram Premium tier comparison table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumTierComparisonRow {
    /// Capability name.
    pub feature: String,
    /// Free tier value.
    pub free_value: String,
    /// Telegram Premium tier value.
    pub premium_value: String,
}

// -----------------------------------------------------------------------------
// Emoji Status & Premium Badge Picker (`data_emoji_statuses.h`)
// -----------------------------------------------------------------------------

/// Category tabs matching `Data::EmojiStatuses::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EmojiStatusCategory {
    /// Recently used statuses (`Type::Recent`).
    #[default]
    Recent,
    /// Default Telegram statuses (`Type::Default`).
    Default,
    /// Theme-colored adaptive statuses (`Type::Colored`).
    Colored,
    /// Collectible gift statuses (`Type::Collectibles`).
    Collectibles,
}

impl EmojiStatusCategory {
    /// All picker tabs in order.
    pub const ALL: &'static [Self] = &[
        Self::Recent,
        Self::Default,
        Self::Colored,
        Self::Collectibles,
    ];

    /// Display label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Default => "Default",
            Self::Colored => "Adaptive",
            Self::Collectibles => "Collectibles",
        }
    }
}

/// Duration preset when setting a temporary emoji status (`info_profile_emoji_status_panel.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmojiStatusDuration {
    /// Never expires (`until = 0`).
    #[default]
    Forever,
    /// 1 hour (`3600` seconds).
    OneHour,
    /// 2 hours (`7200` seconds).
    TwoHours,
    /// 8 hours (`28800` seconds).
    EightHours,
    /// 2 days (`172800` seconds).
    TwoDays,
}

impl EmojiStatusDuration {
    /// All presets in menu order.
    pub const ALL: &'static [Self] = &[
        Self::Forever,
        Self::OneHour,
        Self::TwoHours,
        Self::EightHours,
        Self::TwoDays,
    ];

    /// Display label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Forever => "Until I change it",
            Self::OneHour => "For 1 hour",
            Self::TwoHours => "For 2 hours",
            Self::EightHours => "For 8 hours",
            Self::TwoDays => "For 2 days",
        }
    }

    /// Duration in seconds (`0` for forever).
    #[must_use]
    pub const fn seconds(self) -> i32 {
        match self {
            Self::Forever => 0,
            Self::OneHour => 3_600,
            Self::TwoHours => 7_200,
            Self::EightHours => 28_800,
            Self::TwoDays => 172_800,
        }
    }
}

/// Selectable custom emoji status item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiStatusItem {
    /// Synthetic custom-emoji document ID.
    pub id: u64,
    /// Human-readable name (e.g. `"Focused"`, `"Coffee Break"`, `"Diamond Crown"`).
    pub name: String,
    /// Short monogram/glyph drawn inside the status badge.
    pub glyph: String,
    /// Category tab.
    pub category: EmojiStatusCategory,
    /// Primary badge color RGB.
    pub color_rgb: [u8; 3],
    /// Secondary tint RGB (for collectible backdrop).
    pub accent_rgb: [u8; 3],
    /// Whether this entry is a Collectible gift status (`EmojiStatusCollectible`).
    pub is_collectible: bool,
    /// Optional collectible slug (e.g. `"PrismCrown-104"`).
    pub collectible_slug: Option<String>,
}

/// Current active emoji status on the user's profile (`Data::EmojiStatusData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveEmojiStatus {
    /// Active status ID (`None` displays the default Telegram Premium star badge).
    pub status_id: Option<u64>,
    /// Optional duration preset applied.
    pub duration: EmojiStatusDuration,
}

/// Complete snapshot of Business & Premium state loaded into the host.
#[derive(Debug, Clone)]
pub struct BusinessOverviewSnapshot {
    /// Profile display name (synthetic).
    pub account_name: String,
    /// Account username without `@`.
    pub account_username: String,
    /// Synthetic phone placeholder string (never a real phone number).
    pub account_phone_display: String,
    /// Whether Telegram Premium is active in this session.
    pub is_premium: bool,
    /// Whether Sponsored Messages are hidden/disabled (Premium toggle in `settings_business.cpp`).
    pub sponsored_disabled: bool,
    /// Profile business details (working hours, location, chat intro).
    pub details: BusinessDetails,
    /// Available server timezones.
    pub timezones: Vec<Timezone>,
    /// Greeting message settings.
    pub greeting: GreetingSettings,
    /// Away message settings.
    pub away: AwaySettings,
    /// Connected chatbot settings.
    pub chatbots: ChatbotsSettings,
    /// All quick reply shortcuts keyed by ID.
    pub shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcut>,
    /// All business chat links.
    pub chat_links: Vec<BusinessChatLink>,
    /// Contacts/chats available in the recipients picker.
    pub available_users: Vec<BusinessUserRef>,
    /// Available Chat Intro greeting stickers.
    pub intro_stickers: Vec<IntroStickerOption>,
    /// Active emoji status.
    pub emoji_status: ActiveEmojiStatus,
    /// Available emoji status catalog.
    pub emoji_statuses: Vec<EmojiStatusItem>,
    /// Premium limits configuration.
    pub premium_limits: PremiumLimitsData,
    /// Premium features list in `FallbackOrder`.
    pub premium_features: Vec<PremiumFeatureEntry>,
    /// Premium subscription period options.
    pub premium_plans: Vec<PremiumPlanOption>,
    /// Free vs Premium comparison rows.
    pub tier_comparison: Vec<PremiumTierComparisonRow>,
    /// Inactive channels for `ChannelsLimitBox`.
    pub inactive_channels: Vec<InactiveChannelEntry>,
    /// Admined public channels for `PublicLinksLimitBox`.
    pub public_link_channels: Vec<PublicLinkChannelEntry>,
}
