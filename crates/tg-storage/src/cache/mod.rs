//! Media and file caching subsystems.
//!
//! - [`binlog`]: Byte-compatible port of tdesktop's `lib_storage/storage/cache/` encrypted
//!   binlog cache database (`user_data/cache` and `user_data/media_cache`).
//! - [`media_cache`]: Rustgram-native SQLite-indexed media cache and local file location registry.

pub mod binlog;
pub mod media_cache;
