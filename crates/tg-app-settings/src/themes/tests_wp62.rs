//! WP-62 unit tests, `egui_kittest` UI interaction & rasterizer snapshot tests,
//! and headless wgpu before/after screenshot capture helper.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_theme::svg_pattern::{PatternTileMode, render_tiled_pattern_wallpaper};
use tg_theme::{
    CloudTheme, CloudThemeSettings, CloudThemeType, Color, EmbeddedType, WallPaper, WallPaperFlags,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::theme::ThemeManager;

use crate::themes::{
    BackgroundConfig, ChatBackground, ChatThemeChoice, ChooseChatThemeState, CloudDocumentRef,
    CloudThemeCreateRequest, CloudThemeInstallRequest, CloudThemeRecord, CloudThemeUpdateRequest,
    CloudThemesResult, CloudWallPaperRecord, CloudWallPapersResult, PeerColorOptionRecord,
    PeerColorsResult, PeerProfileColorOptionRecord, PeerProfileColorSet, PeerProfileColorsResult,
    ShareThemeBackdropOption, ShareThemeBackdropsResult, ShareThemeChoice, ShareThemePickerState,
    ShareThemeTab, ThemesBackend, ThemesStudio, ThemesTab, UniqueGiftChatThemeRecord,
    UniqueGiftChatThemesResult, WallPaperLookup, generate_pattern_wallpaper_preview,
};

static WGPU_LOCK: Mutex<()> = Mutex::new(());

/// Deterministic synthetic SVG pattern with clear high-contrast geometric shapes (stars/circles/stripes)
/// suitable for both positive (`+40`) and negative (`-50` dark-mode) intensity verification.
fn sample_svg_pattern_bytes() -> Vec<u8> {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <circle cx="32" cy="32" r="18" fill="#000000"/>
  <rect x="76" y="16" width="32" height="32" rx="8" fill="#000000"/>
  <polygon points="32,74 44,108 16,86 48,86 20,108" fill="#000000"/>
  <circle cx="92" cy="92" r="16" fill="none" stroke="#000000" stroke-width="6"/>
</svg>"##
        .to_vec()
}

/// Minimal RFC-1952 gzip writer (stored deflate block + IEEE CRC-32) so tests generate valid `.tgv`
/// streams without adding a new crate dependency to `tg-app-settings`.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    // GZIP header: ID1, ID2, CM=8(deflate), FLG=0, MTIME=0, XFL=0, OS=255
    out.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF]);
    // Deflate stored blocks (max 65535 bytes per block)
    let mut offset = 0;
    loop {
        let remain = data.len().saturating_sub(offset);
        let chunk_len = remain.min(0xFFFF);
        let is_final = offset + chunk_len >= data.len();
        out.push(if is_final { 0x01 } else { 0x00 });
        let len = chunk_len as u16;
        let nlen = !len;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
        if is_final {
            break;
        }
    }
    // CRC-32 (IEEE 802.3)
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc = !crc;
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Gzipped `.tgv` encoding of [`sample_svg_pattern_bytes`].
fn sample_tgv_pattern_bytes() -> Vec<u8> {
    gzip_stored(&sample_svg_pattern_bytes())
}

fn sample_gift_record(
    gift_id: u64,
    slug: &str,
    title: &str,
    num: i32,
    light_colors: Vec<Color>,
    dark_colors: Vec<Color>,
    accent: Color,
) -> UniqueGiftChatThemeRecord {
    let mut light_wp = WallPaper::custom()
        .with_background_colors(light_colors)
        .with_pattern_intensity(38);
    light_wp.flags = WallPaperFlags::PATTERN;
    light_wp.slug = format!("{slug}_day");

    let mut dark_wp = WallPaper::custom()
        .with_background_colors(dark_colors)
        .with_pattern_intensity(-46);
    dark_wp.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);
    dark_wp.slug = format!("{slug}_night");

    let mut theme = CloudTheme {
        id: gift_id,
        slug: slug.to_owned(),
        title: title.to_owned(),
        ..CloudTheme::default()
    };
    theme.settings.insert(
        CloudThemeType::Light,
        CloudThemeSettings {
            paper: Some(light_wp),
            accent_color: accent,
            outgoing_accent_color: Some(accent),
            outgoing_messages_colors: vec![accent],
        },
    );
    theme.settings.insert(
        CloudThemeType::Dark,
        CloudThemeSettings {
            paper: Some(dark_wp),
            accent_color: accent,
            outgoing_accent_color: Some(accent),
            outgoing_messages_colors: vec![accent],
        },
    );

    UniqueGiftChatThemeRecord {
        gift_id,
        slug: slug.to_owned(),
        title: title.to_owned(),
        num,
        theme,
    }
}

