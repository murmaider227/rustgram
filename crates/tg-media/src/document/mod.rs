//! Document attributes helpers: MIME type tables, extension classification, magic-byte sniffing,
//! and safe filename sanitization/generation.
//!
//! Port of `Telegram/SourceFiles/core/mime_type.cpp`,
//! `Telegram/lib_base/base/base_file_utilities.cpp`,
//! `Telegram/lib_base/base/platform/win/base_file_utilities_win.cpp`,
//! `Telegram/SourceFiles/data/data_document.cpp` (`FileNameForSave`, `DocumentFileNameForSave`),
//! and `Telegram/SourceFiles/core/file_utilities.cpp` (`filedialogDefaultName`, `filedialogNextFilename`).

use std::path::{Path, PathBuf};

/// File type category derived from file extension (`Core::NameType` in `core/mime_type.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameType {
    /// Image file (`.jpg`, `.png`, `.webp`, `.gif`, ...).
    Image,
    /// Video or animated sticker file (`.mp4`, `.mov`, `.webm`, `.tgs`, ...).
    Video,
    /// Audio file (`.mp3`, `.ogg`, `.opus`, `.flac`, ...).
    Audio,
    /// Document / ebook / office / HTML file (`.pdf`, `.docx`, `.txt`, ...).
    Document,
    /// Archive file (`.zip`, `.7z`, `.tar`, `.rar`, ...).
    Archive,
    /// Telegram theme or palette file (`.tdesktop-theme`, `.tdesktop-palette`, ...).
    ThemeFile,
    /// Other benign developer/system file extensions.
    OtherBenign,
    /// Potentially executable / dangerous file on the target OS.
    Executable,
    /// Unrecognized extension.
    Unknown,
}

/// Resolved MIME metadata with canonical name, default extension, and file-dialog filter string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimeTypeInfo {
    /// Canonical MIME type (e.g., `"image/webp"`).
    pub name: &'static str,
    /// Glob patterns (e.g., `&["*.webp"]`).
    pub glob_patterns: &'static [&'static str],
    /// Human-readable dialog filter (e.g., `"WebP image (*.webp)"`).
    pub filter_string: &'static str,
}

impl MimeTypeInfo {
    /// Returns the first dotted extension (e.g. `".webp"`), if any.
    #[must_use]
    pub fn primary_extension(&self) -> Option<&'static str> {
        self.glob_patterns
            .first()
            .and_then(|pat| pat.strip_prefix('*'))
    }
}

