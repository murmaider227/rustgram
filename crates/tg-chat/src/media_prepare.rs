//! "Send files" media preparation, photo/video dimension and aspect-ratio validation,
//! compression/grouping flags (`SendFilesWay`), slowmode checks, and album grouping rules.
//!
//! Port of:
//! - `Telegram/SourceFiles/storage/storage_media_prepare.h` and `storage_media_prepare.cpp`
//! - `Telegram/SourceFiles/ui/chat/attach/attach_prepare.h` and `attach_prepare.cpp`
//! - `Telegram/SourceFiles/ui/chat/attach/attach_send_files_way.h` and `attach_send_files_way.cpp`
//! - `Telegram/SourceFiles/storage/localimageloader.h` and `localimageloader.cpp` (`kFileSizeLimit`, `PhotoSideLimit`)
//! - `Telegram/SourceFiles/core/mime_type.cpp` (`IsMimeSticker`, `IsMimeAcceptedForPhotoVideoAlbum`, `DetectNameType`)
//! - `Telegram/lib_ui/ui/image/image_prepare.h` (`kReadBytesLimit`, `kReadMaxArea`)

use std::collections::VecDeque;

use crate::types::Size;

/// Maximum number of items in a single Telegram album, matching `kMaxAlbumCount = 10`.
pub const MAX_ALBUM_ITEMS: usize = 10;

/// Standard compressed photo max side length in pixels (`PhotoSideLimit(false) = 1280`).
pub const STANDARD_PHOTO_SIDE_LIMIT: i32 = 1280;

/// High-quality (HD) compressed photo max side length in pixels (`PhotoSideLimit(true) = 2560`).
pub const LARGE_PHOTO_SIDE_LIMIT: i32 = 2560;

/// Maximum allowed aspect ratio between width and height for photo/video thumbnails (`20:1`),
/// matching `Ui::ValidateThumbDimensions`.
pub const MAX_THUMB_ASPECT_RATIO: i64 = 20;

/// Maximum uploadable file size for standard accounts (2000 MiB), matching `kFileSizeLimit`.
pub const FILE_SIZE_LIMIT: i64 = 2_000 * 1024 * 1024;

/// Maximum uploadable file size for Premium accounts (4000 MiB), matching `kFileSizePremiumLimit`.
pub const FILE_SIZE_PREMIUM_LIMIT: i64 = 4_000 * 1024 * 1024;

/// Maximum raw image file size read as a photo (64 MiB), matching `Images::kReadBytesLimit`.
pub const IMAGE_READ_BYTES_LIMIT: i64 = 64 * 1024 * 1024;

/// Maximum image pixel area read as a photo (`12_032 * 9_024`), matching `Images::kReadMaxArea`.
pub const IMAGE_READ_MAX_AREA: i64 = 12_032 * 9_024;

/// Returns `true` if `(width, height)` are positive and have aspect ratio at most `20:1`.
///
/// Exact port of `Ui::ValidateThumbDimensions` in `ui/chat/attach/attach_prepare.cpp`.
pub const fn validate_thumb_dimensions(width: i32, height: i32) -> bool {
    if width <= 0 || height <= 0 {
        return false;
    }
    let w = width as i64;
    let h = height as i64;
    w <= MAX_THUMB_ASPECT_RATIO * h && h <= MAX_THUMB_ASPECT_RATIO * w
}

/// Returns `true` if an image with `(width, height)`, `animated`, and `mime` can be sent
/// as a compressed photo / photo-video album item.
///
/// Exact port of `ValidPhotoForAlbum` in `storage/storage_media_prepare.cpp`.
pub fn valid_photo_for_album(width: i32, height: i32, animated: bool, mime: &str) -> bool {
    if animated || (!mime.is_empty() && !mime.starts_with("image/")) {
        return false;
    }
    validate_thumb_dimensions(width, height)
}

/// Returns `true` if a video with thumbnail dimensions `(width, height)` is valid for an album.
///
/// Exact port of `ValidVideoForAlbum` in `storage/storage_media_prepare.cpp`.
pub const fn valid_video_for_album(width: i32, height: i32) -> bool {
    validate_thumb_dimensions(width, height)
}

/// Returns the photo side limit (`2560` when `send_large_photos` is true, else `1280`).
///
/// Exact port of `PhotoSideLimit(bool large)` in `storage/localimageloader.cpp`.
pub const fn photo_side_limit(send_large_photos: bool) -> i32 {
    if send_large_photos {
        LARGE_PHOTO_SIDE_LIMIT
    } else {
        STANDARD_PHOTO_SIDE_LIMIT
    }
}

/// Downscales `size` to fit inside `side_limit x side_limit` keeping aspect ratio,
/// matching `PrepareShownDimensions` in `storage/storage_media_prepare.cpp`.
pub fn prepare_shown_dimensions(size: Size, side_limit: i32) -> Size {
    if size.width > side_limit || size.height > side_limit {
        size.scaled_keep_aspect(Size::new(side_limit, side_limit))
    } else {
        size
    }
}

/// Returns `true` if `mime` is an animated Telegram sticker (`application/x-tgsticker`).
pub fn is_mime_sticker_animated(mime: &str) -> bool {
    mime == "application/x-tgsticker"
}

/// Returns `true` if `mime` is a sticker (`image/webp` or `application/x-tgsticker`),
/// matching `Core::IsMimeSticker`.
pub fn is_mime_sticker(mime: &str) -> bool {
    mime == "image/webp" || is_mime_sticker_animated(mime)
}

/// Returns `true` if `mime` is sent as a video (`video/mp4` or `video/quicktime`).
pub fn is_mime_sent_as_video(mime: &str) -> bool {
    mime == "video/mp4" || mime == "video/quicktime"
}