/// Recording fake [`ThemesBackend`] exercising every WP-58 / WP-62 trait method without network.
#[derive(Default)]
struct RecordingThemesBackend {
    tgv_bytes: Vec<u8>,
    gift_pages: Vec<(Vec<UniqueGiftChatThemeRecord>, Option<String>)>,
    backdrops: Vec<ShareThemeBackdropOption>,
    peer_colors: Vec<PeerColorOptionRecord>,
    profile_colors: Vec<PeerProfileColorOptionRecord>,
    saved_themes: Mutex<Vec<(u64, u64, bool)>>,
    set_chat_themes: Mutex<Vec<(i64, ChatThemeChoice)>>,
    set_share_themes: Mutex<Vec<(i64, ShareThemeChoice)>>,
    gift_theme_calls: Mutex<Vec<(String, i32, i64)>>,
    share_backdrop_calls: Mutex<Vec<i32>>,
    peer_color_calls: Mutex<Vec<i32>>,
    profile_color_calls: Mutex<Vec<i32>>,
    download_calls: Mutex<Vec<i64>>,
}

impl ThemesBackend for RecordingThemesBackend {
    fn get_themes(&self, _hash: i64) -> Result<CloudThemesResult, String> {
        Ok(CloudThemesResult::NotModified)
    }

    fn upload_theme(
        &self,
        _file_name: &str,
        _theme_bytes: &[u8],
        _thumb_bytes: Option<&[u8]>,
    ) -> Result<CloudDocumentRef, String> {
        Err("not used in wp62 test".to_owned())
    }

    fn create_theme(&self, _req: &CloudThemeCreateRequest) -> Result<CloudThemeRecord, String> {
        Err("not used in wp62 test".to_owned())
    }

    fn update_theme(&self, _req: &CloudThemeUpdateRequest) -> Result<CloudThemeRecord, String> {
        Err("not used in wp62 test".to_owned())
    }

    fn install_theme(&self, _req: &CloudThemeInstallRequest) -> Result<(), String> {
        Ok(())
    }

    fn get_chat_themes(&self, _hash: i64) -> Result<CloudThemesResult, String> {
        Ok(CloudThemesResult::NotModified)
    }

    fn get_wallpapers(&self, _hash: i64) -> Result<CloudWallPapersResult, String> {
        let mut wp = WallPaper::new(9001)
            .with_background_colors(vec![
                Color::rgb(0xDB, 0xDD, 0xBB),
                Color::rgb(0x6B, 0xA5, 0x87),
                Color::rgb(0xD5, 0xD8, 0x8D),
                Color::rgb(0x88, 0xB8, 0x84),
            ])
            .with_pattern_intensity(40);
        wp.flags = WallPaperFlags::PATTERN;
        wp.slug = "meadow-vector".to_owned();
        Ok(CloudWallPapersResult::WallPapers {
            hash: 9001,
            wallpapers: vec![CloudWallPaperRecord {
                wallpaper: wp,
                motion: true,
                is_no_file: false,
                document: Some(CloudDocumentRef {
                    id: 55_001,
                    access_hash: 77_001,
                    file_reference: vec![1, 2, 3],
                    dc_id: 2,
                    mime_type: "application/x-tgwallpattern".to_owned(),
                    size: self.tgv_bytes.len() as i64,
                }),
            }],
        })
    }

    fn get_multi_wallpapers(
        &self,
        _lookups: &[WallPaperLookup],
    ) -> Result<Vec<CloudWallPaperRecord>, String> {
        Ok(Vec::new())
    }

    fn save_wallpaper(&self, _record: &CloudWallPaperRecord, _unsave: bool) -> Result<(), String> {
        Ok(())
    }

    fn install_wallpaper(&self, _record: &CloudWallPaperRecord) -> Result<(), String> {
        Ok(())
    }

    fn upload_wallpaper(
        &self,
        _bytes: &[u8],
        _mime_type: &str,
        _for_chat: bool,
        record: &CloudWallPaperRecord,
    ) -> Result<CloudWallPaperRecord, String> {
        Ok(record.clone())
    }