/// Comprehensive MIME-to-extension table covering all tdesktop custom types (`core/mime_type.cpp`)
/// and standard Telegram media/document types.
const MIME_TABLE: &[MimeTypeInfo] = &[
    // Custom Telegram types from core/mime_type.cpp:70-121
    MimeTypeInfo {
        name: "image/webp",
        glob_patterns: &["*.webp"],
        filter_string: "WebP image (*.webp)",
    },
    MimeTypeInfo {
        name: "application/x-tgsticker",
        glob_patterns: &["*.tgs"],
        filter_string: "Telegram sticker (*.tgs)",
    },
    MimeTypeInfo {
        name: "application/x-tgwallpattern",
        glob_patterns: &["*.tgv"],
        filter_string: "Wallpaper pattern (*.tgv)",
    },
    MimeTypeInfo {
        name: "application/x-tdesktop-theme",
        glob_patterns: &["*.tdesktop-theme"],
        filter_string: "Theme files (*.tdesktop-theme)",
    },
    MimeTypeInfo {
        name: "application/x-tdesktop-palette",
        glob_patterns: &["*.tdesktop-palette"],
        filter_string: "Palette files (*.tdesktop-palette)",
    },
    // Images
    MimeTypeInfo {
        name: "image/jpeg",
        glob_patterns: &["*.jpg", "*.jpeg", "*.jfif"],
        filter_string: "JPEG image (*.jpg *.jpeg)",
    },
    MimeTypeInfo {
        name: "image/png",
        glob_patterns: &["*.png"],
        filter_string: "PNG image (*.png)",
    },
    MimeTypeInfo {
        name: "image/gif",
        glob_patterns: &["*.gif"],
        filter_string: "GIF image (*.gif)",
    },
    MimeTypeInfo {
        name: "image/bmp",
        glob_patterns: &["*.bmp"],
        filter_string: "BMP image (*.bmp)",
    },
    MimeTypeInfo {
        name: "image/tiff",
        glob_patterns: &["*.tiff", "*.tif"],
        filter_string: "TIFF image (*.tiff *.tif)",
    },
    MimeTypeInfo {
        name: "image/svg+xml",
        glob_patterns: &["*.svg", "*.svgz"],
        filter_string: "SVG image (*.svg)",
    },
    MimeTypeInfo {
        name: "image/avif",
        glob_patterns: &["*.avif"],
        filter_string: "AVIF image (*.avif)",
    },
    MimeTypeInfo {
        name: "image/heic",
        glob_patterns: &["*.heic"],
        filter_string: "HEIC image (*.heic)",
    },
    MimeTypeInfo {
        name: "image/x-icon",
        glob_patterns: &["*.ico"],
        filter_string: "ICO image (*.ico)",
    },
    // Video
    MimeTypeInfo {
        name: "video/mp4",
        glob_patterns: &["*.mp4", "*.m4v"],
        filter_string: "MP4 video (*.mp4)",
    },
    MimeTypeInfo {
        name: "video/quicktime",
        glob_patterns: &["*.mov"],
        filter_string: "MOV video (*.mov)",
    },
    MimeTypeInfo {
        name: "video/webm",
        glob_patterns: &["*.webm"],
        filter_string: "WebM video (*.webm)",
    },
    MimeTypeInfo {
        name: "video/x-matroska",
        glob_patterns: &["*.mkv"],
        filter_string: "Matroska video (*.mkv)",
    },
    MimeTypeInfo {
        name: "video/x-msvideo",
        glob_patterns: &["*.avi"],
        filter_string: "AVI video (*.avi)",
    },
    MimeTypeInfo {
        name: "video/mpeg",
        glob_patterns: &["*.mpeg", "*.mpg"],
        filter_string: "MPEG video (*.mpeg *.mpg)",
    },
    MimeTypeInfo {
        name: "video/ogg",
        glob_patterns: &["*.ogv"],
        filter_string: "Ogg video (*.ogv)",
    },
    MimeTypeInfo {
        name: "video/3gpp",
        glob_patterns: &["*.3gp", "*.3gpp"],
        filter_string: "3GP video (*.3gp)",
    },
    MimeTypeInfo {
        name: "video/x-flv",
        glob_patterns: &["*.flv"],
        filter_string: "Flash video (*.flv)",
    },
    MimeTypeInfo {
        name: "video/x-ms-wmv",
        glob_patterns: &["*.wmv"],
        filter_string: "WMV video (*.wmv)",
    },
    // Audio
    MimeTypeInfo {
        name: "audio/ogg",
        glob_patterns: &["*.ogg", "*.opus"],
        filter_string: "OGG Opus audio (*.ogg)",
    },
    MimeTypeInfo {
        name: "audio/opus",
        glob_patterns: &["*.opus", "*.ogg"],
        filter_string: "Opus audio (*.opus)",
    },
    MimeTypeInfo {
        name: "audio/mpeg",
        glob_patterns: &["*.mp3"],
        filter_string: "MP3 audio (*.mp3)",
    },
    MimeTypeInfo {
        name: "audio/mp3",
        glob_patterns: &["*.mp3"],
        filter_string: "MP3 audio (*.mp3)",
    },
    MimeTypeInfo {
        name: "audio/mp4",
        glob_patterns: &["*.m4a", "*.m4b"],
        filter_string: "M4A audio (*.m4a)",
    },
    MimeTypeInfo {
        name: "audio/aac",
        glob_patterns: &["*.aac"],
        filter_string: "AAC audio (*.aac)",
    },
    MimeTypeInfo {
        name: "audio/flac",
        glob_patterns: &["*.flac"],
        filter_string: "FLAC audio (*.flac)",
    },
    MimeTypeInfo {
        name: "audio/wav",
        glob_patterns: &["*.wav"],
        filter_string: "WAV audio (*.wav)",
    },
    MimeTypeInfo {
        name: "audio/x-wav",
        glob_patterns: &["*.wav"],
        filter_string: "WAV audio (*.wav)",
    },
    MimeTypeInfo {
        name: "audio/midi",
        glob_patterns: &["*.mid", "*.midi"],
        filter_string: "MIDI audio (*.mid *.midi)",
    },
    // Documents & Archives
    MimeTypeInfo {
        name: "application/pdf",
        glob_patterns: &["*.pdf"],
        filter_string: "PDF document (*.pdf)",
    },
    MimeTypeInfo {
        name: "text/plain",
        glob_patterns: &["*.txt"],
        filter_string: "Text file (*.txt)",
    },
    MimeTypeInfo {
        name: "text/html",
        glob_patterns: &["*.html", "*.htm"],
        filter_string: "HTML document (*.html *.htm)",
    },
    MimeTypeInfo {
        name: "text/csv",
        glob_patterns: &["*.csv"],
        filter_string: "CSV document (*.csv)",
    },
    MimeTypeInfo {
        name: "application/json",
        glob_patterns: &["*.json"],
        filter_string: "JSON file (*.json)",
    },
    MimeTypeInfo {
        name: "application/xml",
        glob_patterns: &["*.xml"],
        filter_string: "XML document (*.xml)",
    },
    MimeTypeInfo {
        name: "application/zip",
        glob_patterns: &["*.zip"],
        filter_string: "ZIP archive (*.zip)",
    },
    MimeTypeInfo {
        name: "application/gzip",
        glob_patterns: &["*.gz"],
        filter_string: "Gzip archive (*.gz)",
    },
    MimeTypeInfo {
        name: "application/x-tar",
        glob_patterns: &["*.tar"],
        filter_string: "TAR archive (*.tar)",
    },
    MimeTypeInfo {
        name: "application/x-7z-compressed",
        glob_patterns: &["*.7z"],
        filter_string: "7-Zip archive (*.7z)",
    },
    MimeTypeInfo {
        name: "application/vnd.rar",
        glob_patterns: &["*.rar"],
        filter_string: "RAR archive (*.rar)",
    },
    MimeTypeInfo {
        name: "application/x-rar-compressed",
        glob_patterns: &["*.rar"],
        filter_string: "RAR archive (*.rar)",
    },
    MimeTypeInfo {
        name: "application/msword",
        glob_patterns: &["*.doc"],
        filter_string: "Word document (*.doc)",
    },
    MimeTypeInfo {
        name: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        glob_patterns: &["*.docx"],
        filter_string: "Word document (*.docx)",
    },
    MimeTypeInfo {
        name: "application/vnd.ms-excel",
        glob_patterns: &["*.xls"],
        filter_string: "Excel spreadsheet (*.xls)",
    },
    MimeTypeInfo {
        name: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        glob_patterns: &["*.xlsx"],
        filter_string: "Excel spreadsheet (*.xlsx)",
    },
    MimeTypeInfo {
        name: "application/vnd.ms-powerpoint",
        glob_patterns: &["*.ppt"],
        filter_string: "PowerPoint presentation (*.ppt)",
    },
    MimeTypeInfo {
        name: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        glob_patterns: &["*.pptx"],
        filter_string: "PowerPoint presentation (*.pptx)",
    },
    MimeTypeInfo {
        name: "application/epub+zip",
        glob_patterns: &["*.epub"],
        filter_string: "EPUB e-book (*.epub)",
    },
    MimeTypeInfo {
        name: "application/vnd.android.package-archive",
        glob_patterns: &["*.apk"],
        filter_string: "Android package (*.apk)",
    },
    MimeTypeInfo {
        name: "application/octet-stream",
        glob_patterns: &["*.bin"],
        filter_string: "Binary file (*.bin)",
    },
];