/// Returns `true` if `mime` is accepted when editing a photo/video album item,
/// matching `Core::IsMimeAcceptedForPhotoVideoAlbum`.
pub fn is_mime_accepted_for_photo_video_album(mime: &str) -> bool {
    mime == "image/jpeg" || mime == "image/png" || is_mime_sent_as_video(mime)
}

/// Compression and grouping preference flags for sending files, matching `Ui::SendFilesWay`.
///
/// Exact port of `Telegram/SourceFiles/ui/chat/attach/attach_send_files_way.h` and `.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SendFilesWay {
    flags: u8,
}

impl Default for SendFilesWay {
    fn default() -> Self {
        Self::new()
    }
}

impl SendFilesWay {
    const GROUP_FILES: u8 = 1 << 0;
    const SEND_IMAGES_AS_PHOTOS: u8 = 1 << 1;
    const HAS_COMPRESSED_STICKERS: u8 = 1 << 2;
    const SEND_LARGE_PHOTOS: u8 = 1 << 3;
    const DEFAULT_FLAGS: u8 = Self::GROUP_FILES | Self::SEND_IMAGES_AS_PHOTOS;
    const SERIALIZED_SEND_LARGE_PHOTOS: i32 = 4;

    /// Constructs the default `SendFilesWay` (`group_files = true`, `send_images_as_photos = true`).
    pub const fn new() -> Self {
        Self {
            flags: Self::DEFAULT_FLAGS,
        }
    }

    /// Whether compatible adjacent files are grouped into albums.
    pub const fn group_files(self) -> bool {
        (self.flags & Self::GROUP_FILES) != 0
    }

    /// Whether images/videos are compressed and sent as photos/media instead of raw files.
    pub const fn send_images_as_photos(self) -> bool {
        (self.flags & Self::SEND_IMAGES_AS_PHOTOS) != 0
    }

    /// Whether photos use the 2560px high-quality side limit instead of 1280px.
    pub const fn send_large_photos(self) -> bool {
        (self.flags & Self::SEND_LARGE_PHOTOS) != 0
    }

    /// Sets `send_images_as_photos`. When `has_compressed_stickers` is set and `value == false`,
    /// also clears `group_files`, matching `SendFilesWay::setSendImagesAsPhotos`.
    pub fn set_send_images_as_photos(&mut self, value: bool) {
        if value {
            self.flags |= Self::SEND_IMAGES_AS_PHOTOS;
        } else {
            if (self.flags & Self::HAS_COMPRESSED_STICKERS) != 0 {
                self.set_group_files(false);
            }
            self.flags &= !Self::SEND_IMAGES_AS_PHOTOS;
        }
    }

    /// Sets `group_files`. When `has_compressed_stickers` is set and `value == true`,
    /// also enables `send_images_as_photos`, matching `SendFilesWay::setGroupFiles`.
    pub fn set_group_files(&mut self, value: bool) {
        if value {
            self.flags |= Self::GROUP_FILES;
            if (self.flags & Self::HAS_COMPRESSED_STICKERS) != 0 {
                self.set_send_images_as_photos(true);
            }
        } else {
            self.flags &= !Self::GROUP_FILES;
        }
    }

    /// Sets `send_large_photos`.
    pub fn set_send_large_photos(&mut self, value: bool) {
        if value {
            self.flags |= Self::SEND_LARGE_PHOTOS;
        } else {
            self.flags &= !Self::SEND_LARGE_PHOTOS;
        }
    }

    /// Sets `has_compressed_stickers` (couples `group_files` and `send_images_as_photos`).
    pub fn set_has_compressed_stickers(&mut self, value: bool) {
        if value {
            self.flags |= Self::HAS_COMPRESSED_STICKERS;
        } else {
            self.flags &= !Self::HAS_COMPRESSED_STICKERS;
        }
    }

    /// Serializes to tdesktop's settings integer (`0..=7`),
    /// matching `SendFilesWay::serialize()`.
    pub const fn serialize(self) -> i32 {
        let mut result = if self.send_images_as_photos() && self.group_files() {
            0
        } else if self.send_images_as_photos() {
            1
        } else if self.group_files() {
            3
        } else {
            2
        };
        if self.send_large_photos() {
            result |= Self::SERIALIZED_SEND_LARGE_PHOTOS;
        }
        result
    }

    /// Deserializes from tdesktop's settings integer (`0..=7`),
    /// matching `SendFilesWay::FromSerialized()`.
    pub fn from_serialized(mut value: i32) -> Option<Self> {
        if !(0..=7).contains(&value) {
            return None;
        }
        let send_large = (value & Self::SERIALIZED_SEND_LARGE_PHOTOS) != 0;
        value &= !Self::SERIALIZED_SEND_LARGE_PHOTOS;
        let mut result = Self::new();
        result.set_group_files(value == 0 || value == 3);
        result.set_send_images_as_photos(value == 0 || value == 1);
        result.set_send_large_photos(send_large);
        Some(result)
    }
}

/// Album media grouping type, matching `Ui::AlbumType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlbumType {
    /// Not grouped as an album (sent as standalone messages).
    #[default]
    None,
    /// Photo + Video album (`sendImagesAsPhotos && groupFiles`).
    PhotoVideo,
    /// Audio/Music album.
    Music,
    /// Document/File album (files, or photos+videos when `!sendImagesAsPhotos`).
    File,
}

/// Classified media kind of a prepared file, matching `Ui::PreparedFile::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreparedFileType {
    /// Non-groupable media (e.g. animated GIF).
    None,
    /// Valid photo (can be sent compressed or as a file).
    Photo,
    /// Valid video (can be sent as media/GIF or as a file).
    Video,
    /// Audio/song file.
    Music,
    /// Generic file / document.
    #[default]
    File,
}

