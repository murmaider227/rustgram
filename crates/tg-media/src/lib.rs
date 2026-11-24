//! `tg-media`: Pure-Rust media logic ported from Telegram Desktop (C++/Qt, GPL-3.0).
//!
//! Covers:
//! - [`image`]: JPEG/PNG/WebP/GIF decoding, EXIF orientation, progressive JPEG inspection,
//!   inline stripped JPEG & vector path thumbnails (`image_prepare.cpp`), scaling, `Blur` /
//!   `BlurLargeImage` (StackBlur), circle/rounded corner masks, colorize, opaque, dithering,
//!   and photo/document thumbnail size selection (`data_session.cpp`, `data_photo.cpp`).
//! - [`transfer`]: MTProto download manager, per-DC session balancer, priority queue,
//!   out-of-order part assembly, CDN AES-256-CTR + SHA-256 verification, streamed file
//!   downloader (`streamed_file_downloader.cpp`), upload planner & multi-session uploader
//!   (`file_upload.cpp`), [`transfer::MediaInvoker`] network trait, and [`transfer::FakeMediaInvoker`].
//! - [`cache`]: tdesktop-compatible 128-bit [`cache::StorageKey`] derivations and disk-backed
//!   [`cache::LocalFileCache`] with size/time limits and LRU eviction.
//! - [`audio`]: 5-bit packed voice waveforms (`documentWaveformDecode` / `documentWaveformEncode5bit`),
//!   PCM/envelope waveform conversion, and voice duration formatting helpers.
//! - [`document`]: MIME type tables, extension classification (`core/mime_type.cpp`), magic-byte
//!   sniffing, and Windows/cross-platform filename sanitization (`FileNameFromUserString` / `FileNameForSave`).

pub mod audio;
pub mod cache;
pub mod document;
pub mod error;
pub mod image;
pub mod transfer;

pub use error::{MediaError, Result};