    fn set_chat_wallpaper(
        &self,
        _peer_id: i64,
        _record: Option<&CloudWallPaperRecord>,
        _for_both: bool,
        _revert: bool,
        _msg_id: Option<i32>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn download_theme_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String> {
        self.download_calls.lock().expect("lock").push(doc.id);
        Ok(self.tgv_bytes.clone())
    }

    fn download_wallpaper_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String> {
        self.download_calls.lock().expect("lock").push(doc.id);
        Ok(self.tgv_bytes.clone())
    }

    fn save_theme(&self, theme_id: u64, access_hash: u64, unsave: bool) -> Result<(), String> {
        self.saved_themes
            .lock()
            .expect("lock")
            .push((theme_id, access_hash, unsave));
        Ok(())
    }

    fn set_chat_theme(&self, peer_id: i64, choice: &ChatThemeChoice) -> Result<(), String> {
        self.set_chat_themes
            .lock()
            .expect("lock")
            .push((peer_id, choice.clone()));
        Ok(())
    }

    fn get_unique_gift_chat_themes(
        &self,
        offset: &str,
        limit: i32,
        hash: i64,
    ) -> Result<UniqueGiftChatThemesResult, String> {
        self.gift_theme_calls
            .lock()
            .expect("lock")
            .push((offset.to_owned(), limit, hash));
        let page_idx: usize = if offset.is_empty() {
            0
        } else {
            offset.parse().unwrap_or(1)
        };
        if let Some((themes, next_offset)) = self.gift_pages.get(page_idx) {
            Ok(UniqueGiftChatThemesResult::Themes {
                hash: 4242,
                themes: themes.clone(),
                next_offset: next_offset.clone(),
            })
        } else {
            Ok(UniqueGiftChatThemesResult::NotModified)
        }
    }

    fn get_share_theme_backdrops(&self, hash: i32) -> Result<ShareThemeBackdropsResult, String> {
        self.share_backdrop_calls.lock().expect("lock").push(hash);
        Ok(ShareThemeBackdropsResult::Backdrops {
            hash: 8811,
            backdrops: self.backdrops.clone(),
        })
    }

    fn set_share_theme(&self, peer_id: i64, choice: &ShareThemeChoice) -> Result<(), String> {
        self.set_share_themes
            .lock()
            .expect("lock")
            .push((peer_id, choice.clone()));
        Ok(())
    }

    fn get_peer_colors(&self, hash: i32) -> Result<PeerColorsResult, String> {
        self.peer_color_calls.lock().expect("lock").push(hash);
        let suggested = self
            .peer_colors
            .iter()
            .filter(|c| !c.hidden)
            .map(|c| c.color_id)
            .collect();
        Ok(PeerColorsResult::Colors {
            hash: 3101,
            options: self.peer_colors.clone(),
            suggested,
        })
    }

    fn get_peer_profile_colors(&self, hash: i32) -> Result<PeerProfileColorsResult, String> {
        self.profile_color_calls.lock().expect("lock").push(hash);
        Ok(PeerProfileColorsResult::Colors {
            hash: 3202,
            options: self.profile_colors.clone(),
        })
    }
}

fn sample_recording_backend() -> Arc<RecordingThemesBackend> {
    let gift1 = sample_gift_record(
        801,
        "PlushPepe-77",
        "Emerald Relic",
        77,
        vec![
            Color::rgb(0xC9, 0xF0, 0xD8),
            Color::rgb(0x76, 0xC8, 0x9E),
            Color::rgb(0xDC, 0xF5, 0xB8),
            Color::rgb(0x8E, 0xD1, 0xA8),
        ],
        vec![
            Color::rgb(0x11, 0x29, 0x1F),
            Color::rgb(0x1E, 0x4A, 0x38),
            Color::rgb(0x16, 0x33, 0x28),
            Color::rgb(0x24, 0x52, 0x3D),
        ],
        Color::rgb(0x2F, 0x9E, 0x6F),
    );
    let gift2 = sample_gift_record(
        802,
        "StarCrown-14",
        "Solar Crown",
        14,
        vec![
            Color::rgb(0xFF, 0xEB, 0xC8),
            Color::rgb(0xF5, 0xB8, 0x6E),
            Color::rgb(0xFF, 0xD9, 0xA0),
        ],
        vec![
            Color::rgb(0x2F, 0x1E, 0x10),
            Color::rgb(0x5E, 0x3A, 0x19),
            Color::rgb(0x3D, 0x26, 0x13),
        ],
        Color::rgb(0xD9, 0x82, 0x2B),
    );
    let gift3 = sample_gift_record(
        803,
        "AstralOrb-305",
        "Astral Orb",
        305,
        vec![
            Color::rgb(0xD5, 0xD8, 0xFF),
            Color::rgb(0x8C, 0x96, 0xF2),
            Color::rgb(0xE0, 0xE2, 0xFF),
        ],
        vec![
            Color::rgb(0x15, 0x17, 0x38),
            Color::rgb(0x2B, 0x30, 0x6B),
            Color::rgb(0x1E, 0x21, 0x4F),
        ],
        Color::rgb(0x6C, 0x76, 0xE6),
    );

    Arc::new(RecordingThemesBackend {
        tgv_bytes: sample_tgv_pattern_bytes(),
        gift_pages: vec![
            (vec![gift1, gift2], Some("1".to_owned())),
            (vec![gift3], None),
        ],
        backdrops: vec![
            ShareThemeBackdropOption {
                backdrop_id: 10,
                name: "Azure Reef".to_owned(),
                center_color: Color::rgb(0x2A, 0x9D, 0xF4),
                edge_color: Color::rgb(0x12, 0x4E, 0x89),
                pattern_color: Color::rgb(0x0B, 0x30, 0x57),
                text_color: Color::WHITE,
            },
            ShareThemeBackdropOption {
                backdrop_id: 11,
                name: "Amber Dusk".to_owned(),
                center_color: Color::rgb(0xF0, 0x8A, 0x4B),
                edge_color: Color::rgb(0x8E, 0x2E, 0x24),
                pattern_color: Color::rgb(0x54, 0x19, 0x14),
                text_color: Color::WHITE,
            },
            ShareThemeBackdropOption {
                backdrop_id: 12,
                name: "Jade Canopy".to_owned(),
                center_color: Color::rgb(0x3B, 0xB2, 0x73),
                edge_color: Color::rgb(0x15, 0x57, 0x3C),
                pattern_color: Color::rgb(0x0C, 0x38, 0x25),
                text_color: Color::WHITE,
            },
            ShareThemeBackdropOption {
                backdrop_id: 13,
                name: "Iris Mist".to_owned(),
                center_color: Color::rgb(0x88, 0x60, 0xD0),
                edge_color: Color::rgb(0x3F, 0x25, 0x7A),
                pattern_color: Color::rgb(0x27, 0x16, 0x52),
                text_color: Color::WHITE,
            },
        ],
        peer_colors: vec![
            PeerColorOptionRecord {
                color_id: 0,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light_colors: vec![Color::rgb(0xCC, 0x50, 0x49)],
                dark_colors: vec![Color::rgb(0xFF, 0x84, 0x7C)],
            },
            PeerColorOptionRecord {
                color_id: 1,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light_colors: vec![Color::rgb(0xD6, 0x77, 0x22)],
                dark_colors: vec![Color::rgb(0xFF, 0xB3, 0x6B)],
            },
            PeerColorOptionRecord {
                color_id: 2,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light_colors: vec![Color::rgb(0x40, 0xA9, 0x20)],
                dark_colors: vec![Color::rgb(0x7B, 0xD9, 0x5D)],
            },
            PeerColorOptionRecord {
                color_id: 3,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light_colors: vec![Color::rgb(0x30, 0x9E, 0xBA)],
                dark_colors: vec![Color::rgb(0x6E, 0xD4, 0xED)],
            },
        ],
        profile_colors: vec![
            PeerProfileColorOptionRecord {
                color_id: 0,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light: PeerProfileColorSet {
                    palette: vec![Color::rgb(0xB8, 0x47, 0x40)],
                    bg: vec![Color::rgb(0xD8, 0x64, 0x5A), Color::rgb(0x8C, 0x2D, 0x29)],
                    story: vec![Color::rgb(0xFF, 0x8A, 0x78)],
                },
                dark: PeerProfileColorSet::default(),
            },
            PeerProfileColorOptionRecord {
                color_id: 1,
                hidden: false,
                channel_min_level: None,
                group_min_level: None,
                light: PeerProfileColorSet {
                    palette: vec![Color::rgb(0x2B, 0x7D, 0xA8)],
                    bg: vec![Color::rgb(0x3E, 0x9B, 0xCC), Color::rgb(0x1B, 0x4E, 0x73)],
                    story: vec![Color::rgb(0x6E, 0xC8, 0xF5)],
                },
                dark: PeerProfileColorSet::default(),
            },
        ],
        ..Default::default()
    })
}

fn render_studio_wgpu(size: Vec2, studio: ThemesStudio) -> image::RgbaImage {
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(1.0 / 30.0)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            ctx.all_styles_mut(|style| {
                style.visuals.text_cursor.blink = false;
                style.scroll_animation = egui::style::ScrollAnimation::none();
                style.animation_time = 0.0;
            });
        })
        .build_ui_state(
            |ui, st: &mut ThemesStudio| {
                st.show_at(ui, true, 1_000);
                ui.ctx().all_styles_mut(|style| {
                    style.visuals.text_cursor.blink = false;
                    style.scroll_animation = egui::style::ScrollAnimation::none();
                    style.animation_time = 0.0;
                });
            },
            studio,
        );
    for _ in 0..4 {
        harness.step();
    }
    harness.render().expect("wgpu render studio")
}

