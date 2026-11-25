//! Security regression tests for `SEC-A` in `tg-app-media` (`overlay.rs` & `mock.rs`).
//!
//! Covers:
//! - A2: Remote filenames with `..\`, `C:\...`, and collisions stay inside `save_directory`
//!   under safe unique names without overwriting existing files.
//! - A3: Opening an executable (`x.exe`) or extensionless (`README`) item from the media overlay
//!   emits `OverlayEvent::ConfirmOpenExternal` instead of calling `open_in_external_app` directly.
//! - A4: `MediaOverlayWidget` rejects images whose dimensions exceed `tg_media::image::READ_MAX_AREA`
//!   without attempting full pixel-buffer allocation.

use std::io::Write;
use std::path::PathBuf;

use tg_text::TextWithEntities;

use crate::media::backend::{MediaBackend, MediaItem, MediaItemId, MediaItemKind};
use crate::media::mock::{MockActionLog, MockMediaBackend};
use crate::media::overlay::{MediaOverlayWidget, OverlayEvent};

fn make_item(
    id: MediaItemId,
    kind: MediaItemKind,
    file_name: &str,
    mime_type: &str,
    size: u64,
    local_path: Option<PathBuf>,
) -> MediaItem {
    MediaItem {
        id,
        peer_id: 1,
        msg_id: id.0 as i32,
        kind,
        width: 100,
        height: 100,
        duration_ms: 0,
        file_size: size,
        file_name: file_name.to_owned(),
        mime_type: mime_type.to_owned(),
        sender_name: "Test".to_owned(),
        date_text: "Today".to_owned(),
        caption: TextWithEntities::default(),
        rotation_degrees: 0,
        has_alpha: false,
        allows_forward: true,
        allows_delete: true,
        allows_save: true,
        local_path,
        grouped_id: None,
        attached_stickers_media: None,
    }
}

/// Builds a minimal syntactically-valid PNG header (signature + IHDR + IEND) claiming `(width, height)`.
///
/// Only ~67 bytes on disk, so an unchecked decoder attempting to allocate `width * height * 4`
/// bytes would OOM, whereas `tg_media::image::read_image` rejects it immediately from the IHDR.
fn make_oversized_png_header(width: u32, height: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &[0x00]);
    push_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn seca_a4_overlay_rejects_image_exceeding_read_max_area() {
    let backend = MockMediaBackend::new();
    let bomb_id = MediaItemId(990_004);
    let bomb_png = make_oversized_png_header(20_000, 20_000); // 400M px > 108.5M px limit
    let mut item = make_item(
        bomb_id,
        MediaItemKind::Photo,
        "bomb.png",
        "image/png",
        bomb_png.len() as u64,
        None,
    );
    item.width = 20_000;
    item.height = 20_000;
    backend.insert_item(item, bomb_png);

    let mut overlay = MediaOverlayWidget::new();
    overlay
        .open_item(&backend, 1, bomb_id, 1_000)
        .expect("open_item should record decode error rather than panic");
    let err = overlay
        .error_message()
        .expect("oversized photo must fail with area-limit error");
    assert!(
        err.contains("area limit") || err.contains("exceed"),
        "expected READ_MAX_AREA error, got: {err}"
    );
}

