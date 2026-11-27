//! Panic fuzz over theme files, palettes, cloud theme text and wallpaper links (TGB-3065).

use tg_fuzz::Fuzz;

use crate::colorizer::Colorizer;
use crate::palette::Palette;
use crate::theme_file::{self, CloudTheme, ParsedTheme, ThemeCache};
use crate::wallpaper::{self, WallPaper};

const ITERATIONS: usize = 3_000;

const PALETTE_TOKENS: &[&str] = &[
    "windowBg",
    "windowFg",
    "dialogsBg",
    ": ",
    ";",
    "#",
    "#ff00ff",
    "#ff00ff80",
    "#fff",
    "// c\n",
    "/* c */",
    "/*",
    "*/",
    "windowBg: #112233;\n",
    "historyTextInFg: windowFg;\n",
    "\u{feff}",
];

const URL_TOKENS: &[&str] = &[
    "tg://bg?",
    "https://t.me/bg/",
    "slug=",
    "color=",
    "gradient=",
    "rotation=",
    "intensity=",
    "mode=",
    "blur",
    "motion",
    "ff00ff",
    "ff00ff-00ff00",
    "~",
    "-",
    "&",
    "?",
    "=",
];

fn seeds() -> Vec<Vec<u8>> {
    let palette = theme_file::generate_default_palette();
    let zip = theme_file::pack_theme(&ParsedTheme {
        palette: palette.clone(),
        ..ParsedTheme::default()
    })
    .unwrap_or_default();
    let cloud = theme_file::write_cloud_to_text(&CloudTheme {
        id: 1,
        access_hash: 2,
        slug: "slug".to_string(),
        ..CloudTheme::default()
    });
    let mut paper = WallPaper::construct_default();
    paper.slug = "slug".to_string();
    vec![palette, zip, cloud, paper.serialize()]
}

#[test]
fn theme_and_palette_bytes_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7E3E_0001, ITERATIONS);
    let seeds = seeds();
    let colorizer = Colorizer::default();
    for i in 0..fuzz.iterations() {
        let data = match fuzz.rng().below(3) {
            0 => {
                let seed = fuzz.rng().below(seeds.len());
                fuzz.mutate(&seeds[seed])
            }
            1 => fuzz.text(PALETTE_TOKENS, 40).into_bytes(),
            _ => fuzz.bytes(256),
        };
        fuzz.check("palette and theme parsers", i, &data, || {
            let _ = theme_file::strip_comments(&data);
            let _ = theme_file::read_palette_values(&data, |_, _| true);
            let mut palette = Palette::default();
            let _ = theme_file::load_color_scheme(&data, &colorizer, &mut palette);
            let _ = theme_file::read_palette_content_from_bytes(&data);
            let _ = theme_file::parse_theme(&data, false, Some(&colorizer));
            let _ = theme_file::load_theme_from_bytes(&data, &colorizer, Some(&data), true);
            let _ = theme_file::load_from_cache(&data, &ThemeCache::default());
            let _ = theme_file::colorize_in_content(&data, &colorizer);
            let _ = theme_file::replace_adjustable_colors(&data, &palette);
            let _ = theme_file::read_cloud_from_text(&data);
            let _ = theme_file::strip_cloud_text_fields(&data);
            let _ = WallPaper::from_serialized(&data);
            let _ = crate::svg_pattern::decode_svg_or_tgv_bytes(&data, 1 << 20);
        });
    }
    fuzz.finish();
}

#[test]
fn wallpaper_links_and_color_strings_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7E3E_0002, ITERATIONS * 5);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(URL_TOKENS, 16);
        fuzz.check("wallpaper strings", i, &s, || {
            let _ = wallpaper::parse_wallpaper_url(&s);
            let _ = wallpaper::colors_from_string(&s);
            let _ = wallpaper::color_from_rgb6(&s);
            let _ = WallPaper::from_colors_slug(&s);
            let _ = crate::color::Color::from_hex(&s);
            let _ = crate::colorizer::colorize_hex(&s, &Colorizer::default());
            let _ = theme_file::is_good_slug(&s);
        });
    }
    fuzz.finish();
}

#[test]
fn colorize_hex_with_multibyte_chars_does_not_panic() {
    // Old code sliced `hex_color[1..7]` on a 9-byte string such as `#` + a flag emoji.
    assert_eq!(
        crate::colorizer::colorize_hex("#\u{1f1fa}\u{1f1e6}", &Colorizer::default()),
        None
    );
}
