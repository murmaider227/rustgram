//! TGB-2946: a native toast shows the sender's loaded photo userpic
//! (`CachedUserpics::get(key, peer, view)` with a loaded `userpicCloudImage`), and the empty
//! userpic until the photo is loaded.
//!
//! With `$TGB2946_SNAP_DIR` set, the two toast images are saved there.

use std::sync::Arc;

use tg_app_core::stand_in::types::MsgId;
use tg_ui::userpic_images::UserpicImage;

use crate::notifications::backend::IncomingMessageNotification;
use crate::notifications::manager::{
    MINIMAL_DELAY_MS, ManagerType, NotificationSystem, SkipDecision, WindowFocusState,
};
use crate::notifications::mock::MockNotifyBackend;
use crate::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use crate::notifications::utilities::{NOTIFY_USERPIC_SIZE, UserpicKey, userpic_from_photo};

const MAGENTA: [u8; 3] = [0xE0, 0x10, 0xC0];
const PHOTO_ID: i64 = 29_460_001;

fn photo() -> Arc<UserpicImage> {
    let side = 96;
    let mut rgba = Vec::with_capacity(side * side * 4);
    for _ in 0..side * side {
        rgba.extend_from_slice(&[MAGENTA[0], MAGENTA[1], MAGENTA[2], 0xFF]);
    }
    Arc::new(UserpicImage {
        photo_id: PHOTO_ID,
        width: side,
        height: side,
        rgba,
    })
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rustgram_tgb2946_{tag}_{}", std::process::id()))
}

/// Shows one message notification through the native manager and returns the toast image.
fn toast_image(with_photo: bool, tag: &str) -> image::RgbaImage {
    let dir = temp_dir(tag);
    let mut sys = NotificationSystem::headless(
        NotificationPreferences::default(),
        NotifySettingsStore::default(),
        dir.clone(),
    );
    sys.set_manager_type(ManagerType::Native, 0);
    let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
    if with_photo {
        let photo = photo();
        sys.set_userpic_photo_lookup(Some(Arc::new(move |peer_id| {
            (peer_id == peer.0).then(|| Arc::clone(&photo))
        })));
    }
    let mut backend = MockNotifyBackend::new();
    let focus = WindowFocusState::with_defaults(1);
    let now_unix = 1_700_000_000;
    let notification = IncomingMessageNotification {
        session_id: 1,
        peer_id: peer,
        sender_id: peer,
        sender_name: "Support Desk".to_owned(),
        chat_title: "Support Desk".to_owned(),
        msg_id: MsgId(11),
        text: "The build is green".to_owned(),
        ..IncomingMessageNotification::default()
    };
    assert_eq!(
        sys.schedule(notification, &focus, 1_000, now_unix),
        SkipDecision::Notify { silent: false }
    );
    sys.tick(&mut backend, &focus, 1_000 + MINIMAL_DELAY_MS, now_unix);
    let shown = sys.dispatched_notifications();
    assert_eq!(shown.len(), 1);
    let path = shown[0]
        .image_path
        .clone()
        .expect("a native toast has an image");
    let image = image::open(&path).expect("toast png").to_rgba8();
    if let Some(dir) = std::env::var_os("TGB2946_SNAP_DIR") {
        let _ = image.save(std::path::PathBuf::from(dir).join(format!("tgb2946_toast_{tag}.png")));
    }
    let _ = std::fs::remove_dir_all(&dir);
    image
}

fn is_magenta(px: &image::Rgba<u8>) -> bool {
    px.0[..3]
        .iter()
        .zip(MAGENTA)
        .all(|(&a, b)| (i32::from(a) - i32::from(b)).abs() <= 3)
}

#[test]
fn a_native_toast_shows_the_loaded_photo() {
    let with_photo = toast_image(true, "photo");
    let c = with_photo.width() / 2;
    assert!(is_magenta(with_photo.get_pixel(c, c)), "the photo");
    assert_eq!(with_photo.get_pixel(0, 0).0[3], 0, "masked to a circle");

    let empty = toast_image(false, "empty");
    assert!(
        !is_magenta(empty.get_pixel(c, c)),
        "no photo: the empty userpic"
    );
}

#[test]
fn the_photo_key_is_new_per_photo_and_never_a_colour_key() {
    let peer = 42;
    let photo_key = UserpicKey::for_peer_photo(peer, PHOTO_ID);
    assert_ne!(photo_key, UserpicKey::for_peer_photo(peer, PHOTO_ID + 1));
    assert_ne!(photo_key, UserpicKey::for_peer(peer, PHOTO_ID as u64));
    assert_ne!(photo_key, UserpicKey::HIDDEN_PLACEHOLDER);
}

#[test]
fn the_photo_image_is_straight_alpha() {
    let image = userpic_from_photo(&photo(), NOTIFY_USERPIC_SIZE);
    let side = NOTIFY_USERPIC_SIZE as usize;
    // An edge pixel: partly covered, but its colour stays the photo's colour.
    let row = side / 2;
    let edge = (0..side)
        .map(|x| &image.pixels[(row * side + x) * 4..(row * side + x) * 4 + 4])
        .find(|px| px[3] > 0 && px[3] < 255)
        .expect("an antialiased edge pixel");
    for (got, want) in edge[..3].iter().zip(MAGENTA) {
        assert!(
            (i32::from(*got) - i32::from(want)).abs() <= 3,
            "edge {edge:?} is not premultiplied"
        );
    }
}