#[test]
fn seca_a2_overlay_save_blocks_traversal_drive_override_and_overwrites() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let save_dir = tmp.path().join("downloads");
    std::fs::create_dir_all(&save_dir).expect("mkdir");
    let canon_save_dir = std::fs::canonicalize(&save_dir).expect("canon");

    // Pre-create `x.bat` inside save_dir to prove we never overwrite existing files.
    let existing = save_dir.join("x.bat");
    {
        let mut f = std::fs::File::create(&existing).expect("create existing");
        f.write_all(b"ORIGINAL").expect("write");
    }

    let backend = MockMediaBackend::new();
    let bat_id = MediaItemId(990_001);
    let dll_id = MediaItemId(990_002);

    backend.insert_item(
        make_item(
            bat_id,
            MediaItemKind::Document,
            r"..\..\Startup\x.bat",
            "application/octet-stream",
            7,
            None,
        ),
        b"PAYLOAD".to_vec(),
    );
    backend.insert_item(
        make_item(
            dll_id,
            MediaItemKind::Document,
            r"C:\Windows\x.dll",
            "application/octet-stream",
            6,
            None,
        ),
        b"DLLBIN".to_vec(),
    );

    let mut overlay = MediaOverlayWidget::new();
    overlay.set_save_directory(save_dir.clone());

    let mut events = Vec::new();
    overlay
        .open_item(&backend, 1, bat_id, 1_000)
        .expect("open bat");
    overlay.save_current_item(&backend, 1_000, &mut events);

    let saved_bat = events
        .iter()
        .find_map(|e| match e {
            OverlayEvent::Saved(p) => Some(p.clone()),
            _ => None,
        })
        .expect("must emit OverlayEvent::Saved");
    assert_eq!(saved_bat.parent(), Some(canon_save_dir.as_path()));
    assert_eq!(
        saved_bat.file_name().and_then(|n| n.to_str()),
        Some("x (1).bat"),
        "existing x.bat must not be overwritten"
    );
    assert_eq!(
        std::fs::read(&existing).expect("read original"),
        b"ORIGINAL"
    );
    assert_eq!(std::fs::read(&saved_bat).expect("read saved"), b"PAYLOAD");
    assert!(
        !tmp.path().join("Startup").exists(),
        "must never create Startup outside save_dir"
    );

    events.clear();
    overlay
        .open_item(&backend, 1, dll_id, 1_100)
        .expect("open dll");
    overlay.save_current_item(&backend, 1_100, &mut events);
    let saved_dll = events
        .iter()
        .find_map(|e| match e {
            OverlayEvent::Saved(p) => Some(p.clone()),
            _ => None,
        })
        .expect("must emit OverlayEvent::Saved for dll");
    assert_eq!(saved_dll.parent(), Some(canon_save_dir.as_path()));
    assert_eq!(
        saved_dll.file_name().and_then(|n| n.to_str()),
        Some("x.dll")
    );

    // Direct save_to_disk with `..` traversal must also be refused.
    assert!(
        backend
            .save_to_disk(bat_id, &save_dir.join(r"..\escaped.bat"))
            .is_err()
    );
}

#[test]
fn seca_a3_overlay_open_external_asks_confirmation_for_exe_and_extensionless() {
    let backend = MockMediaBackend::new();
    let exe_id = MediaItemId(990_010);
    let noext_id = MediaItemId(990_011);
    let pdf_id = MediaItemId(990_012);

    for (id, name) in [
        (exe_id, "x.exe"),
        (noext_id, "README"),
        (pdf_id, "manual.pdf"),
    ] {
        backend.insert_item(
            make_item(
                id,
                MediaItemKind::Document,
                name,
                "application/octet-stream",
                4,
                Some(PathBuf::from(name)),
            ),
            b"TEST".to_vec(),
        );
    }

    let mut overlay = MediaOverlayWidget::new();

    // 1. Executable `x.exe`: must NOT launch immediately, must emit ConfirmOpenExternal.
    overlay.open_item(&backend, 1, exe_id, 1_000).unwrap();
    let mut events = Vec::new();
    overlay.request_open_external_current(&backend, 1_000, &mut events);
    assert!(
        events.iter().any(|e| matches!(
            e,
            OverlayEvent::ConfirmOpenExternal { item_id, file_name }
                if *item_id == exe_id && file_name == "x.exe"
        )),
        "x.exe must emit ConfirmOpenExternal, got: {events:?}"
    );
    assert!(
        !backend
            .action_log()
            .iter()
            .any(|a| matches!(a, MockActionLog::OpenedExternal(id, _) if *id == exe_id)),
        "x.exe must NOT call open_in_external_app before confirmation"
    );

    // 2. Extensionless `README`: must NOT launch immediately, must emit ConfirmOpenExternal.
    overlay.open_item(&backend, 1, noext_id, 1_100).unwrap();
    events.clear();
    overlay.request_open_external_current(&backend, 1_100, &mut events);
    assert!(
        events.iter().any(|e| matches!(
            e,
            OverlayEvent::ConfirmOpenExternal { item_id, file_name }
                if *item_id == noext_id && file_name == "README"
        )),
        "extensionless file must emit ConfirmOpenExternal, got: {events:?}"
    );

    // 3. Benign `manual.pdf`: opens directly.
    overlay.open_item(&backend, 1, pdf_id, 1_200).unwrap();
    events.clear();
    overlay.request_open_external_current(&backend, 1_200, &mut events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OverlayEvent::OpenExternal(_))),
        "benign PDF should open directly, got: {events:?}"
    );
}