#[test]
fn chat_background_rasterizes_tgv_and_svg_for_positive_and_negative_intensity() {
    let tgv = sample_tgv_pattern_bytes();
    let svg = sample_svg_pattern_bytes();
    assert!(ChatBackground::is_svg_or_tgv_pattern_bytes(&tgv));
    assert!(ChatBackground::is_svg_or_tgv_pattern_bytes(&svg));
    assert!(!ChatBackground::is_svg_or_tgv_pattern_bytes(b"not-an-svg"));

    // Positive intensity (+40 light mode)
    let mut wp_pos = WallPaper::new(42)
        .with_background_colors(vec![
            Color::rgb(0xDB, 0xDD, 0xBB),
            Color::rgb(0x6B, 0xA5, 0x87),
            Color::rgb(0xD5, 0xD8, 0x8D),
            Color::rgb(0x88, 0xB8, 0x84),
        ])
        .with_pattern_intensity(40);
    wp_pos.flags = WallPaperFlags::PATTERN;

    let cfg_tgv_pos =
        BackgroundConfig::from_svg_pattern(wp_pos.clone(), tgv.clone(), false).expect("tgv +40");
    let cfg_svg_pos =
        BackgroundConfig::from_svg_pattern(wp_pos.clone(), svg.clone(), false).expect("svg +40");
    assert!(cfg_tgv_pos.has_svg_pattern());
    assert_eq!(cfg_tgv_pos.pattern, crate::themes::PatternKind::None);

    let mut bg = ChatBackground::new();
    let img_tgv_pos = bg.render_image(&cfg_tgv_pos, 192, 192, 0);
    let img_svg_pos = bg.render_image(&cfg_svg_pos, 192, 192, 0);
    assert_eq!(
        img_tgv_pos.as_raw(),
        img_svg_pos.as_raw(),
        ".tgv gzip and raw .svg must rasterize identically"
    );

    let direct_pos = render_tiled_pattern_wallpaper(
        &tgv,
        192,
        192,
        &wp_pos.background_colors,
        wp_pos.rotation,
        wp_pos.intensity,
        Some((93, 192)), // pattern scaled to the area height at 1440:2960 (`CacheBackgroundByRequest`)
        PatternTileMode::Centered,
    )
    .expect("direct tg-theme render +40");
    assert_eq!(
        img_tgv_pos.as_raw(),
        direct_pos.as_raw(),
        "ChatBackground must use tg_theme::svg_pattern directly without procedural fallback"
    );

    // Negative intensity (-50 dark mode)
    let mut wp_neg = wp_pos.clone().with_pattern_intensity(-50);
    wp_neg.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);
    let cfg_tgv_neg =
        BackgroundConfig::from_svg_pattern(wp_neg.clone(), tgv.clone(), false).expect("tgv -50");
    let img_tgv_neg = bg.render_image(&cfg_tgv_neg, 192, 192, 0);
    let direct_neg = render_tiled_pattern_wallpaper(
        &tgv,
        192,
        192,
        &wp_neg.background_colors,
        wp_neg.rotation,
        wp_neg.intensity,
        Some((93, 192)), // pattern scaled to the area height at 1440:2960 (`CacheBackgroundByRequest`)
        PatternTileMode::Centered,
    )
    .expect("direct tg-theme render -50");
    assert_eq!(img_tgv_neg.as_raw(), direct_neg.as_raw());
    assert_ne!(
        img_tgv_pos.as_raw(),
        img_tgv_neg.as_raw(),
        "positive (+40) and negative (-50) intensity composites must differ visibly"
    );

    // Preview helper produces matching output
    let preview_img = generate_pattern_wallpaper_preview(&cfg_tgv_pos, 192, 192)
        .expect("preview helper for svg pattern");
    assert_eq!(preview_img.dimensions(), (192, 192));
}

