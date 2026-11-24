//! Group calls (Voice Chats / Video Chats / Live Streams / Conferences) protocol core.
//!
//! Port of `data/data_group_call.h/.cpp`, `calls/group/calls_group_common.h/.cpp`,
//! and `calls/group/calls_group_call.h/.cpp`.

pub mod e2e;
pub mod messages;
pub mod participant;
pub mod payload;
pub mod state;
pub mod video;

/// Default per-participant audio volume (`Calls::Group::kDefaultVolume = 10000`, 100%).
pub const DEFAULT_VOLUME: i32 = 10_000;

/// Maximum per-participant audio volume (`Calls::Group::kMaxVolume = 20000`, 200%).
pub const MAX_VOLUME: i32 = 20_000;

/// Minimum clamped non-zero volume sent to `phone.editGroupCallParticipant` (`calls_group_call.cpp:4032`).
pub const MIN_VOLUME: i32 = 1;

/// Volume sentinel sent in self-updates when `f_volume` is unset (`calls_group_call.cpp:3935`).
pub const SELF_UPDATE_DUMMY_VOLUME: i32 = 100_000;

/// Voice activity audio level threshold (`GroupCall::kSpeakLevelThreshold = 0.2`).
pub const SPEAK_LEVEL_THRESHOLD: f32 = 0.2;

/// Duration in ms that sounding/speaking state is kept after last audio level
/// (`Data::GroupCall::kSoundStatusKeptFor = 1500`).
pub const SOUND_STATUS_KEPT_FOR_MS: u64 = 1_500;

/// Base interval in ms for checking last-spoke expirations (`kCheckLastSpokeInterval = 1000`).
pub const CHECK_LAST_SPOKE_INTERVAL_MS: u64 = 1_000;

/// Duration in ms that entries remain in `_lastSpoke` (`kKeepInListFor = kCheckLastSpokeInterval * 2`).
pub const KEEP_LAST_SPOKE_FOR_MS: u64 = CHECK_LAST_SPOKE_INTERVAL_MS * 2;

/// Periodic `phone.checkGroupCall` interval while connecting (`kCheckJoinedTimeout = 4000`).
pub const CHECK_JOINED_TIMEOUT_MS: u64 = 4_000;

/// Minimum interval between speaking send-progress updates (`kUpdateSendActionEach = 500`).
pub const UPDATE_SEND_ACTION_EACH_MS: u64 = 500;

/// Duration in ms to hold a manually selected large video endpoint (`kFixManualLargeVideoDuration`).
pub const FIX_MANUAL_LARGE_VIDEO_MS: u64 = 5_000;

/// Duration in ms to hold a speaker-selected large video endpoint (`kFixSpeakingLargeVideoDuration`).
pub const FIX_SPEAKING_LARGE_VIDEO_MS: u64 = 3_000;

/// Duration in ms that `active_date` keeps a participant speaking when not joined (`kSpeakingAfterActive`).
pub const SPEAKING_AFTER_ACTIVE_MS: u64 = 6_000;

/// Shortened active-speaking tail after joining (`kActiveAfterJoined`).
pub const ACTIVE_AFTER_JOINED_MS: u64 = 1_000;

/// Timeout before reloading when out-of-order versioned updates sit in the queue (`kWaitForUpdatesTimeout`).
pub const WAIT_FOR_UPDATES_TIMEOUT_MS: u64 = 3_000;

/// Interval after which a group call metadata snapshot is considered stale (`kReloadStaleTimeout`).
pub const RELOAD_STALE_TIMEOUT_MS: u64 = 16_000;

/// Page size for `phone.getGroupParticipants` (`kRequestPerPage = 50`).
pub const REQUEST_PER_PAGE: i32 = 50;

/// Maximum users invited per `phone.inviteToGroupCall` call (`kMaxInvitePerSlice = 10`).
pub const MAX_INVITE_PER_SLICE: usize = 10;

/// Synthetic video endpoint ID for RTMP livestreams (`Data::RtmpEndpointId()`).
pub const RTMP_ENDPOINT_ID: &str = "unified";

/// Weight of one `Full` quality video stream in `Medium` units (`kFullAsMediumsCount = 4`).
pub const FULL_AS_MEDIUMS_COUNT: usize = 4;

/// Maximum total video quality budget in `Medium` units (`kMaxMediumQualities = 16`).
pub const MAX_MEDIUM_QUALITIES: usize = 16;

/// Extract a conference invite slug from a `t.me/+...` / `tg://call?slug=...` URL.
///
/// Port of `Calls::Group::ExtractConferenceSlug` (`calls_group_common.cpp:529-542`).
pub fn extract_conference_slug(link: &str) -> Option<String> {
    let before_hash = link.split('#').next()?;
    let before_amp = before_hash.split('&').next()?;
    if let Some((_, slug)) = before_amp.split_once("slug=")
        && !slug.is_empty()
    {
        return Some(slug.to_string());
    }
    None
}
