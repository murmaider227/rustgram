//! Rustgram-specific strings for `calls` with no exact `resources/lang.strings` key.
//!
//! All standard Telegram Desktop call phrases live in `tg_lang::tr::*`.

/// Confirmation prompt shown when a 1:1 call awaits user confirmation before dialing.
pub const START_CALL_CONFIRM: &str = "Start a call?";

/// PiP label when sharing the local screen in a 1:1 call.
pub const SHARING_SCREEN: &str = "Sharing Screen";

/// PiP label when the local camera is active in a 1:1 call.
pub const YOUR_CAMERA: &str = "Your Camera";

/// HD video indicator suffix on the 1:1 incoming video backdrop.
pub const HD_VIDEO_SUFFIX: &str = "HD Video";

/// Header for the 1:1 call audio device selector popup.
pub const AUDIO_DEVICES: &str = "Audio Devices";

/// Header for the 1:1 call camera device selector popup.
pub const CAMERA_DEVICE: &str = "Camera Device";

/// Post-call low-rating submit button text.
pub const SEND_FEEDBACK: &str = "Send Feedback";

/// Wide-mode participants sidebar title in the group call window.
pub const PARTICIPANTS_HEADER: &str = "Participants";

/// Compact "Display As" pill label in the group call header bar.
pub const DISPLAY_AS_PILL: &str = "Display As";

/// Compact recording indicator badge in the group call header.
pub const REC_BADGE: &str = "REC";

/// Compact dock label for stopping screen share in a group call.
pub const STOP_SHARE_SHORT: &str = "Stop Share";

/// Compact dock label when actively speaking in a group call.
pub const SPEAKING_LABEL: &str = "Speaking";

/// Compact dock label when Push-to-Talk is actively transmitting.
pub const PUSH_TO_TALK_SHORT: &str = "Push-to-Talk";

/// Compact dock label when force-muted and able to raise hand.
pub const RAISE_HAND_LABEL: &str = "Raise Hand";

/// Compact dock label after raising hand in a group call.
pub const HAND_RAISED_LABEL: &str = "Hand Raised";

/// Compact dock label while reconnecting.
pub const CONNECTING_SHORT: &str = "Connecting";

/// Suffix appended to the current user's name in the participants list.
pub const SELF_BADGE_SUFFIX: &str = "(You)";

/// Inline volume slider label prefix in expanded participant controls.
pub const VOLUME_PREFIX: &str = "Volume";

/// Section heading for admin permission checkboxes in group call settings.
pub const PERMISSIONS_HEADING: &str = "Permissions";

/// Section heading for audio and camera selectors in group call settings.
pub const AUDIO_VIDEO_DEVICES_HEADING: &str = "Audio & Video Devices";

/// Checkbox label to enable Push-to-Talk in group call settings.
pub const ENABLE_PTT_CHECKBOX: &str = "Enable Push-to-Talk";

/// Label before the Push-to-Talk shortcut capture button.
pub const SHORTCUT_LABEL: &str = "Shortcut:";

/// Prompt shown on the Push-to-Talk button while capturing a key chord.
pub const PRESS_SHORTCUT_HINT: &str = "Press shortcut... (Esc to cancel)";

/// Prompt shown when no Push-to-Talk shortcut is bound yet.
pub const CLICK_TO_SET_SHORTCUT: &str = "Click to set shortcut";

/// Section heading for invite link sharing in group call settings.
pub const INVITE_LINK_HEADING: &str = "Invite Link";

/// Section heading for the recent 1:1 calls list in the Calls box.
pub const RECENT_CALLS_HEADING: &str = "Recent Calls";

/// Subtitle for an active broadcast live stream in the Calls box.
pub const LIVE_STREAM_ACTIVE: &str = "Live stream active";

/// Subtitle for an active group voice chat in the Calls box.
pub const VOICE_CHAT_ACTIVE: &str = "Voice chat active";

/// Compact spotlight overlay button label to pin a video tile.
pub const PIN_LABEL: &str = "Pin";

/// Compact spotlight overlay button label to unpin a video tile.
pub const UNPIN_LABEL: &str = "Unpin";

/// Synthetic demo slide header for screen-share video tiles.
pub const SCREEN_TILE_DEMO_HEADER: &str = "Rustgram Call Architecture — Live Screen";

/// Suffix appended to a screen-share video tile owner name.
pub const SCREEN_SUFFIX: &str = "(Screen)";

/// Window title prefix for 1:1 call floating windows.
pub const CALL_WINDOW_PREFIX: &str = "Call";