#[test]
fn chat_background_tgv_wgpu_snapshot_renders_nonzero_pattern_pixels() {
    let _guard = WGPU_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tgv = sample_tgv_pattern_bytes();
    let mut wp = WallPaper::new(99)
        .with_background_colors(vec![
            Color::rgb(0x16, 0x26, 0x3D),
            Color::rgb(0x28, 0x4E, 0x73),
            Color::rgb(0x1B, 0x34, 0x52),
            Color::rgb(0x32, 0x62, 0x8C),
        ])
        .with_pattern_intensity(-50);
    wp.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);

    let cfg = BackgroundConfig::from_svg_pattern(wp, tgv, false).expect("tgv config");
    let mut bg = ChatBackground::new();
    bg.set_dark_mode(true);
    bg.set_global(cfg);

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(256.0, 256.0))
        .with_step_dt(1.0 / 30.0)
        .with_render_cursor(false)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui_state(
            |ui, chat_bg: &mut ChatBackground| {
                let rect = ui.max_rect();
                chat_bg.paint(ui, rect, None);
            },
            bg,
        );
    harness.step();
    harness.step();
    let rendered = harness.render().expect("wgpu render tgv background");
    assert_eq!(rendered.dimensions(), (256, 256));

    // Verify rendered canvas is not flat/blank and has contrast variation from gradient + negative pattern
    let mut min_luma = u32::MAX;
    let mut max_luma = 0u32;
    for p in rendered.pixels() {
        let luma = u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]);
        min_luma = min_luma.min(luma);
        max_luma = max_luma.max(luma);
    }
    assert!(
        max_luma.saturating_sub(min_luma) >= 40,
        "expected visible pattern/gradient contrast, got range {min_luma}..={max_luma}"
    );
}