/// Returns whether a file of `file_type` can belong to `album`, matching `Ui::CanBeInAlbumType`.
pub const fn can_be_in_album_type(file_type: PreparedFileType, album: AlbumType) -> bool {
    match album {
        AlbumType::None => false,
        AlbumType::PhotoVideo => {
            matches!(file_type, PreparedFileType::Photo | PreparedFileType::Video)
        }
        AlbumType::Music => matches!(file_type, PreparedFileType::Music),
        AlbumType::File => matches!(
            file_type,
            PreparedFileType::Photo | PreparedFileType::Video | PreparedFileType::File
        ),
    }
}

/// Probed media metadata for classifying a file during `PrepareDetails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbedMedia {
    /// Image file with dimensions and animation flag.
    Image {
        /// Pixel dimensions.
        dimensions: Size,
        /// `true` for animated GIF/WebP.
        animated: bool,
    },
    /// Video file with thumbnail dimensions and audio/format flags.
    Video {
        /// Thumbnail pixel dimensions.
        thumbnail_dimensions: Size,
        /// Whether the video stream has an audio track.
        has_audio: bool,
        /// Whether the source is already a silent GIFV.
        is_gifv: bool,
        /// Whether the source is a WebM sticker.
        is_webm_sticker: bool,
        /// Whether the user toggled "mute / send as GIF" in the video editor.
        modified_as_gif: bool,
    },
    /// Song / music file.
    Song,
    /// Unrecognized / generic document.
    Other,
}

/// Prepared file descriptor, matching `Ui::PreparedFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFile {
    /// Local path (may be empty for clipboard images).
    pub path: String,
    /// File size in bytes.
    pub size: i64,
    /// Detected MIME type (e.g. `"image/jpeg"`, `"image/webp"`, `"video/mp4"`).
    pub mime: String,
    /// Original pixel dimensions (for photos/videos).
    pub original_dimensions: Size,
    /// Display/upload dimensions after `side_limit` clamping.
    pub shown_dimensions: Size,
    /// Classified file type.
    pub file_type: PreparedFileType,
    /// Whether this video is already a GIFV.
    pub is_gifv: bool,
    /// Whether this video originally has audio.
    pub video_has_audio: bool,
    /// Whether this video was muted into a GIF via editor modifications.
    pub video_modified_as_gif: bool,
    /// Whether this photo has an animated paint overlay without music (sends as GIF).
    pub sends_animation_as_gif: bool,
    /// Whether spoiler flag is enabled.
    pub spoiler: bool,
    /// Whether HD photo flag is enabled.
    pub send_large_photos: bool,
}

impl PreparedFile {
    /// Classifies a file from its probed media details and applies `side_limit`,
    /// directly porting `Storage::PrepareDetails` (`storage/storage_media_prepare.cpp:474-513`).
    pub fn from_probe(
        path: impl Into<String>,
        size: i64,
        mime: impl Into<String>,
        probe: ProbedMedia,
        side_limit: i32,
    ) -> Self {
        let path = path.into();
        let mime = mime.into();
        let mut file = Self {
            path,
            size,
            mime,
            original_dimensions: Size::default(),
            shown_dimensions: Size::default(),
            file_type: PreparedFileType::File,
            is_gifv: false,
            video_has_audio: false,
            video_modified_as_gif: false,
            sends_animation_as_gif: false,
            spoiler: false,
            send_large_photos: false,
        };

        match probe {
            ProbedMedia::Image {
                dimensions,
                animated,
            } => {
                file.original_dimensions = dimensions;
                if valid_photo_for_album(dimensions.width, dimensions.height, animated, &file.mime)
                {
                    file.shown_dimensions = prepare_shown_dimensions(dimensions, side_limit);
                    file.file_type = PreparedFileType::Photo;
                } else if animated {
                    file.file_type = PreparedFileType::None;
                }
            }
            ProbedMedia::Video {
                thumbnail_dimensions,
                has_audio,
                is_gifv,
                is_webm_sticker: _,
                modified_as_gif,
            } => {
                file.is_gifv = is_gifv;
                file.video_has_audio = has_audio;
                if valid_video_for_album(thumbnail_dimensions.width, thumbnail_dimensions.height) {
                    file.original_dimensions = thumbnail_dimensions;
                    file.shown_dimensions =
                        prepare_shown_dimensions(thumbnail_dimensions, side_limit);
                    file.video_modified_as_gif = !has_audio || modified_as_gif;
                    file.file_type = PreparedFileType::Video;
                }
            }
            ProbedMedia::Song => {
                file.file_type = PreparedFileType::Music;
            }
            ProbedMedia::Other => {
                file.file_type = PreparedFileType::File;
            }
        }
        file
    }

    /// Returns `true` if this file is a WebP/TGS sticker photo, matching `PreparedFile::isSticker()`.
    pub fn is_sticker(&self) -> bool {
        self.file_type == PreparedFileType::Photo && is_mime_sticker(&self.mime)
    }

    /// Returns `true` if this is a non-GIFV video file, matching `PreparedFile::isVideoFile()`.
    pub const fn is_video_file(&self) -> bool {
        matches!(self.file_type, PreparedFileType::Video) && !self.is_gifv
    }

    /// Returns `true` if this photo is eligible for the HD (2560px) option:
    /// non-sticker photo with either dimension strictly greater than `1280`.
    ///
    /// Exact port of `PreparedFile::canUseHighQualityPhoto()`.
    pub fn can_use_high_quality_photo(&self) -> bool {
        self.file_type == PreparedFileType::Photo
            && !self.is_sticker()
            && (self.original_dimensions.width > STANDARD_PHOTO_SIDE_LIMIT
                || self.original_dimensions.height > STANDARD_PHOTO_SIDE_LIMIT)
    }

