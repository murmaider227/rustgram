//! Media and file caching subsystems.
//!
//! - [`binlog`]: Byte-compatible port of tdesktop's `lib_storage/storage/cache/` encrypted
//!   binlog cache database (`user_data/cache` and `user_data/media_cache`).
//! - [`media_cache`]: Rustgram-native SQLite-indexed media cache and local file location registry.
//! - [`legacy_temp`]: Startup sweep of temp-directory folders left by older builds.

pub mod binlog;
pub mod legacy_temp;
pub mod media_cache;