#[test]
fn unique_gift_chat_theme_picker_kittest_selects_paginates_and_dispatches_rpc() {
    let backend = sample_recording_backend();
    let mut picker = ChooseChatThemeState::new(777, "Orbit Group");
    let bg = ChatBackground::new();
    let tm = ThemeManager::new();

    // Hydrate page 1 (2 gifts, next_offset = Some("1"))
    let count = picker
        .hydrate_unique_gift_themes(backend.as_ref(), false)
        .expect("hydrate unique gift themes page 1");
    assert_eq!(count, 2);
    assert_eq!(picker.gift_next_offset(), Some("1"));
    assert_eq!(backend.gift_theme_calls.lock().expect("lock").len(), 1);
    assert_eq!(backend.gift_theme_calls.lock().expect("lock")[0].1, 24);

    // Load page 2 via pagination
    let count2 = picker
        .hydrate_unique_gift_themes(backend.as_ref(), true)
        .expect("hydrate unique gift themes page 2");
    assert_eq!(count2, 3);
    assert_eq!(picker.gift_next_offset(), None);

    // Mark gift #14 as taken by another chat to verify reassignment confirmation
    assert!(picker.set_gift_taken_by("StarCrown-14", Some("Design Squad".to_owned())));

    struct HarnessState {
        picker: ChooseChatThemeState,
        bg: ChatBackground,
        tm: ThemeManager,
        backend: Arc<RecordingThemesBackend>,
        last_applied: Option<(i64, ChatThemeChoice)>,
    }

    let initial_gen = picker.preview_generation();
    let state = HarnessState {
        picker,
        bg,
        tm,
        backend: Arc::clone(&backend),
        last_applied: None,
    };

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(760.0, 560.0))
        .with_step_dt(1.0 / 30.0)
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut HarnessState| {
                let pal = st.tm.palette().clone();
                let out = st.picker.show_with_backend(
                    ui,
                    &mut st.bg,
                    false,
                    &pal,
                    Some(st.backend.as_ref()),
                );
                if let Some((peer_id, choice)) = out.applied_choice {
                    let _ = st.backend.set_chat_theme(peer_id, &choice);
                    st.last_applied = Some((peer_id, choice));
                }
            },
            state,
        );

    harness.step();

    // Click collectible gift "Emerald Relic gift theme #77"
    harness.get_by_label("Emerald Relic gift theme #77").click();
    harness.step();
    assert_eq!(
        harness.state().picker.selected_choice(),
        &ChatThemeChoice::UniqueGift("PlushPepe-77".to_owned())
    );
    assert!(
        harness.state().picker.preview_generation() > initial_gen,
        "selecting a unique gift theme must advance preview generation"
    );

    // Click Apply Theme -> dispatches `messages.setChatTheme` with `ChatThemeChoice::UniqueGift`
    let apply_label = tg_lang::tr::lng_chat_theme_apply();
    harness.get_by_label(&apply_label).click();
    harness.step();

    let sent = backend.set_chat_themes.lock().expect("lock");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0],
        (777, ChatThemeChoice::UniqueGift("PlushPepe-77".to_owned()))
    );
    drop(sent);

    // Selecting a taken gift ("Solar Crown") shows reassignment confirmation before dispatching
    harness.get_by_label("Solar Crown gift theme #14").click();
    harness.step();
    harness.step();
    harness.get_by_label(&apply_label).click();
    harness.step();
    harness.step();
    assert_eq!(
        backend.set_chat_themes.lock().expect("lock").len(),
        1,
        "taken gift must wait for reassignment confirmation"
    );
    harness.get_by_label("Use Here").click();
    harness.step();
    harness.step();
    let sent_after = backend.set_chat_themes.lock().expect("lock");
    assert_eq!(sent_after.len(), 2);
    assert_eq!(
        sent_after[1],
        (777, ChatThemeChoice::UniqueGift("StarCrown-14".to_owned()))
    );
}

