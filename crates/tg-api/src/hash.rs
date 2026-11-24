//! Telegram 64-bit rolling list hash functions (`Api::HashInit`, `Api::HashUpdate`,
//! `Api::HashFinalize`, `Api::CountHash`, `CountDocumentVectorHash`).
//!
//! Port origin: `Telegram/SourceFiles/api/api_hash.h` and `api/api_hash.cpp`.
//!
//! Core rolling hash primitives are re-exported from [`tg_data`] so scheduled/quick-reply
//! lists, contacts, stickers, GIFs, and cached lists share one exact implementation.

pub use tg_data::{
    hash_finalize, hash_init, hash_update_i32, hash_update_i64, hash_update_u32, hash_update_u64,
};

/// Computes the 64-bit rolling hash over a sequence of `u64` values
/// (`Api::CountHash` in `api/api_hash.h:63-70`).
#[must_use]
pub fn count_hash_u64<I>(values: I) -> u64
where
    I: IntoIterator<Item = u64>,
{
    tg_data::count_hash(values)
}

/// Computes the 64-bit rolling hash over a sequence of `i64` values
/// (`Api::CountHash` in `api/api_hash.h:63-70`).
#[must_use]
pub fn count_hash_i64<I>(values: I) -> u64
where
    I: IntoIterator<Item = i64>,
{
    let mut result = hash_init();
    for value in values {
        hash_update_i64(&mut result, value);
    }
    hash_finalize(result)
}

/// Computes the 64-bit rolling hash over a sequence of `u32` values
/// (`Api::CountHash` in `api/api_hash.h:63-70`).
#[must_use]
pub fn count_hash_u32<I>(values: I) -> u64
where
    I: IntoIterator<Item = u32>,
{
    let mut result = hash_init();
    for value in values {
        hash_update_u32(&mut result, value);
    }
    hash_finalize(result)
}

/// Computes the 64-bit rolling hash over a sequence of `i32` values (each sign-extended
/// to 64 bits, matching `Api::HashUpdate(uint64&, int32)` in `api/api_hash.h:55-57`).
#[must_use]
pub fn count_hash_i32<I>(values: I) -> u64
where
    I: IntoIterator<Item = i32>,
{
    let mut result = hash_init();
    for value in values {
        hash_update_i32(&mut result, value);
    }
    hash_finalize(result)
}

/// Computes the 64-bit `contacts.getContacts` hash over contact bare user IDs.
///
/// Sorts `user_ids` in ascending numeric order, deduplicates them, and feeds each `u64`
/// into `Api::HashInit` / `HashUpdate` / `HashFinalize` (`api/api_hash.h:36-70`),
/// returning the bit-cast `i64` for TL `contacts.getContacts#5dd69e12 hash:long`.
#[must_use]
pub fn count_contacts_hash(user_ids: &[u64]) -> i64 {
    let mut sorted = user_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    count_hash_u64(sorted) as i64
}

/// Computes the 64-bit rolling hash over a slice of document IDs
/// (`CountDocumentVectorHash`, `api/api_hash.cpp:18-25`).
#[must_use]
pub fn count_document_vector_hash(document_ids: &[u64]) -> u64 {
    count_hash_u64(document_ids.iter().copied())
}
