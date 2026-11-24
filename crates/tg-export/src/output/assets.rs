//! Static HTML export assets copied from `Telegram/Resources/export_html/` (GPL-3.0).
//!
//! Matches the asset list copied by `HtmlWriter::start` in `export/output/export_output_html.cpp:5717-5752`.

/// Static asset entry `(relative_path, bytes)` written into the HTML export output folder.
pub struct ExportHtmlAsset {
    /// Relative path inside the export directory (e.g. `"css/style.css"`).
    pub relative_path: &'static str,
    /// Raw file bytes.
    pub bytes: &'static [u8],
}

/// All static HTML assets copied by `HtmlWriter::start` (44 files: CSS, JS, and 1x/2x PNG icons).
pub const EXPORT_HTML_ASSETS: &[ExportHtmlAsset] = &[
    ExportHtmlAsset {
        relative_path: "css/style.css",
        bytes: include_bytes!("../../assets/export_html/css/style.css"),
    },
    ExportHtmlAsset {
        relative_path: "images/back.png",
        bytes: include_bytes!("../../assets/export_html/images/back.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/back@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/back@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_call.png",
        bytes: include_bytes!("../../assets/export_html/images/media_call.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_call@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_call@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_contact.png",
        bytes: include_bytes!("../../assets/export_html/images/media_contact.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_contact@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_contact@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_file.png",
        bytes: include_bytes!("../../assets/export_html/images/media_file.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_file@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_file@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_game.png",
        bytes: include_bytes!("../../assets/export_html/images/media_game.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_game@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_game@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_location.png",
        bytes: include_bytes!("../../assets/export_html/images/media_location.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_location@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_location@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_music.png",
        bytes: include_bytes!("../../assets/export_html/images/media_music.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_music@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_music@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_photo.png",
        bytes: include_bytes!("../../assets/export_html/images/media_photo.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_photo@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_photo@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_shop.png",
        bytes: include_bytes!("../../assets/export_html/images/media_shop.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_shop@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_shop@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_video.png",
        bytes: include_bytes!("../../assets/export_html/images/media_video.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_video@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_video@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_voice.png",
        bytes: include_bytes!("../../assets/export_html/images/media_voice.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/media_voice@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/media_voice@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_calls.png",
        bytes: include_bytes!("../../assets/export_html/images/section_calls.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_calls@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_calls@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_chats.png",
        bytes: include_bytes!("../../assets/export_html/images/section_chats.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_chats@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_chats@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_contacts.png",
        bytes: include_bytes!("../../assets/export_html/images/section_contacts.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_contacts@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_contacts@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_frequent.png",
        bytes: include_bytes!("../../assets/export_html/images/section_frequent.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_frequent@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_frequent@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_music.png",
        bytes: include_bytes!("../../assets/export_html/images/section_music.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_music@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_music@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_other.png",
        bytes: include_bytes!("../../assets/export_html/images/section_other.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_other@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_other@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_photos.png",
        bytes: include_bytes!("../../assets/export_html/images/section_photos.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_photos@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_photos@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_sessions.png",
        bytes: include_bytes!("../../assets/export_html/images/section_sessions.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_sessions@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_sessions@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_stories.png",
        bytes: include_bytes!("../../assets/export_html/images/section_stories.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_stories@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_stories@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_web.png",
        bytes: include_bytes!("../../assets/export_html/images/section_web.png"),
    },
    ExportHtmlAsset {
        relative_path: "images/section_web@2x.png",
        bytes: include_bytes!("../../assets/export_html/images/section_web@2x.png"),
    },
    ExportHtmlAsset {
        relative_path: "js/script.js",
        bytes: include_bytes!("../../assets/export_html/js/script.js"),
    },
];