#[test]
fn share_theme_picker_kittest_selects_backdrop_updates_preview_and_dispatches_rpc() {
    let backend = sample_recording_backend();
    let mut picker = ShareThemePickerState::new(505, "Eva Summer");
    let count = picker
        .hydrate_from_backend(backend.as_ref())
        .expect("hydrate share backdrops + peer colors");
    assert_eq!(count, 4);
    assert_eq!(picker.peer_colors().len(), 4);
    assert_eq!(picker.peer_profile_colors().len(), 2);

    struct ShareHarnessState {
        picker: ShareThemePickerState,
        tm: ThemeManager,
        backend: Arc<RecordingThemesBackend>,
    }

    let state = ShareHarnessState {
        picker,
        tm: ThemeManager::new(),
        backend: Arc::clone(&backend),
    };

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(760.0, 520.0))
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ShareHarnessState| {
                let pal = st.tm.palette().clone();
                st.picker.show(ui, &pal, &[], Some(st.backend.as_ref()));
            },
            state,
        );

    harness.step();
    let initial_preview_gen = harness.state().picker.preview_generation();
    assert!(initial_preview_gen >= 1);

    // Click "Amber Dusk" circular swatch (step twice so the next frame regenerates the preview texture)
    harness.get_by_label("Share backdrop: Amber Dusk").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().picker.selected_backdrop_id(), Some(11));
    assert!(
        harness.state().picker.preview_generation() > initial_preview_gen,
        "selecting a backdrop swatch must regenerate the live share card texture"
    );

    // Apply share theme -> dispatches `messages.setShareTheme` with `backdrop_id: Some(11)`
    harness.get_by_label("Apply Share Theme").click();
    harness.step();
    harness.step();

    let calls = backend.set_share_themes.lock().expect("lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            505,
            ShareThemeChoice::EmojiBackdrop {
                backdrop_id: Some(11),
                emojis: vec![],
            }
        )
    );
    drop(calls);

    // Toggle to Emoji tab and verify sub-tab switch works cleanly
    harness.state_mut().picker.active_tab = ShareThemeTab::Emoji;
    harness.step();
    assert!(harness.state_mut().picker.toggle_emoji("✨"));
    assert_eq!(harness.state().picker.selected_emojis(), &["✨".to_owned()]);
}