/// Looks up MIME metadata by MIME type name (`Core::MimeTypeForName` in `core/mime_type.cpp:106`).
///
/// Handles tdesktop aliases such as `application/x-tgtheme-tdesktop` -> `application/x-tdesktop-theme`
/// and `audio/mpeg3` -> `audio/mp3`.
#[must_use]
pub fn mime_type_for_name(mime: &str) -> Option<&'static MimeTypeInfo> {
    let lower = mime.trim().to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "application/x-tgtheme-tdesktop" => "application/x-tdesktop-theme",
        "audio/mpeg3" => "audio/mp3",
        "image/jpg" => "image/jpeg",
        "audio/x-m4a" => "audio/mp4",
        other => other,
    };
    MIME_TABLE.iter().find(|entry| entry.name == canonical)
}

/// Returns the default dotted extension (e.g. `".mp4"`) for a MIME type, if known.
#[must_use]
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    mime_type_for_name(mime).and_then(MimeTypeInfo::primary_extension)
}

/// Looks up MIME metadata from a filename or extension (`Core::MimeTypeForFile` extension check).
#[must_use]
pub fn mime_type_for_filename(filepath: &str) -> Option<&'static MimeTypeInfo> {
    let ext = file_extension(filepath);
    if ext.is_empty() {
        return None;
    }
    let ext_lower = ext.to_ascii_lowercase();
    let needle = format!("*.{ext_lower}");
    MIME_TABLE
        .iter()
        .find(|entry| entry.glob_patterns.iter().any(|&p| p == needle))
}

