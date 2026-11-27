//! UIP4-C: the profile Mute button's right-click menu (`MuteMenu::SetupMuteMenu`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app_core::stand_in::theme::ThemeMode;

use crate::info::backend::InfoBackend;
use crate::info::colors::InfoColors;
use crate::info::mock::{MockBackend, mock_peers};
use crate::info::mute_menu::{MUTE_FOREVER_OR_UNMUTE, SOUND_TOGGLE, mute_menu_items};
use crate::info::wrap_widget::{InfoWrapEvent, InfoWrapWidget};
use tg_ui::widgets::menu::MenuItem;

struct State {
    wrap: InfoWrapWidget,
    backend: MockBackend,
    events: Vec<InfoWrapEvent>,
}

fn harness() -> Harness<'static, State> {
    let colors = InfoColors::for_mode(ThemeMode::Day);
    let mut backend = MockBackend::new();
    if backend
        .peer_profile(mock_peers::USER_MIRA)
        .is_some_and(|p| p.muted)
    {
        backend.toggle_notifications(mock_peers::USER_MIRA);
    }
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(420.0, 800.0))
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut State| {
                let events = st.wrap.show(ui, &mut st.backend, true, &colors);
                st.events.extend(events);
            },
            State {
                wrap: InfoWrapWidget::for_peer(mock_peers::USER_MIRA),
                backend,
                events: Vec::new(),
            },
        );
    harness.run();
    harness
}

fn labels(items: &[MenuItem]) -> Vec<(String, String, bool)> {
    items
        .iter()
        .filter_map(|it| match it {
            MenuItem::Action {
                id,
                label,
                attention,
                ..
            } => Some((id.clone(), label.clone(), *attention)),
            _ => None,
        })
        .collect()
}

#[test]
fn mute_menu_rows_follow_fill_mute_menu() {
    let rows = labels(&mute_menu_items(false, false));
    let names: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(
        names,
        [
            "Select tone",
            "Disable sound",
            "Mute for...",
            "Mute forever"
        ]
    );
    assert!(rows[3].2, "Mute forever is red (menuIconAttentionColor)");

    let rows = labels(&mute_menu_items(true, true));
    assert_eq!(rows[1].0, SOUND_TOGGLE);
    assert_eq!(rows[1].1, "Enable sound");
    assert_eq!(rows[3].0, MUTE_FOREVER_OR_UNMUTE);
    assert_eq!(rows[3].1, "Unmute");
    assert!(!rows[3].2);
}

#[test]
fn right_click_on_mute_opens_the_menu_and_left_click_toggles() {
    let mut h = harness();
    let mute = h.get_by_label("Mute").rect().center();

    h.input_mut().events.push(egui::Event::PointerMoved(mute));
    h.run();
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: mute,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
    assert!(h.state().wrap.is_mute_menu_open());
    assert!(
        !h.state()
            .backend
            .peer_profile(mock_peers::USER_MIRA)
            .is_some_and(|p| p.muted),
        "a right click only opens the menu"
    );

    h.get_by_label("Mute for...").click();
    h.run();
    assert!(
        h.state()
            .events
            .contains(&InfoWrapEvent::OpenMuteFor(mock_peers::USER_MIRA))
    );
    assert!(!h.state().wrap.is_mute_menu_open());

    // Left click: `MuteMenu::ToggleMuteForever`.
    h.get_by_label("Mute").click();
    h.run();
    assert!(
        h.state()
            .backend
            .peer_profile(mock_peers::USER_MIRA)
            .is_some_and(|p| p.muted)
    );
}

#[test]
fn sound_toggle_is_handed_to_the_host() {
    let mut h = harness();
    h.state_mut()
        .wrap
        .set_notify_sound_silent(mock_peers::USER_MIRA, true);
    let mute = h.get_by_label("Mute").rect().center();
    h.input_mut().events.push(egui::Event::PointerMoved(mute));
    h.run();
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: mute,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
    h.get_by_label("Enable sound").click();
    h.run();
    assert_eq!(
        h.state_mut().wrap.take_sound_toggle_request(),
        Some((mock_peers::USER_MIRA, true))
    );
    assert_eq!(h.state_mut().wrap.take_sound_toggle_request(), None);
}