#[test]
fn themes_studio_hydrates_cloud_tgv_pattern_and_dispatches_save_theme() {
    let backend = sample_recording_backend();
    let mut studio = ThemesStudio::new();
    studio.set_backend(backend.clone());
    studio
        .refresh_cloud_catalogs()
        .expect("refresh cloud catalogs");

    // Verify `.tgv` pattern bytes were downloaded and attached to the cloud wallpaper
    assert_eq!(
        backend.download_calls.lock().expect("lock").as_slice(),
        &[55_001]
    );
    let cloud_item = studio
        .bg_picker
        .items()
        .iter()
        .find(|item| item.id == "cloud_wp_9001")
        .expect("cloud_wp_9001 wallpaper present");
    assert!(cloud_item.config.has_svg_pattern());

    // Verify `save_theme` dispatches cleanly
    studio
        .save_cloud_theme_live(901, 4444, false)
        .expect("save theme");
    studio
        .save_cloud_theme_live(901, 4444, true)
        .expect("unsave theme");
    let saved = backend.saved_themes.lock().expect("lock");
    assert_eq!(saved.as_slice(), &[(901, 4444, false), (901, 4444, true)]);
}

#[test]
fn wp62_capture_before_after_screenshots() {
    let Ok(stage) = std::env::var("RUSTGRAM_WP62_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_WP62_SNAP_STAGE"))
    else {
        return;
    };
    if stage != "before" && stage != "after" {
        return;
    }
    let _guard = WGPU_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let snap_dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp62"));
    std::fs::create_dir_all(&snap_dir).expect("create wp62 snap dir");

    let sizes = [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ];

    let backend = sample_recording_backend();

    for (size_label, size) in sizes {
        // 1. Backgrounds tab (showcasing real `.tgv` vector pattern wallpaper in `after`)
        let mut studio_bg = ThemesStudio::new();
        studio_bg.active_tab = ThemesTab::Backgrounds;
        if stage == "after" {
            studio_bg.set_backend(backend.clone());
            let _ = studio_bg.refresh_cloud_catalogs();
            studio_bg.bg_picker.select_by_id("cloud_wp_9001");
        } else {
            studio_bg.bg_picker.select_by_id("classic_meadow");
        }
        let img = render_studio_wgpu(size, studio_bg);
        img.save(snap_dir.join(format!("backgrounds_{size_label}_{stage}.png")))
            .expect("save backgrounds png");

        // 2. Chat Themes tab (showcasing Unique Gift Themes + Shareable Theme Backdrops in `after`)
        let mut studio_ct = ThemesStudio::new();
        studio_ct.active_tab = ThemesTab::ChatThemes;
        if stage == "after" {
            studio_ct.set_backend(backend.clone());
            let _ = studio_ct.refresh_cloud_catalogs();
            studio_ct
                .chat_theme_picker
                .set_gift_taken_by("StarCrown-14", Some("Design Squad".to_owned()));
            studio_ct.chat_theme_picker.select_gift_slug("PlushPepe-77");
            studio_ct.share_theme_picker.select_backdrop(Some(10), 0.0);
        } else {
            studio_ct.chat_theme_picker.select_token("astronaut");
        }
        let img = render_studio_wgpu(size, studio_ct);
        img.save(snap_dir.join(format!("chat_themes_{size_label}_{stage}.png")))
            .expect("save chat_themes png");

        // 3. Previews tab (showcasing clean copy + Cloud Themes Save/Install controls in `after`)
        let mut studio_prev = ThemesStudio::new();
        studio_prev.active_tab = ThemesTab::Previews;
        studio_prev.preview_embedded = EmbeddedType::Night;
        if stage == "after" {
            studio_prev.set_backend(backend.clone());
            studio_prev.set_cloud_themes(vec![
                CloudThemeRecord {
                    theme: CloudTheme {
                        id: 901,
                        access_hash: 4444,
                        slug: "nordic_pine".to_owned(),
                        title: "Nordic Pine".to_owned(),
                        ..CloudTheme::default()
                    },
                    is_creator: true,
                    is_default: false,
                    for_chat: false,
                    document: None,
                },
                CloudThemeRecord {
                    theme: CloudTheme {
                        id: 902,
                        access_hash: 5555,
                        slug: "midnight_slate".to_owned(),
                        title: "Midnight Slate".to_owned(),
                        ..CloudTheme::default()
                    },
                    is_creator: false,
                    is_default: false,
                    for_chat: false,
                    document: None,
                },
            ]);
        }
        let img = render_studio_wgpu(size, studio_prev);
        img.save(snap_dir.join(format!("previews_{size_label}_{stage}.png")))
            .expect("save previews png");
    }
}
