//! Name colours against values computed with tdesktop's formulas (TGB-3012).

use egui::Color32;

use super::*;
use crate::theme::DayPalette;

/// `historyPeerNNameFg` of `colors.palette`, by palette slot.
const DAY_NAMES: [u32; 8] = [
    0xc0_3d_33, 0x4f_ad_2d, 0xd0_93_06, 0x16_8a_cd, 0x85_44_d6, 0xcd_40_73, 0x29_96_ad, 0xce_67_1b,
];

/// Colour index -> palette slot (`ColorIndexToPaletteIndex`: `{ 0, 7, 4, 1, 6, 3, 5 }`).
const SLOT: [usize; 7] = [0, 7, 4, 1, 6, 3, 5];

#[test]
fn simple_indexes_use_the_palette_slot_of_color_index_to_palette_index() {
    let palette = DayPalette::new();
    for index in 0..7u8 {
        let accent = PeerAccent {
            index,
            collectible: None,
        };
        assert_eq!(
            peer_accent_name_color(accent, &palette, false),
            color_from_rgb(DAY_NAMES[SLOT[usize::from(index)]]),
            "index {index}"
        );
    }
}

#[test]
fn default_accent_is_the_bare_id_mod_7() {
    // `Data::DecideColorIndex(peerId)` masks the type bits, then `id % 7`.
    assert_eq!(PeerAccent::for_peer_id(1_234_567_890).index, 3);
    assert_eq!(PeerAccent::for_peer_id(777_000).index, 0);
    assert_eq!(PeerAccent::for_peer_id(1_087_968_824).index, 5);
}

#[test]
fn server_index_wins_over_the_id_based_one() {
    // A sender whose id gives red (index 0) but whose server colour is 5 (slot 3, blue) is drawn blue.
    let palette = DayPalette::new();
    let accent = PeerAccent {
        index: 5,
        collectible: None,
    };
    assert_eq!(PeerAccent::for_peer_id(777_000).index, 0);
    assert_eq!(
        peer_accent_name_color(accent, &palette, false),
        Color32::from_rgb(0x16, 0x8a, 0xcd)
    );
}

#[test]
fn collectible_accent_replaces_the_index_and_follows_the_theme() {
    let palette = DayPalette::new();
    let accent = PeerAccent {
        index: 2,
        collectible: Some(CollectibleAccent {
            day: 0x12_34_56,
            night: 0xab_cd_ef,
        }),
    };
    assert_eq!(
        peer_accent_name_color(accent, &palette, false),
        Color32::from_rgb(0x12, 0x34, 0x56)
    );
    assert_eq!(
        peer_accent_name_color(accent, &palette, true),
        Color32::from_rgb(0xab, 0xcd, 0xef)
    );
}

/// The only test that touches the process-wide table, so parallel tests cannot see it half set.
#[test]
fn extended_indexes_use_the_server_colour_set_then_fall_back_to_mod_7() {
    let palette = DayPalette::new();
    let blue_red = PeerAccent {
        index: 10,
        collectible: None,
    };
    // No colour sets yet: `colorIndex %= kSimpleColorIndexCount` -> 3 -> slot 1 (green).
    clear_color_indices();
    assert_eq!(
        peer_accent_name_color(blue_red, &palette, false),
        color_from_rgb(DAY_NAMES[1])
    );

    let mut table: ColorIndices = [ColorIndexData::default(); COLOR_INDEX_COUNT as usize];
    table[10] = ColorIndexData {
        light: [Some(0x3e_88_f7), Some(0xff_32_a0), None],
        dark: [Some(0x52_bf_ff), Some(0xff_59_7b), None],
    };
    assert!(set_color_indices(table));
    assert!(!set_color_indices(table), "same sets do not repaint");
    assert_eq!(
        peer_accent_name_color(blue_red, &palette, false),
        Color32::from_rgb(0x3e, 0x88, 0xf7)
    );
    assert_eq!(
        peer_accent_name_color(blue_red, &palette, true),
        Color32::from_rgb(0x52, 0xbf, 0xff)
    );
    // An index with no set (`!colors[0]`) falls back to `index % 7`: 13 % 7 = 6 -> slot 5.
    let unknown = PeerAccent {
        index: 13,
        collectible: None,
    };
    assert_eq!(
        peer_accent_name_color(unknown, &palette, false),
        color_from_rgb(DAY_NAMES[5])
    );
    clear_color_indices();
}