    /// Returns `true` if a video with audio was explicitly muted into a GIF (so it must leave albums),
    /// matching `PreparedFile::sendsVideoAsGif()`.
    pub const fn sends_video_as_gif(&self) -> bool {
        matches!(self.file_type, PreparedFileType::Video)
            && self.video_has_audio
            && self.video_modified_as_gif
    }

    /// Returns the album type for this file given `send_images_as_photos`,
    /// matching `PreparedFile::albumType()`.
    pub const fn album_type(&self, send_images_as_photos: bool) -> AlbumType {
        match self.file_type {
            PreparedFileType::Photo => {
                if send_images_as_photos {
                    AlbumType::PhotoVideo
                } else {
                    AlbumType::File
                }
            }
            PreparedFileType::Video => AlbumType::PhotoVideo,
            PreparedFileType::Music => AlbumType::Music,
            PreparedFileType::File => AlbumType::File,
            PreparedFileType::None => AlbumType::None,
        }
    }
}

/// Validation error from [`prepare_media_list`], matching `Ui::PreparedList::Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreparedListError {
    /// No error.
    #[default]
    None,
    /// A URL was not a local file.
    NonLocalUrl,
    /// Entry was a directory.
    Directory,
    /// File does not exist, is unreadable, or has size `<= 0`.
    EmptyFile,
    /// File exceeds `FILE_SIZE_LIMIT` (non-Premium) or `FILE_SIZE_PREMIUM_LIMIT` (Premium).
    TooLargeFile,
}

/// Input entry describing a candidate file for [`prepare_media_list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFile {
    /// Path or URL display string.
    pub path: String,
    /// `false` if this came from a non-local URL.
    pub is_local: bool,
    /// `true` if path is a directory.
    pub is_dir: bool,
    /// `true` if path exists, is a regular file, and is readable.
    pub is_readable_file: bool,
    /// File size in bytes.
    pub size: i64,
    /// Detected MIME type.
    pub mime: String,
    /// Probed media metadata (used for the first `MAX_ALBUM_ITEMS` files).
    pub probe: ProbedMedia,
}

/// Prepared list of files ready for the send-files box, matching `Ui::PreparedList`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreparedList {
    /// Validation error (if preparation aborted early).
    pub error: PreparedListError,
    /// Offending path/URL when `error != None`.
    pub error_data: String,
    /// First batch of up to 10 prepared files.
    pub files: Vec<PreparedFile>,
    /// Remaining files queued beyond the first 10.
    pub files_to_process: VecDeque<PreparedFile>,
    /// Optional forced override for `send_images_as_photos`.
    pub override_send_images_as_photos: Option<bool>,
}

impl PreparedList {
    /// Returns whether a caption can be added to this list, matching `PreparedList::canAddCaption`.
    pub fn can_add_caption(&self, compress: bool) -> bool {
        let Some(last) = self.files.last() else {
            return false;
        };
        let is_tgs = last.path.to_ascii_lowercase().ends_with(".tgs");
        let is_sticker = is_tgs || (!compress && is_mime_sticker(&last.mime));
        !is_sticker
    }

    /// Returns whether the caption can be moved above media, matching `PreparedList::canMoveCaption`.
    pub fn can_move_caption(&self, sending_album: bool, compress: bool) -> bool {
        if !self.can_add_caption(compress) || self.files.len() > MAX_ALBUM_ITEMS {
            return false;
        }
        if !sending_album || !compress {
            return self.files.len() == 1;
        }
        self.files.iter().all(|f| {
            matches!(
                f.file_type,
                PreparedFileType::Photo | PreparedFileType::Video
            )
        })
    }

    /// Returns whether the list can be made paid media, matching `PreparedList::canChangePrice`.
    pub fn can_change_price(&self, sending_album: bool, compress: bool) -> bool {
        self.can_move_caption(sending_album, compress)
    }

    /// Returns whether this list can be sent as a single message in a slowmode chat.
    pub fn can_be_sent_in_slowmode(&self) -> bool {
        self.can_be_sent_in_slowmode_with(&PreparedList::default())
    }

    /// Returns whether `self` combined with `other` can be sent as a single message in slowmode.
    ///
    /// Exact port of `PreparedList::canBeSentInSlowmodeWith` in `attach_prepare.cpp:378-410`:
    /// - No deferred `files_to_process`, total count `<= 10`.
    /// - `< 2` total items is always OK.
    /// - No `Type::None` (animated GIFs).
    /// - Files and Videos cannot be grouped together; Music can only group with Music.
    pub fn can_be_sent_in_slowmode_with(&self, other: &PreparedList) -> bool {
        if !self.files_to_process.is_empty() || !other.files_to_process.is_empty() {
            return false;
        }
        let total = self.files.len() + other.files.len();
        if total < 2 {
            return true;
        }
        if total > MAX_ALBUM_ITEMS {
            return false;
        }

        let has = |t: PreparedFileType| {
            self.files
                .iter()
                .chain(other.files.iter())
                .any(|f| f.file_type == t)
        };
        let has_non_grouping = has(PreparedFileType::None);
        let has_photos = has(PreparedFileType::Photo);
        let has_files = has(PreparedFileType::File);
        let has_videos = has(PreparedFileType::Video);
        let has_music = has(PreparedFileType::Music);

        if has_non_grouping {
            false
        } else if has_files {
            !has_music && !has_videos
        } else if has_videos {
            !has_music && !has_files
        } else if has_music {
            !has_videos && !has_files && !has_photos
        } else {
            !has_non_grouping && (!has_files || !has_videos)
        }
    }

