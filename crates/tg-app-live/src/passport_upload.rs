//! Telegram Passport secure file encryption and chunked upload (`upload.saveFilePart`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/passport/passport_encryption.cpp`
//! - `Telegram/SourceFiles/passport/passport_form_controller.cpp`
//! - `Telegram/SourceFiles/storage/file_upload.cpp`

use std::path::PathBuf;
use std::time::Duration;

use tg_api::RpcClient;
use tg_passport::controller::PassportController;
use tg_passport::encryption::{SECRET_SIZE, encrypt_file_for_upload, encrypt_value_secret};
use tg_passport::types::{
    EditFile, FileType, LoadStatus, PassportFile, PassportValue, UploadScanData, ValueType,
};
use tg_tl::functions as tl_fns;

use crate::convert::now_unix_i32;
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;

/// Maximum Passport scan/selfie/translation file size in bytes (10 MiB, matching tdesktop).
pub const PASSPORT_MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Chunk size for `upload.saveFilePart` (512 KiB — maximum allowed by Telegram MTProto,
/// divisible by 1024 bytes).
pub const PASSPORT_UPLOAD_PART_SIZE: usize = 512 * 1024;

/// Per-part RPC timeout for `upload.saveFilePart`.
const PASSPORT_UPLOAD_PART_TIMEOUT: Duration = Duration::from_secs(20);

/// Encrypts `raw_bytes` per Telegram Passport (`passport_encryption.cpp`), uploads all chunks
/// via `upload.saveFilePart`, and returns a staged [`EditFile`] whose [`EditFile::to_input_tl`]
/// emits `InputSecureFileUploaded`.
pub fn encrypt_and_upload_passport_file(
    live: &LiveSession,
    master_secret: &[u8; SECRET_SIZE],
    file_type: FileType,
    raw_bytes: &[u8],
    file_id: i64,
) -> Result<EditFile, String> {
    if raw_bytes.is_empty() {
        return Err("Passport scan file cannot be empty.".to_owned());
    }
    if raw_bytes.len() > PASSPORT_MAX_FILE_SIZE {
        return Err(format!(
            "Passport file exceeds {} MiB limit ({} bytes).",
            PASSPORT_MAX_FILE_SIZE / (1024 * 1024),
            raw_bytes.len()
        ));
    }
    if file_id == 0 {
        return Err("Passport upload file_id must be non-zero.".to_owned());
    }

    let encrypted = encrypt_file_for_upload(raw_bytes, None)
        .map_err(|e| format!("Passport file encryption failed: {e}"))?;
    let encrypted_secret =
        encrypt_value_secret(&encrypted.file_secret, master_secret, &encrypted.hash)
            .map_err(|e| format!("Passport file secret encryption failed: {e}"))?;

    let total_len = encrypted.bytes.len();
    let parts_count = total_len.div_ceil(PASSPORT_UPLOAD_PART_SIZE) as i32;
    let dc_id = live.runtime().rpc().main_dc_id();

    for (idx, chunk) in encrypted
        .bytes
        .chunks(PASSPORT_UPLOAD_PART_SIZE)
        .enumerate()
    {
        let req = tl_fns::upload::SaveFilePart {
            file_id,
            file_part: idx as i32,
            bytes: chunk.to_vec(),
        };
        let rpc = live.runtime().rpc().clone();
        let saved = block_on_runtime(live.runtime().tokio_handle(), async move {
            with_rpc_timeout(PASSPORT_UPLOAD_PART_TIMEOUT, async move {
                rpc.invoke(&req).await
            })
            .await
        })?;
        if !saved {
            return Err(format!(
                "Server rejected upload.saveFilePart chunk {idx}/{parts_count}."
            ));
        }
    }

    let fields = PassportFile {
        id: file_id,
        access_hash: 0,
        size: total_len as i64,
        dc_id,
        date: now_unix_i32(),
        hash: encrypted.hash.to_vec(),
        secret: encrypted.file_secret.to_vec(),
        encrypted_secret: encrypted_secret.to_vec(),
        download_status: LoadStatus::Done,
        error: String::new(),
    };

    let upload_data = UploadScanData {
        file_id,
        parts_count,
        md5_checksum: encrypted.md5_checksum,
        hash: encrypted.hash.to_vec(),
        bytes: Vec::new(),
        status: LoadStatus::Done,
    };

    Ok(EditFile {
        file_type,
        fields,
        upload_data: Some(upload_data),
        deleted: false,
    })
}

/// Stages a newly uploaded [`EditFile`] onto `controller`'s form value for `value_type`
/// so the subsequent [`PassportController::prepare_save_value`] emits `InputSecureFileUploaded`.
pub fn stage_uploaded_file_in_controller(
    controller: &mut PassportController,
    value_type: ValueType,
    edit_file: EditFile,
) {
    let value = controller
        .form_mut()
        .values
        .entry(value_type)
        .or_insert_with(|| PassportValue::new(value_type));

    // Preserve already-saved scans in the edit buffers before adding new uploads.
    if value.scans_in_edit.is_empty() && !value.scans.is_empty() {
        value.scans_in_edit = value
            .scans
            .iter()
            .cloned()
            .map(|fields| EditFile {
                file_type: FileType::Scan,
                fields,
                upload_data: None,
                deleted: false,
            })
            .collect();
    }
    if value.translations_in_edit.is_empty() && !value.translations.is_empty() {
        value.translations_in_edit = value
            .translations
            .iter()
            .cloned()
            .map(|fields| EditFile {
                file_type: FileType::Translation,
                fields,
                upload_data: None,
                deleted: false,
            })
            .collect();
    }
    if value.special_scans_in_edit.is_empty() && !value.special_scans.is_empty() {
        value.special_scans_in_edit = value
            .special_scans
            .iter()
            .map(|(&ft, fields)| {
                (
                    ft,
                    EditFile {
                        file_type: ft,
                        fields: fields.clone(),
                        upload_data: None,
                        deleted: false,
                    },
                )
            })
            .collect();
    }

    match edit_file.file_type {
        FileType::FrontSide | FileType::ReverseSide | FileType::Selfie => {
            value
                .special_scans_in_edit
                .insert(edit_file.file_type, edit_file);
        }
        FileType::Scan => {
            value.scans_in_edit.push(edit_file);
        }
        FileType::Translation => {
            value.translations_in_edit.push(edit_file);
        }
    }
}

/// Parses an optional `"<slot>:<file_path>"` or bare file path string passed in `scan_labels`.
///
/// Supported prefixes: `"front:"`, `"reverse:"`, `"selfie:"`, `"translation:"`, `"scan:"`.
/// Returns `None` for purely symbolic demo labels (like `"Front"` or `"Scan Page 1"`) that
/// do not point to a file on disk.
#[must_use]
pub fn parse_scan_upload_spec(label: &str, value_type: ValueType) -> Option<(FileType, PathBuf)> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    let prefixes = [
        ("front:", FileType::FrontSide),
        ("front_side:", FileType::FrontSide),
        ("reverse:", FileType::ReverseSide),
        ("reverse_side:", FileType::ReverseSide),
        ("selfie:", FileType::Selfie),
        ("translation:", FileType::Translation),
        ("scan:", FileType::Scan),
    ];
    for (prefix, ft) in prefixes {
        if lowered.starts_with(prefix) {
            let path_str = trimmed[prefix.len()..].trim();
            if !path_str.is_empty() {
                return Some((ft, PathBuf::from(path_str)));
            }
            return None;
        }
    }
    let candidate = PathBuf::from(trimmed);
    if candidate.is_file() {
        let default_ft = match value_type {
            ValueType::Passport
            | ValueType::DriverLicense
            | ValueType::IdentityCard
            | ValueType::InternalPassport => FileType::FrontSide,
            _ => FileType::Scan,
        };
        return Some((default_ft, candidate));
    }
    None
}