/// Detects MIME metadata from leading magic bytes (`Core::MimeTypeForData`).
#[must_use]
pub fn mime_type_for_data(data: &[u8]) -> Option<&'static MimeTypeInfo> {
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return mime_type_for_name("image/webp");
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return mime_type_for_name("image/jpeg");
    }
    if data.len() >= 8 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        return mime_type_for_name("image/png");
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return mime_type_for_name("image/gif");
    }
    if data.len() >= 4 && &data[0..4] == b"OggS" {
        return mime_type_for_name("audio/ogg");
    }
    if data.len() >= 4 && &data[0..4] == b"fLaC" {
        return mime_type_for_name("audio/flac");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return mime_type_for_name("audio/wav");
    }
    if data.len() >= 4 && &data[0..4] == b"%PDF" {
        return mime_type_for_name("application/pdf");
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        if &data[8..12] == b"qt  " {
            return mime_type_for_name("video/quicktime");
        }
        return mime_type_for_name("video/mp4");
    }
    if data.len() >= 4 && data[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return mime_type_for_name("video/webm");
    }
    if data.len() >= 3 && (&data[0..3] == b"ID3" || (data[0] == 0xFF && (data[1] & 0xE0) == 0xE0)) {
        return mime_type_for_name("audio/mpeg");
    }
    if data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
        return mime_type_for_name("application/gzip");
    }
    if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
        return mime_type_for_name("application/zip");
    }
    None
}

/// Returns `true` if `mime` is Telegram's animated Lottie sticker MIME (`Core::IsMimeStickerAnimated`).
#[must_use]
pub fn is_mime_sticker_animated(mime: &str) -> bool {
    mime == "application/x-tgsticker"
}

/// Returns `true` if `mime` is a static WebP or animated TGS sticker (`Core::IsMimeSticker`).
#[must_use]
pub fn is_mime_sticker(mime: &str) -> bool {
    mime == "image/webp" || is_mime_sticker_animated(mime)
}

/// Returns `true` if `mime` is sent as a video (`Core::IsMimeSentAsVideo`).
#[must_use]
pub fn is_mime_sent_as_video(mime: &str) -> bool {
    mime == "video/mp4" || mime == "video/quicktime"
}