    /// Returns whether the "Group items" checkbox should be offered.
    ///
    /// Exact port of `PreparedList::hasGroupOption` in `attach_prepare.cpp:445-464`.
    pub fn has_group_option(&self, slowmode: bool) -> bool {
        if slowmode || self.files.len() < 2 {
            return false;
        }
        let mut last_type = PreparedFileType::None;
        for file in &self.files {
            let compatible = file.file_type == last_type
                || matches!(
                    (file.file_type, last_type),
                    (PreparedFileType::Video, PreparedFileType::Photo)
                        | (PreparedFileType::Photo, PreparedFileType::Video)
                        | (PreparedFileType::File, PreparedFileType::Photo)
                        | (PreparedFileType::Photo, PreparedFileType::File)
                );
            if compatible && last_type != PreparedFileType::None {
                return true;
            }
            last_type = file.file_type;
        }
        false
    }

    /// Returns whether the "Compress images" / "Send as photos" checkbox should be offered.
    ///
    /// Exact port of `PreparedList::hasSendImagesAsPhotosOption` in `attach_prepare.cpp:466-475`.
    pub fn has_send_images_as_photos_option(&self, slowmode: bool) -> bool {
        if slowmode {
            return self.files.len() == 1
                && matches!(
                    self.files[0].file_type,
                    PreparedFileType::Photo | PreparedFileType::Video
                );
        }
        self.files.iter().any(|f| {
            matches!(
                f.file_type,
                PreparedFileType::Photo | PreparedFileType::Video
            )
        })
    }

    /// Returns whether any file is a sticker, matching `PreparedList::hasSticker`.
    pub fn has_sticker(&self) -> bool {
        self.files.iter().any(PreparedFile::is_sticker)
    }

    /// Returns whether the spoiler toggle should appear in the send menu,
    /// matching `PreparedList::hasSpoilerMenu`.
    pub fn has_spoiler_menu(&self, compress: bool) -> bool {
        let all_video = self
            .files
            .iter()
            .all(|f| f.file_type == PreparedFileType::Video);
        let all_media = self.files.iter().all(|f| {
            matches!(
                f.file_type,
                PreparedFileType::Photo | PreparedFileType::Video
            )
        });
        all_video || (all_media && compress)
    }

    /// Returns whether the HD photo quality option should appear,
    /// matching `PreparedList::hasSendLargePhotosOption`.
    pub fn has_send_large_photos_option(&self, compress: bool) -> bool {
        compress
            && self
                .files
                .iter()
                .any(PreparedFile::can_use_high_quality_photo)
    }
}

/// Validates and prepares candidate files into a [`PreparedList`], splitting after 10 items.
///
/// Direct port of `Storage::PrepareMediaList` (`storage/storage_media_prepare.cpp:343-424`).
/// If `stop_on_error` is `true`, returns immediately on the first invalid file; otherwise
/// skips invalid files and reports them in the returned error list.
pub fn prepare_media_list(
    candidates: Vec<CandidateFile>,
    is_premium: bool,
    send_large_photos: bool,
    stop_on_error: bool,
) -> (PreparedList, Vec<(PreparedListError, String)>) {
    let side_limit = photo_side_limit(send_large_photos);
    let mut result = PreparedList::default();
    let mut errors = Vec::new();

    for c in candidates {
        let err = if !c.is_local {
            Some(PreparedListError::NonLocalUrl)
        } else if c.is_dir {
            Some(PreparedListError::Directory)
        } else if !c.is_readable_file || c.size <= 0 {
            Some(PreparedListError::EmptyFile)
        } else if c.size > FILE_SIZE_PREMIUM_LIMIT || (c.size > FILE_SIZE_LIMIT && !is_premium) {
            Some(PreparedListError::TooLargeFile)
        } else {
            None
        };

        if let Some(error) = err {
            errors.push((error, c.path.clone()));
            if stop_on_error {
                let mut err_list = PreparedList {
                    error,
                    error_data: c.path.clone(),
                    ..PreparedList::default()
                };
                if error == PreparedListError::TooLargeFile {
                    err_list.files.push(PreparedFile::from_probe(
                        c.path,
                        c.size,
                        c.mime,
                        ProbedMedia::Other,
                        side_limit,
                    ));
                }
                return (err_list, errors);
            }
            continue;
        }

        let prepared = PreparedFile::from_probe(c.path, c.size, c.mime, c.probe, side_limit);
        if result.files.len() < MAX_ALBUM_ITEMS {
            result.files.push(prepared);
        } else {
            result.files_to_process.push_back(prepared);
        }
    }

    (result, errors)
}

/// A contiguous group of files to be sent either as an album or standalone,
/// matching `Ui::PreparedGroup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGroup {
    /// Files belonging to this group.
    pub files: Vec<PreparedFile>,
    /// Album type (`AlbumType::None` for single-item or ungrouped batches).
    pub album_type: AlbumType,
}

fn group_type_for_file(
    file_type: PreparedFileType,
    group_files: bool,
    send_images_as_photos: bool,
) -> AlbumType {
    match file_type {
        PreparedFileType::Music => {
            if group_files {
                AlbumType::Music
            } else {
                AlbumType::None
            }
        }
        PreparedFileType::Video | PreparedFileType::Photo => {
            if group_files && send_images_as_photos {
                AlbumType::PhotoVideo
            } else if group_files && !send_images_as_photos {
                AlbumType::File
            } else {
                AlbumType::None
            }
        }
        PreparedFileType::File => {
            if group_files {
                AlbumType::File
            } else {
                AlbumType::None
            }
        }
        PreparedFileType::None => AlbumType::None,
    }
}