/// Peer id of [`hidden_phone_backend`]'s user (no mock extras are keyed to it).
const HIDDEN_USER: tg_app_core::stand_in::types::PeerId =
    tg_app_core::stand_in::types::PeerId::from_user(990_066);

/// A user with no public phone, username or bio, who is not a contact, with one photo and one
/// file: the stage-3 test account's profile.
fn hidden_phone_backend() -> crate::info::SinglePeerFallback {
    let inner = MockBackend::new();
    let mut profile = inner
        .peer_profile(mock_peers::USER_MIRA)
        .expect("mock user");
    profile.id = HIDDEN_USER;
    profile.title = "Sample User".to_owned();
    profile.color_index = 5;
    profile.auto_delete_ttl_seconds = 0;
    profile.phone = None;
    profile.usernames.clear();
    profile.bio.clear();
    profile.birthday = None;
    profile.verified = false;
    profile.premium = false;
    profile.is_online = false;
    profile.status = "last seen recently".to_owned();
    profile.is_contact = false;
    profile.muted = false;
    profile.media_counts = crate::info::SharedMediaCounts {
        photos_videos: 1,
        files: 1,
        ..crate::info::SharedMediaCounts::default()
    };
    crate::info::SinglePeerFallback { profile, inner }
}

struct CaptureState {
    wrap: InfoWrapWidget,
    backend: crate::info::SinglePeerFallback,
}

/// Writes `profile_hidden_{day,night}.png` (the side-column profile of a user with nothing
/// public) and `mute_menu_{day,night}.png` (the same with the Mute menu open) under
/// `RUSTGRAM_SNAPSHOT_DIR` (or the temp dir). The harness frame adds 8 px on every side, so the
/// column itself is 292 px wide like tdesktop's third column.
#[test]
#[ignore = "captures the profile column under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_profile_column() {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip4c"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (mode, name) in [(ThemeMode::Day, "day"), (ThemeMode::Night, "night")] {
        let colors = InfoColors::for_mode(mode);
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(308.0, 576.0))
            .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
            .wgpu()
            .build_ui_state(
                move |ui, st: &mut CaptureState| {
                    let _ = st.wrap.show(ui, &mut st.backend, true, &colors);
                },
                CaptureState {
                    wrap: InfoWrapWidget::for_peer(HIDDEN_USER),
                    backend: hidden_phone_backend(),
                },
            );
        for _ in 0..10 {
            h.step();
        }
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("profile_hidden_{name}.png")))
            .expect("save png");

        let mute = h.get_by_label("Mute").rect().center();
        h.input_mut().events.push(egui::Event::PointerMoved(mute));
        h.run();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: mute,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.run();
        }
        for _ in 0..30 {
            h.step();
        }
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("mute_menu_{name}.png")))
            .expect("save png");
    }
}

fn gray_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::One);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().expect("png header");
        let row = vec![0u8; (width as usize).div_ceil(8)];
        let mut stream = writer.stream_writer().expect("png stream");
        use std::io::Write as _;
        for _ in 0..height {
            stream.write_all(&row).expect("png row");
        }
        stream.finish().expect("png finish");
    }
    out
}

#[test]
fn shared_media_thumbs_decode_bounded() {
    use crate::info::media::{
        SHARED_MEDIA_THUMB_MAX_BYTES, SHARED_MEDIA_THUMB_MAX_SIDE, decode_shared_media_thumb,
    };

    let small = decode_shared_media_thumb(&gray_png(90, 60)).expect("small thumb decodes");
    assert_eq!(small.dimensions(), (90, 60));

    // A large but allowed photo is scaled down for the grid, keeping its aspect ratio.
    let big = decode_shared_media_thumb(&gray_png(1280, 640)).expect("1280x640 decodes");
    assert_eq!(big.dimensions(), (320, 160));

    // Rejected from the header, before pixels are allocated: one side over 2560 px, and a
    // ~20 KiB 1-bit PNG that would expand to 121 megapixels.
    assert!(decode_shared_media_thumb(&gray_png(SHARED_MEDIA_THUMB_MAX_SIDE + 1, 8)).is_none());
    assert!(decode_shared_media_thumb(&gray_png(11_000, 11_000)).is_none());

    // Oversized input, empty input and garbage.
    let mut huge = gray_png(16, 16);
    huge.resize(SHARED_MEDIA_THUMB_MAX_BYTES + 1, 0);
    assert!(decode_shared_media_thumb(&huge).is_none());
    assert!(decode_shared_media_thumb(&[]).is_none());
    assert!(decode_shared_media_thumb(b"not an image at all").is_none());
}

