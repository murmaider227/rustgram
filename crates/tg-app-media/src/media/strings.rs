//! Rustgram-specific strings for the media viewer and audio player that have no exact key in
//! `resources/lang.strings`.

/// Context menu label for rotating an image/video 90 degrees clockwise.
pub const ROTATE_CW_90: &str = "Rotate 90° Clockwise";

/// Toast shown after launching a media item in an external application.
pub const OPENED_IN_EXTERNAL_APP: &str = "Opened in external app";

/// Toast prefix shown when saving a media item to disk fails.
pub const SAVE_FAILED_PREFIX: &str = "Save failed";

/// Toast shown after copying an image or video frame to the clipboard.
pub const COPIED_TO_CLIPBOARD: &str = "Copied to clipboard";

/// Toast shown after forwarding a media item.
pub const FORWARDED_MEDIA: &str = "Forwarded media";

/// Fallback title in the audio player bar when track metadata is absent.
pub const AUDIO_TRACK_FALLBACK: &str = "Audio Track";

/// Caption shown in the standalone screenshot harness for the combined bar/PiP/float showcase.
pub const CAPTURE_STAGE_TITLE: &str =
    "Top Audio Player Bar + Picture-in-Picture + Floating Round Video";

/// Context menu / toolbar label for running Windows OCR text recognition on the active photo.
pub const RECOGNIZE_TEXT_LABEL: &str = "Recognize Text";

/// Button label to copy recognized / selected OCR text to the clipboard.
pub const COPY_RECOGNIZED_TEXT_LABEL: &str = "Copy Text";

/// Button label to select all recognized OCR text on the active photo.
pub const SELECT_ALL_OCR_TEXT_LABEL: &str = "Select All";

/// Toast shown when OCR finds no readable text in the active photo.
pub const OCR_NO_TEXT_FOUND: &str = "No text recognized in this image";

/// Toast shown when Windows OCR is unavailable on the current OS/language pack.
pub const OCR_UNAVAILABLE: &str = "Text recognition is unavailable on this system";

/// Toast shown after copying recognized OCR text to the clipboard.
pub const OCR_TEXT_COPIED_TOAST: &str = "Text copied to clipboard";