/// Returns `true` if `mime` is accepted in photo/video albums (`Core::IsMimeAcceptedForPhotoVideoAlbum`).
#[must_use]
pub fn is_mime_accepted_for_photo_video_album(mime: &str) -> bool {
    mime == "image/jpeg" || mime == "image/png" || is_mime_sent_as_video(mime)
}

/// Returns `true` if the file should be treated as an image (`Core::FileIsImage`).
#[must_use]
pub fn file_is_image(name: &str, mime: &str) -> bool {
    if name.is_empty() {
        mime.to_ascii_lowercase().starts_with("image/")
    } else {
        detect_name_type(name) == NameType::Image
    }
}

/// Extracts the file extension (without leading dot) after the last path separator,
/// mirroring `Core::FileExtension` (`core/mime_type.cpp:256`).
#[must_use]
pub fn file_extension(filepath: &str) -> &str {
    let bytes = filepath.as_bytes();
    for (idx, &b) in bytes.iter().enumerate().rev() {
        if b == b'.' {
            return &filepath[idx + 1..];
        }
        if b == b'/' || b == b'\\' {
            return "";
        }
    }
    ""
}

// Exact extension sets ported from `Telegram/SourceFiles/core/mime_type.cpp:265-317`.
const IMAGE_EXTS: &[&str] = &[
    "afdesign",
    "ai",
    "avif",
    "bmp",
    "dng",
    "gif",
    "heic",
    "icns",
    "ico",
    "jfif",
    "jpeg",
    "jpg",
    "jpg-large",
    "jxl",
    "nef",
    "png",
    "png-large",
    "psd",
    "qoi",
    "raw",
    "sketch",
    "svg",
    "tga",
    "tif",
    "tiff",
    "webp",
];

const VIDEO_EXTS: &[&str] = &[
    "3g2", "3gp", "3gpp", "aep", "avi", "flv", "h264", "m4s", "m4v", "mkv", "mov", "mp4", "mpeg",
    "mpg", "ogv", "srt", "tgs", "tgv", "vob", "webm", "wmv",
];

const AUDIO_EXTS: &[&str] = &[
    "aac", "ac3", "aif", "amr", "caf", "cda", "cue", "flac", "m4a", "m4b", "mid", "midi", "mp3",
    "ogg", "opus", "wav", "wma",
];

const DOCUMENT_EXTS: &[&str] = &[
    "cbr", "cbz", "csv", "diag", "djvu", "doc", "docx", "epub", "fb2", "htm", "html", "key", "kml",
    "log", "m3u", "m3u8", "mobi", "odp", "ods", "odt", "ost", "p7b", "pdf", "pem", "pps", "ppsx",
    "ppt", "pptx", "prc", "ps", "pub", "ris", "rtf", "tex", "text", "tl", "txt", "wpd", "wpl",
    "xhtml", "xls", "xlsx", "xml", "xspf",
];

const ARCHIVE_EXTS: &[&str] = &[
    "7z", "arj", "bz2", "gz", "rar", "tar", "xz", "z", "zip", "zst",
];

const THEME_EXTS: &[&str] = &[
    "attheme",
    "tdesktop-palette",
    "tdesktop-theme",
    "tgios-theme",
];

const OTHER_BENIGN_EXTS: &[&str] = &[
    "asoundrc",
    "bak",
    "c",
    "cc",
    "cmake",
    "core",
    "cpp",
    "cs",
    "csr",
    "css",
    "cur",
    "cxx",
    "dat",
    "db",
    "diff",
    "dlc",
    "dmg",
    "dmp",
    "eot",
    "file",
    "fnt",
    "fon",
    "gbr",
    "gitignore",
    "gpx",
    "gyp",
    "h",
    "ics",
    "img",
    "ipa",
    "ips",
    "iso",
    "java",
    "json",
    "keylayout",
    "m",
    "md",
    "mem",
    "mk",
    "mm",
    "ninja",
    "otf",
    "ovpn",
    "part",
    "patch",
    "pcap",
    "pcapng",
    "pdb",
    "pkpass",
    "plain",
    "plist",
    "rss",
    "sln",
    "sql",
    "strings",
    "swift",
    "tdesktop-endpoints",
    "torrent",
    "ts",
    "ttc",
    "ttf",
    "vcd",
    "xaml",
    "xib",
    "zxp",
    "class",
];