/// Divides `files` into contiguous albums (max 10 items per album) or standalone groups.
///
/// Exact port of `GroupRanges` and `Ui::DivideByGroups` in
/// `Telegram/SourceFiles/ui/chat/attach/attach_prepare.cpp:137-204, 545-564`.
pub fn divide_by_groups(
    files: Vec<PreparedFile>,
    way: SendFilesWay,
    slowmode: bool,
) -> Vec<PreparedGroup> {
    if files.is_empty() {
        return Vec::new();
    }

    let send_images_as_photos = way.send_images_as_photos();
    let group_files = way.group_files() || slowmode;

    let mut ranges: Vec<(usize, usize, AlbumType)> = Vec::new();
    let mut from = 0usize;
    let mut group_type = AlbumType::None;

    for (i, file) in files.iter().enumerate() {
        let file_group_type = if file.sends_animation_as_gif || file.sends_video_as_gif() {
            AlbumType::None
        } else {
            group_type_for_file(file.file_type, group_files, send_images_as_photos)
        };
        let count = i - from;
        if (i > from && group_type != file_group_type)
            || (group_type != AlbumType::None && count == MAX_ALBUM_ITEMS)
        {
            ranges.push((
                from,
                i,
                if count > 1 {
                    group_type
                } else {
                    AlbumType::None
                },
            ));
            from = i;
        }
        group_type = file_group_type;
    }

    let till = files.len();
    let count = till - from;
    ranges.push((
        from,
        till,
        if count > 1 {
            group_type
        } else {
            AlbumType::None
        },
    ));

    let mut iter = files.into_iter();
    let mut result = Vec::with_capacity(ranges.len());
    for (start, end, album_type) in ranges {
        let batch: Vec<PreparedFile> = iter.by_ref().take(end - start).collect();
        result.push(PreparedGroup {
            files: batch,
            album_type,
        });
    }
    result
}

/// Classification of dragged/pasted MIME data, matching `Storage::MimeDataState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimeDataState {
    /// Not acceptable / ignored.
    None,
    /// Single generic file.
    Files,
    /// Multiple generic files.
    FilesArchive,
    /// All entries are small non-GIF readable images.
    PhotoFiles,
    /// All entries are images or videos (some too large or GIF/video).
    MediaFiles,
    /// In-memory clipboard image.
    Image,
    /// Single dropped folder.
    Folder,
}

/// Input entry for [`compute_mime_data_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraggedFileEntry {
    /// File name or path.
    pub path: String,
    /// Whether this URL is a local file.
    pub is_local: bool,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes.
    pub size: i64,
    /// Detected MIME type.
    pub mime: String,
    /// Whether the image decoder can read this file.
    pub image_readable: bool,
}

/// Computes the drag-and-drop overlay state for dropped files,
/// matching `Storage::ComputeMimeDataState` in `storage/storage_media_prepare.cpp:280-341`.
pub fn compute_mime_data_state(
    is_internal_forward: bool,
    has_clipboard_image: bool,
    entries: &[DraggedFileEntry],
) -> MimeDataState {
    if is_internal_forward {
        return MimeDataState::None;
    }
    if has_clipboard_image {
        return MimeDataState::Image;
    }
    if entries.is_empty() {
        return MimeDataState::None;
    }

    let mut all_small_images = true;
    let mut all_media = true;

    for entry in entries {
        if !entry.is_local {
            return MimeDataState::None;
        }
        if entry.is_dir {
            return if entries.len() == 1 {
                MimeDataState::Folder
            } else {
                MimeDataState::None
            };
        }
        if entry.size > FILE_SIZE_PREMIUM_LIMIT {
            return MimeDataState::None;
        }
        if all_small_images
            && (entry.size > IMAGE_READ_BYTES_LIMIT
                || entry.mime == "image/gif"
                || !is_image_extension(&entry.path)
                || !entry.image_readable)
        {
            all_small_images = false;
        }
        if all_media && !is_image_extension(&entry.path) && !is_video_extension(&entry.path) {
            all_media = false;
        }
    }

    if all_small_images {
        MimeDataState::PhotoFiles
    } else if all_media {
        MimeDataState::MediaFiles
    } else if entries.len() > 1 {
        MimeDataState::FilesArchive
    } else {
        MimeDataState::Files
    }
}

/// Returns `true` if `state` can be dropped onto the story editor,
/// matching `Storage::IsAcceptableStoryDragState`.
pub const fn is_acceptable_story_drag_state(state: MimeDataState) -> bool {
    matches!(
        state,
        MimeDataState::Image | MimeDataState::PhotoFiles | MimeDataState::MediaFiles
    )
}

fn file_extension_lower(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) => ext.to_ascii_lowercase(),
        None => String::new(),
    }
}

fn is_image_extension(path: &str) -> bool {
    matches!(
        file_extension_lower(path).as_str(),
        "afdesign"
            | "ai"
            | "avif"
            | "bmp"
            | "dng"
            | "gif"
            | "heic"
            | "icns"
            | "ico"
            | "jfif"
            | "jpeg"
            | "jpg"
            | "jpg-large"
            | "jxl"
            | "nef"
            | "png"
            | "png-large"
            | "psd"
            | "qoi"
            | "raw"
            | "sketch"
            | "svg"
            | "tga"
            | "tif"
            | "tiff"
            | "webp"
    )
}