/// TGB-2875: a left click on Mute toggles the chat like `MuteMenu::ToggleMuteForever` and
/// shows no toast (tdesktop only flips the button).
#[test]
fn mute_click_reports_the_toggle_without_a_toast() {
    let mut h = harness();
    h.get_by_label("Mute").click();
    h.run();
    let events = &h.state().events;
    assert!(
        events.contains(&InfoWrapEvent::NotificationsToggled {
            peer_id: mock_peers::USER_MIRA,
            muted: true,
        }),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, InfoWrapEvent::Toast(_))),
        "no toast: {events:?}"
    );
    assert_eq!(
        crate::info::map_wrap_events(events, mock_peers::USER_MIRA),
        Some(crate::info::InfoAction::ToggleMute(mock_peers::USER_MIRA))
    );
}

/// TGB-2874: `MuteItem` paints "Unmute" in `boxTextFgGood` and "Mute forever" in
/// `menuIconAttentionColor`.
#[test]
fn unmute_row_is_green_and_mute_forever_is_red() {
    let good = |items: &[MenuItem]| -> Vec<bool> {
        items
            .iter()
            .filter_map(|it| match it {
                MenuItem::Action { good, .. } => Some(*good),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        good(&mute_menu_items(true, false)),
        [false, false, false, true]
    );
    assert_eq!(
        good(&mute_menu_items(false, false)),
        [false, false, false, false]
    );
    let colors = InfoColors::for_mode(ThemeMode::Day);
    let style = tg_ui::style::PopupMenuStyle::default_popup(&colors);
    assert_eq!(
        style.menu.item_fg_good,
        tg_ui::theme::Colors::color(&colors, "boxTextFgGood")
    );
    assert_ne!(style.menu.item_fg_good, style.menu.item_fg);
}

/// Wave 8 capture (TGB-2874, TGB-2875): the profile column of a muted chat with its Mute menu
/// open, written to `$W8_SNAP_DIR/<stage>/unmute_menu_<mode>.png` (`<stage>` from
/// `$W8_SNAP_STAGE`, default `after`).
#[test]
#[ignore = "captures the mute menu under W8_SNAP_DIR"]
fn capture_w8_unmute_menu() {
    let Some(dir) = std::env::var_os("W8_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let stage = std::env::var("W8_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = dir.join(stage);
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (mode, name) in [(ThemeMode::Day, "day"), (ThemeMode::Night, "night")] {
        let colors = InfoColors::for_mode(mode);
        let mut backend = MockBackend::new();
        if !backend
            .peer_profile(mock_peers::USER_MIRA)
            .is_some_and(|p| p.muted)
        {
            backend.toggle_notifications(mock_peers::USER_MIRA);
        }
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(420.0, 640.0))
            .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
            .wgpu()
            .build_ui_state(
                move |ui, st: &mut State| {
                    let events = st.wrap.show(ui, &mut st.backend, true, &colors);
                    st.events.extend(events);
                },
                State {
                    wrap: InfoWrapWidget::for_peer(mock_peers::USER_MIRA),
                    backend,
                    events: Vec::new(),
                },
            );
        for _ in 0..10 {
            h.step();
        }
        let mute = h.get_by_label("Unmute").rect().center();
        h.input_mut().events.push(egui::Event::PointerMoved(mute));
        h.run();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: mute,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.run();
        }
        for _ in 0..30 {
            h.step();
        }
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("unmute_menu_{name}.png")))
            .expect("save png");
    }
}