/// Windows executable extension list from `core/mime_type.cpp:296-308`.
const WINDOWS_EXECUTABLE_EXTS: &[&str] = &[
    "ad",
    "ade",
    "adp",
    "ahk",
    "app",
    "application",
    "appref-ms",
    "asp",
    "aspx",
    "asx",
    "bas",
    "bat",
    "bin",
    "cab",
    "cdxml",
    "cer",
    "cfg",
    "cgi",
    "chi",
    "chm",
    "cmd",
    "cnt",
    "com",
    "conf",
    "cpl",
    "crt",
    "csh",
    "der",
    "diagcab",
    "dll",
    "drv",
    "eml",
    "exe",
    "fon",
    "fxp",
    "gadget",
    "grp",
    "hlp",
    "hpj",
    "hta",
    "htt",
    "inf",
    "ini",
    "ins",
    "inx",
    "isp",
    "isu",
    "its",
    "jar",
    "jnlp",
    "job",
    "js",
    "jse",
    "jsp",
    "key",
    "ksh",
    "lexe",
    "library-ms",
    "lnk",
    "local",
    "lua",
    "mad",
    "maf",
    "mag",
    "mam",
    "manifest",
    "maq",
    "mar",
    "mas",
    "mat",
    "mau",
    "mav",
    "maw",
    "mcf",
    "mda",
    "mdb",
    "mde",
    "mdt",
    "mdw",
    "mdz",
    "mht",
    "mhtml",
    "mjs",
    "mmc",
    "mof",
    "msc",
    "msg",
    "msh",
    "msh1",
    "msh1xml",
    "msh2",
    "msh2xml",
    "mshxml",
    "msi",
    "msp",
    "mst",
    "ops",
    "osd",
    "paf",
    "pcd",
    "phar",
    "php",
    "php-s",
    "php3",
    "php4",
    "php5",
    "php7",
    "phps",
    "pht",
    "phtml",
    "pif",
    "pl",
    "plg",
    "pm",
    "pod",
    "prf",
    "prg",
    "ps1",
    "ps1xml",
    "ps2",
    "ps2xml",
    "psc1",
    "psc2",
    "psd1",
    "psm1",
    "pssc",
    "pst",
    "py",
    "py3",
    "pyc",
    "pyd",
    "pyi",
    "pyo",
    "pyw",
    "pyz",
    "pyzw",
    "rb",
    "reg",
    "rgs",
    "scf",
    "scr",
    "sct",
    "search-ms",
    "settingcontent-ms",
    "sh",
    "shb",
    "shs",
    "slk",
    "swf",
    "sys",
    "t",
    "tmp",
    "u3p",
    "url",
    "vb",
    "vbe",
    "vbp",
    "vbs",
    "vbscript",
    "vdx",
    "vsd",
    "vsdm",
    "vsdx",
    "vsmacros",
    "vss",
    "vssm",
    "vssx",
    "vst",
    "vstm",
    "vstx",
    "vsw",
    "vsx",
    "vtx",
    "website",
    "wlua",
    "ws",
    "wsc",
    "wsf",
    "wsh",
    "xbap",
    "xll",
    "xlsb",
    "xlsm",
    "xnk",
    "xs",
];

/// Extensions that can reveal the user's IP address when opened externally (`core/mime_type.cpp:349`).
const IP_REVEALING_EXTS: &[&str] = &[
    "htm", "html", "kml", "kmz", "m3u", "m3u8", "m4v", "svg", "wpl", "xhtml", "xml", "xspf",
];