fn is_video_extension(path: &str) -> bool {
    matches!(
        file_extension_lower(path).as_str(),
        "3g2"
            | "3gp"
            | "3gpp"
            | "aep"
            | "avi"
            | "flv"
            | "h264"
            | "m4s"
            | "m4v"
            | "mkv"
            | "mov"
            | "mp4"
            | "mpeg"
            | "mpg"
            | "ogv"
            | "srt"
            | "tgs"
            | "tgv"
            | "vob"
            | "webm"
            | "wmv"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(path: &str, w: i32, h: i32) -> PreparedFile {
        PreparedFile::from_probe(
            path,
            100_000,
            "image/jpeg",
            ProbedMedia::Image {
                dimensions: Size::new(w, h),
                animated: false,
            },
            STANDARD_PHOTO_SIDE_LIMIT,
        )
    }

    fn video(path: &str, has_audio: bool, modified_as_gif: bool) -> PreparedFile {
        PreparedFile::from_probe(
            path,
            500_000,
            "video/mp4",
            ProbedMedia::Video {
                thumbnail_dimensions: Size::new(1280, 720),
                has_audio,
                is_gifv: !has_audio,
                is_webm_sticker: false,
                modified_as_gif,
            },
            STANDARD_PHOTO_SIDE_LIMIT,
        )
    }

    fn music(path: &str) -> PreparedFile {
        PreparedFile::from_probe(
            path,
            2_000_000,
            "audio/mpeg",
            ProbedMedia::Song,
            STANDARD_PHOTO_SIDE_LIMIT,
        )
    }

    fn doc_file(path: &str) -> PreparedFile {
        PreparedFile::from_probe(
            path,
            50_000,
            "application/pdf",
            ProbedMedia::Other,
            STANDARD_PHOTO_SIDE_LIMIT,
        )
    }

    #[test]
    fn photo_dimensions_and_aspect_ratio_limits() {
        // Exactly 20:1 is valid; 20:1 + 1px is rejected
        assert!(validate_thumb_dimensions(2000, 100));
        assert!(validate_thumb_dimensions(100, 2000));
        assert!(!validate_thumb_dimensions(2001, 100));
        assert!(!validate_thumb_dimensions(100, 2001));
        assert!(!validate_thumb_dimensions(0, 100));

        // Animated GIF cannot be sent as a photo album item -> classified as Type::None
        assert!(!valid_photo_for_album(500, 500, true, "image/gif"));
        let gif = PreparedFile::from_probe(
            "anim.gif",
            10_000,
            "image/gif",
            ProbedMedia::Image {
                dimensions: Size::new(500, 500),
                animated: true,
            },
            STANDARD_PHOTO_SIDE_LIMIT,
        );
        assert_eq!(gif.file_type, PreparedFileType::None);

        // Extreme aspect-ratio image falls back to File
        let pano = photo("pano.jpg", 4000, 100);
        assert_eq!(pano.file_type, PreparedFileType::File);

        // Normal large photo is Photo and supports HD option (> 1280)
        let hd = photo("hd.jpg", 2560, 1440);
        assert_eq!(hd.file_type, PreparedFileType::Photo);
        assert_eq!(hd.shown_dimensions, Size::new(1280, 720));
        assert!(hd.can_use_high_quality_photo());

        let small = photo("small.jpg", 1280, 720);
        assert!(!small.can_use_high_quality_photo());
    }

    #[test]
    fn send_files_way_serialization_and_sticker_coupling() {
        // Default: group=true, photos=true, large=false -> serialized 0
        let mut way = SendFilesWay::new();
        assert_eq!(way.serialize(), 0);

        // Round-trip all 8 valid serialized states 0..=7
        for raw in 0..=7 {
            let decoded = SendFilesWay::from_serialized(raw).expect("valid");
            assert_eq!(decoded.serialize(), raw);
        }
        assert!(SendFilesWay::from_serialized(-1).is_none());
        assert!(SendFilesWay::from_serialized(8).is_none());

        // Sticker coupling: turning off send_images_as_photos clears group_files,
        // turning on group_files re-enables send_images_as_photos
        way.set_has_compressed_stickers(true);
        way.set_send_images_as_photos(false);
        assert!(!way.send_images_as_photos());
        assert!(!way.group_files());
        way.set_group_files(true);
        assert!(way.group_files());
        assert!(way.send_images_as_photos());
    }

    #[test]
    fn divide_by_groups_albums_max_10_and_mixed_media() {
        // 13 photos -> album of 10 + album of 3
        let thirteen: Vec<PreparedFile> = (0..13)
            .map(|i| photo(&format!("p{i}.jpg"), 800, 600))
            .collect();
        let groups = divide_by_groups(thirteen, SendFilesWay::new(), false);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].files.len(), 10);
        assert_eq!(groups[0].album_type, AlbumType::PhotoVideo);
        assert_eq!(groups[1].files.len(), 3);
        assert_eq!(groups[1].album_type, AlbumType::PhotoVideo);

        // 11 photos -> album of 10 + standalone 1 (AlbumType::None!)
        let eleven: Vec<PreparedFile> = (0..11)
            .map(|i| photo(&format!("p{i}.jpg"), 800, 600))
            .collect();
        let groups = divide_by_groups(eleven, SendFilesWay::new(), false);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].album_type, AlbumType::PhotoVideo);
        assert_eq!(groups[1].files.len(), 1);
        assert_eq!(groups[1].album_type, AlbumType::None);

        // Mixed media with send_images_as_photos = true:
        // [Photo, Video, Music, Music, File, File] -> PhotoVideo(2), Music(2), File(2)
        let mixed = vec![
            photo("a.jpg", 800, 600),
            video("b.mp4", true, false),
            music("c.mp3"),
            music("d.mp3"),
            doc_file("e.pdf"),
            doc_file("f.pdf"),
        ];
        let groups = divide_by_groups(mixed, SendFilesWay::new(), false);
        assert_eq!(groups.len(), 3);
        assert_eq!(
            (groups[0].files.len(), groups[0].album_type),
            (2, AlbumType::PhotoVideo)
        );
        assert_eq!(
            (groups[1].files.len(), groups[1].album_type),
            (2, AlbumType::Music)
        );
        assert_eq!(
            (groups[2].files.len(), groups[2].album_type),
            (2, AlbumType::File)
        );

        // Send-as-files rule (send_images_as_photos = false, group_files = true):
        // Photos + Videos + Files all group together into AlbumType::File!
        let mut as_files = SendFilesWay::new();
        as_files.set_send_images_as_photos(false);
        let mixed_files = vec![
            photo("a.jpg", 800, 600),
            doc_file("b.pdf"),
            video("c.mp4", true, false),
        ];
        let groups = divide_by_groups(mixed_files, as_files, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
        assert_eq!(groups[0].album_type, AlbumType::File);

        // Video muted to GIF breaks album
        let with_gif = vec![
            photo("a.jpg", 800, 600),
            video("muted.mp4", true, true),
            photo("b.jpg", 800, 600),
        ];
        let groups = divide_by_groups(with_gif, SendFilesWay::new(), false);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| g.album_type == AlbumType::None));

        // Ungrouped (group_files = false), slowmode = false -> single group of all with AlbumType::None
        let mut ungrouped = SendFilesWay::new();
        ungrouped.set_group_files(false);
        let two_photos = vec![photo("a.jpg", 800, 600), photo("b.jpg", 800, 600)];
        let groups = divide_by_groups(two_photos, ungrouped, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].album_type, AlbumType::None);

        // Slowmode forces grouping even when way.group_files() == false
        let two_photos = vec![photo("a.jpg", 800, 600), photo("b.jpg", 800, 600)];
        let groups = divide_by_groups(two_photos, ungrouped, true);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].album_type, AlbumType::PhotoVideo);
    }

    #[test]
    fn prepare_media_list_validation_and_slowmode_options() {
        let candidates = vec![
            CandidateFile {
                path: "dir".into(),
                is_local: true,
                is_dir: true,
                is_readable_file: false,
                size: 0,
                mime: String::new(),
                probe: ProbedMedia::Other,
            },
            CandidateFile {
                path: "huge.bin".into(),
                is_local: true,
                is_dir: false,
                is_readable_file: true,
                size: FILE_SIZE_LIMIT + 1,
                mime: "application/octet-stream".into(),
                probe: ProbedMedia::Other,
            },
            CandidateFile {
                path: "ok.jpg".into(),
                is_local: true,
                is_dir: false,
                is_readable_file: true,
                size: 1024,
                mime: "image/jpeg".into(),
                probe: ProbedMedia::Image {
                    dimensions: Size::new(1600, 900),
                    animated: false,
                },
            },
        ];

        // Non-premium rejects huge.bin (2000 MiB + 1)
        let (list, errs) = prepare_media_list(candidates.clone(), false, false, false);
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].0, PreparedListError::Directory);
        assert_eq!(errs[1].0, PreparedListError::TooLargeFile);
        assert_eq!(list.files.len(), 1);
        assert!(list.has_send_large_photos_option(true));
        assert!(!list.has_send_large_photos_option(false));

        // Premium accepts huge.bin (<= 4000 MiB)
        let (list_prem, errs_prem) = prepare_media_list(candidates, true, false, false);
        assert_eq!(errs_prem.len(), 1);
        assert_eq!(list_prem.files.len(), 2);

        // Slowmode: Files + Videos cannot be sent together; Photos + Files can
        let list_file_video = PreparedList {
            files: vec![doc_file("a.pdf"), video("b.mp4", true, false)],
            ..PreparedList::default()
        };
        assert!(!list_file_video.can_be_sent_in_slowmode());

        let list_photo_file = PreparedList {
            files: vec![photo("a.jpg", 800, 600), doc_file("b.pdf")],
            ..PreparedList::default()
        };
        assert!(list_photo_file.can_be_sent_in_slowmode());
        assert!(list_photo_file.has_group_option(false));
        assert!(!list_photo_file.has_group_option(true));

        // MimeDataState classification
        let dragged_photos = vec![DraggedFileEntry {
            path: "a.jpg".into(),
            is_local: true,
            is_dir: false,
            size: 1000,
            mime: "image/jpeg".into(),
            image_readable: true,
        }];
        assert_eq!(
            compute_mime_data_state(false, false, &dragged_photos),
            MimeDataState::PhotoFiles
        );
        assert!(is_acceptable_story_drag_state(MimeDataState::PhotoFiles));
        assert!(!is_acceptable_story_drag_state(MimeDataState::Files));

        // Caption, move-caption, sticker, and spoiler rules
        let webp_sticker = PreparedFile::from_probe(
            "sticker.webp",
            5_000,
            "image/webp",
            ProbedMedia::Image {
                dimensions: Size::new(512, 512),
                animated: false,
            },
            STANDARD_PHOTO_SIDE_LIMIT,
        );
        let sticker_list = PreparedList {
            files: vec![webp_sticker],
            ..PreparedList::default()
        };
        assert!(sticker_list.has_sticker());
        assert!(sticker_list.can_add_caption(true));
        assert!(!sticker_list.can_add_caption(false));

        let photo_album = PreparedList {
            files: vec![photo("1.jpg", 800, 600), photo("2.jpg", 800, 600)],
            ..PreparedList::default()
        };
        assert!(photo_album.can_move_caption(true, true));
        assert!(!photo_album.can_move_caption(false, true));
        assert!(photo_album.has_spoiler_menu(true));
        assert!(!photo_album.has_spoiler_menu(false));
    }
}
