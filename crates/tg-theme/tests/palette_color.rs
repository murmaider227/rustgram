//! Typed palette colours (`PaletteColor`) agree with the name lookup for every slot.

use tg_theme::{EmbeddedType, Palette, PaletteColor, prepare_embedded_palette};

#[test]
fn every_named_colour_has_a_typed_slot_with_the_same_value() {
    let palettes = [
        Palette::default(),
        prepare_embedded_palette(EmbeddedType::Night, None).expect("night palette"),
    ];
    for palette in &palettes {
        for row in Palette::ROWS {
            let typed = Palette::color_index(row.name).expect("typed slot");
            assert_eq!(typed.index(), row.index);
            assert_eq!(typed.name(), row.name);
            assert_eq!(PaletteColor::from_index(row.index), Some(typed));
            assert_eq!(
                Some(palette.at(typed)),
                palette.color_by_name(row.name),
                "{}",
                row.name
            );
        }
        assert_eq!(palette.at(PaletteColor::WHITE), palette.white);
        assert_eq!(palette.at(PaletteColor::TRANSPARENT), palette.transparent);
    }
}

#[test]
fn constants_name_their_colours() {
    assert_eq!(PaletteColor::WINDOW_BG.name(), "windowBg");
    assert_eq!(
        PaletteColor::DIALOGS_UNREAD_BG_MUTED.name(),
        "dialogsUnreadBgMuted"
    );
    assert_eq!(
        Palette::color_index("transparent"),
        Some(PaletteColor::TRANSPARENT)
    );
    assert_eq!(Palette::color_index("white"), Some(PaletteColor::WHITE));
    assert_eq!(Palette::color_index("noSuchColour"), None);
    assert_eq!(PaletteColor::from_index(Palette::COUNT), None);
}