/// Classifies a filename or path by its extension (`Core::DetectNameType` in `core/mime_type.cpp:265`).
///
/// Uses Windows executable rules (matching our primary target platform).
#[must_use]
pub fn detect_name_type(filepath: &str) -> NameType {
    let ext = file_extension(filepath).to_ascii_lowercase();
    let s = ext.as_str();
    if WINDOWS_EXECUTABLE_EXTS.contains(&s) {
        NameType::Executable
    } else if IMAGE_EXTS.contains(&s) {
        NameType::Image
    } else if VIDEO_EXTS.contains(&s) {
        NameType::Video
    } else if AUDIO_EXTS.contains(&s) {
        NameType::Audio
    } else if DOCUMENT_EXTS.contains(&s) {
        NameType::Document
    } else if ARCHIVE_EXTS.contains(&s) {
        NameType::Archive
    } else if THEME_EXTS.contains(&s) {
        NameType::ThemeFile
    } else if OTHER_BENIGN_EXTS.contains(&s) {
        NameType::OtherBenign
    } else {
        NameType::Unknown
    }
}

/// Returns `true` if `name_type` permits generating/showing a thumbnail (`Core::NameTypeAllowsThumbnail`).
#[must_use]
pub const fn name_type_allows_thumbnail(name_type: NameType) -> bool {
    matches!(
        name_type,
        NameType::Image
            | NameType::Video
            | NameType::Audio
            | NameType::Document
            | NameType::ThemeFile
    )
}

/// Returns `true` if opening a file with this extension externally may leak the user's IP
/// (`Core::DetectLaunchType` IP-revealing check in `core/mime_type.cpp:349`).
#[must_use]
pub fn is_ip_revealing_filename(filepath: &str) -> bool {
    let ext = file_extension(filepath).to_ascii_lowercase();
    IP_REVEALING_EXTS.contains(&ext.as_str())
}

// ============================================================================
// Filename sanitization & save-path resolution (FileNameFromUserString / FileNameForSave)
// ============================================================================

/// Returns `true` if `ch` is a Unicode BiDi control character that can spoof file extensions
/// (`IsBidiControl` in `base_file_utilities.cpp:16`).
#[must_use]
pub const fn is_bidi_control(ch: char) -> bool {
    let code = ch as u32;
    code == 0x061C
        || code == 0x200E
        || code == 0x200F
        || (code >= 0x202A && code <= 0x202E)
        || (code >= 0x2066 && code <= 0x2069)
}

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Sanitizes an untrusted filename for safe saving on Windows (and cross-platform).
///
/// Exact port of `base::FileNameFromUserString` (`base_file_utilities.cpp:25`) +
/// `base::Platform::FileNameFromUserString` (`base_file_utilities_win.cpp:76`):
/// - Replaces control chars (`< 0x20`), BiDi override/isolate marks, and `/ \ < > : " | ? *` with `'_'`.
/// - Appends `'_'` if the name is empty or ends with `' '` or `'.'`.
/// - Masks `.lnk` and `.scf` extensions by appending `.download` (case-insensitive).
/// - Prefixes Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1..9`, `LPT1..9`,
///   either bare or followed by `'.'`) with `'_'`.
#[must_use]
pub fn sanitize_filename(raw: &str) -> String {
    let mut name = String::with_capacity(raw.len() + 16);
    for ch in raw.chars() {
        let bad = (ch as u32) < 32
            || is_bidi_control(ch)
            || matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*');
        name.push(if bad { '_' } else { ch });
    }
    if name.is_empty() || name.ends_with(' ') || name.ends_with('.') {
        name.push('_');
    }

    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".scf") {
        name.push_str(".download");
    }

    let upper = name.to_ascii_uppercase();
    for &bad in WINDOWS_RESERVED_NAMES {
        if upper.starts_with(bad) {
            let next_byte = upper.as_bytes().get(bad.len()).copied();
            if next_byte.is_none() || next_byte == Some(b'.') {
                name.insert(0, '_');
                break;
            }
        }
    }

    name
}

