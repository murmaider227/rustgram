//! The cached [`Palette::from_theme_manager`] always equals a fresh build (TGB-2893, package F).

use tg_theme::{Color as ThemeColor, EmbeddedType};
use tg_ui::theme::ThemeManager;

use super::Palette;

fn fresh(tm: &ThemeManager) -> Palette {
    Palette::from_colors(tm, tm.is_dark())
}

#[test]
fn cached_palette_follows_every_theme_change() {
    let mut tm = ThemeManager::new();
    assert_eq!(Palette::from_theme_manager(&tm), fresh(&tm));
    let day = Palette::from_theme_manager(&tm);

    tm.set_night_mode(true);
    let night = Palette::from_theme_manager(&tm);
    assert_eq!(night, fresh(&tm));
    assert_ne!(night.window_bg, day.window_bg);

    tm.set_accent(Some(ThemeColor::rgba(0xd0, 0x40, 0x40, 0xff)));
    assert_eq!(Palette::from_theme_manager(&tm), fresh(&tm));

    tm.set_embedded(EmbeddedType::DayBlue, None);
    assert_eq!(Palette::from_theme_manager(&tm), fresh(&tm));
}

#[test]
fn two_managers_never_share_a_cached_palette() {
    let day = ThemeManager::with_embedded(EmbeddedType::Default);
    let night = ThemeManager::with_embedded(EmbeddedType::Night);
    // Same generation number, different managers.
    assert_eq!(day.generation(), night.generation());
    for _ in 0..3 {
        assert_eq!(Palette::from_theme_manager(&day), fresh(&day));
        assert_eq!(Palette::from_theme_manager(&night), fresh(&night));
    }
}

#[test]
fn typed_picks_match_the_name_lookups() {
    use tg_ui::theme::{Colors, PaletteColor};
    for kind in [
        EmbeddedType::Default,
        EmbeddedType::Night,
        EmbeddedType::NightGreen,
    ] {
        let tm = ThemeManager::with_embedded(kind);
        for row in tg_theme::Palette::ROWS {
            let typed = tg_theme::Palette::color_index(row.name).expect("slot");
            assert_eq!(tm.get_at(typed), tm.get(row.name), "{}", row.name);
        }
        assert_eq!(tm.get_at(PaletteColor::TRANSPARENT), tm.get("transparent"));
        assert_eq!(tm.get_at(PaletteColor::WHITE), tm.get("white"));
    }
}