/// Masks `.lnk` / `.scf` suffixes on an already-resolved path (`FileNameForSave` Windows guard).
#[must_use]
pub fn mask_dangerous_windows_extension(path: &str) -> String {
    let lower = path.trim().to_ascii_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".scf") {
        format!("{path}.download")
    } else {
        path.to_string()
    }
}

/// Document kind hint for [`choose_save_filename`], mirroring `DocumentFileNameForSave`
/// (`data/data_document.cpp:243`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// Voice message (`prefix = "audio"`, default `.ogg` or `.mp3`).
    Voice,
    /// Video file (`prefix = "video"`, default `.mov` or MIME extension).
    Video,
    /// Audio music file (`prefix = "doc"`).
    Audio,
    /// General document (`prefix = "doc"`, default `.unknown` or MIME extension).
    File,
}

/// Computes the sanitized filename (and prefix if the name starts with a dot) for saving a document,
/// mirroring `DocumentFileNameForSave` (`data/data_document.cpp:243-303`).
#[must_use]
pub fn document_save_name_and_prefix(
    kind: DocumentKind,
    original_filename: Option<&str>,
    mime: &str,
) -> (String, &'static str) {
    let ext_from_mime = extension_for_mime(mime);
    let (raw_name, prefix) = match kind {
        DocumentKind::Voice => {
            let is_mp3 = mime.eq_ignore_ascii_case("audio/mp3");
            let name = original_filename
                .filter(|s| !s.is_empty())
                .unwrap_or(if is_mp3 { ".mp3" } else { ".ogg" });
            (name.to_string(), "audio")
        }
        DocumentKind::Video => {
            let name = original_filename
                .filter(|s| !s.is_empty())
                .or(ext_from_mime)
                .unwrap_or(".mov");
            (name.to_string(), "video")
        }
        DocumentKind::Audio | DocumentKind::File => {
            let name = original_filename
                .filter(|s| !s.is_empty())
                .or(ext_from_mime)
                .unwrap_or(".unknown");
            (name.to_string(), "doc")
        }
    };
    (sanitize_filename(&raw_name), prefix)
}

/// Calendar date/time components for [`format_default_filename`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDateTime {
    /// Four-digit year (e.g., 2026).
    pub year: i32,
    /// Month in `1..=12`.
    pub month: u32,
    /// Day of month in `1..=31`.
    pub day: u32,
    /// Hour in `0..=23`.
    pub hour: u32,
    /// Minute in `0..=59`.
    pub minute: u32,
    /// Second in `0..=59`.
    pub second: u32,
}

/// Formats a timestamped default filename (`prefix_YYYY-MM-DD_HH-MM-SS.ext`),
/// mirroring `filedialogDefaultName` (`core/file_utilities.cpp:70`).
#[must_use]
pub fn format_default_filename(prefix: &str, extension: &str, dt: FileDateTime) -> String {
    format!(
        "{prefix}_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}{extension}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    )
}

/// Resolves a non-colliding save path in `dir` for `filename`, appending `" (2)"`, `" (3)"`, ...
/// before the extension if a file already exists, mirroring `FileNameForSave` (`data/data_document.cpp:190-241`).
#[must_use]
pub fn resolve_unique_save_path(
    dir: &Path,
    filename: &str,
    prefix: &str,
    timestamp_tag: &str,
) -> PathBuf {
    let mut sanitized = sanitize_filename(filename);
    if sanitized.is_empty() {
        sanitized = ".unknown".to_string();
    }
    if sanitized.starts_with('.') {
        sanitized = format!("{prefix}_{timestamp_tag}{sanitized}");
    }

    let (stem, ext) = match sanitized.rfind('.') {
        Some(pos) => (&sanitized[..pos], &sanitized[pos..]),
        None => (sanitized.as_str(), ""),
    };

    let mut candidate = dir.join(format!("{stem}{ext}"));
    let mut n = 2u32;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }

    let masked = mask_dangerous_windows_extension(&candidate.to_string_lossy());
    PathBuf::from(masked)
}
