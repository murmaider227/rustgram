//! Tests and headless `egui_kittest` snapshot helpers for WP-49 Instant View & Rich Editor wiring:
//! - `IvCachedMedia` disk-backed & RAM caching in `IvResourceBridge` / `InstantViewWindow`
//! - `IvSearchController` find bar (`Ctrl+F`, next/prev, match count, `Esc`)
//! - `html_export`, `clipboard_import`, and `MathBoxState` in `RichEditorWindow`

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{Event, Key, Modifiers, Vec2};
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::PeerId;
use tg_iv::editor::ClipboardPayload;
use tg_iv::rich_page::{RichBlockKind, RichPage};
use tg_iv::{IvCachedMedia, MediaBlob, MediaProvider, MemoryMediaStore, Page, PageBlock, RichText};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::webview::backend::{ClipboardAccess, ExternalOpener, WebViewBackend};
use crate::webview::instant_view::InstantViewWindow;
use crate::webview::mock::{MockWebViewBackend, SAMPLE_IV_URL};
use crate::webview::rich_editor::{RichEditorTab, RichEditorWindow};
use crate::webview::theme::{egui_visuals_from_tg_palette, tg_palette_for_mode};

static WGPU_MEDIA_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct NoopExternalOpener;

impl ExternalOpener for NoopExternalOpener {
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct NoopClipboard;

impl ClipboardAccess for NoopClipboard {
    fn read_text(&self) -> Option<String> {
        None
    }

    fn write_text(&self, _text: &str) -> bool {
        true
    }
}

/// Fixture media provider with real PNG/JPEG/MP3 magic headers (no network).
struct FixtureMediaProvider {
    photo_png: Vec<u8>,
    doc_bytes: Vec<u8>,
    map_jpeg: Vec<u8>,
}

impl FixtureMediaProvider {
    fn sample() -> Self {
        Self {
            photo_png: vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4],
            doc_bytes: vec![b'I', b'D', b'3', 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            map_jpeg: vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'],
        }
    }
}

impl MediaProvider for FixtureMediaProvider {
    fn photo(&self, id: i64) -> Option<MediaBlob> {
        (id == 42).then(|| MediaBlob {
            content_type: String::new(), // empty -> IvCachedMedia sniffs "image/png"
            bytes: self.photo_png.clone(),
        })
    }

    fn document(&self, id: i64) -> Option<MediaBlob> {
        (id == 77).then(|| MediaBlob {
            content_type: String::new(), // empty -> IvCachedMedia sniffs "audio/mpeg"
            bytes: self.doc_bytes.clone(),
        })
    }

    fn map_tile(
        &self,
        _lat: f64,
        _lon: f64,
        _zoom: i32,
        _width: i32,
        _height: i32,
    ) -> Option<MediaBlob> {
        Some(MediaBlob {
            content_type: String::new(), // empty -> IvCachedMedia sniffs "image/jpeg"
            bytes: self.map_jpeg.clone(),
        })
    }
}

fn fixture_iv_page() -> Page {
    Page {
        id: 5001,
        url: "https://example.org/instant-view-fixture".to_owned(),
        site_name: Some("Fixture Press".to_owned()),
        blocks: vec![
            PageBlock::Title(RichText::plain("Instant View Search Fixture")),
            PageBlock::Paragraph(RichText::plain(
                "Instant View renders articles locally. Instant View also caches media.",
            )),
            PageBlock::Paragraph(RichText::plain(
                "Final paragraph mentions instant view one more time for cycling.",
            )),
        ],
        ..Page::default()
    }
}

fn read_response_bytes(resp: tg_webview::HttpResponse) -> Vec<u8> {
    let mut stream = resp.stream.expect("http response stream");
    let len = usize::try_from(stream.size().max(0)).unwrap_or(0);
    let mut buf = vec![0u8; len];
    let n = stream.read(&mut buf).expect("read http stream");
    buf.truncate(n);
    buf
}

fn save_image_png(img: &image::RgbaImage, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    img.save(path).expect("save screenshot png");
}

// ============================================================================
// 1. Unit tests for `IvCachedMedia` disk-backed wiring in `InstantViewWindow`
// ============================================================================

#[test]
fn iv_cached_media_disk_write_through_and_reload_after_release() {
    let dir = TempDir::new().expect("create temp dir for IvCachedMedia");
    let fixture_media = Arc::new(FixtureMediaProvider::sample());
    let expected_png = fixture_media.photo_png.clone();
    let expected_doc = fixture_media.doc_bytes.clone();
    let expected_map = fixture_media.map_jpeg.clone();

    let mut win = InstantViewWindow::with_disk_cache(
        fixture_iv_page(),
        fixture_media,
        dir.path(),
        false,
        None,
    )
    .expect("create disk-backed InstantViewWindow");

    // 1. Initial requests miss cache, pull from FixtureMediaProvider, and write through to disk.
    let photo_resp = win
        .resource_bridge()
        .serve_sync("rustgram-internal://iv/media/photo/42", None)
        .expect("photo 42 response");
    assert_eq!(photo_resp.status_code, 200);
    assert_eq!(read_response_bytes(photo_resp), expected_png);

    let doc_resp = win
        .resource_bridge()
        .serve_sync("rustgram-internal://iv/media/doc/77", None)
        .expect("doc 77 response");
    assert_eq!(doc_resp.status_code, 200);
    assert_eq!(read_response_bytes(doc_resp), expected_doc);

    let map_resp = win
        .resource_bridge()
        .serve_sync(
            "rustgram-internal://iv/media/map/47.3769,8.5417,14,320x180",
            None,
        )
        .expect("map tile response");
    assert_eq!(map_resp.status_code, 200);
    assert_eq!(read_response_bytes(map_resp), expected_map);

    // Sniffed MIME types were stored in IvCachedMedia.
    let cached_photo = win.cached_media().get_photo(42).expect("photo in cache");
    assert_eq!(cached_photo.content_type, "image/png");
    let cached_doc = win.cached_media().get_document(77).expect("doc in cache");
    assert_eq!(cached_doc.content_type, "audio/mpeg");

    // 2. Replace fallback provider with empty MemoryMediaStore and release RAM payloads.
    win.load_page(fixture_iv_page(), Arc::new(MemoryMediaStore::new()), None);
    win.cached_media().release_heavy_data();
    assert_eq!(win.cached_media().stats().entry_count, 0);

    // 3. Requests now hit disk cache directly (real bytes, NOT placeholder SVG).
    let from_disk = win
        .resource_bridge()
        .serve_sync("rustgram-internal://iv/media/photo/42", None)
        .expect("photo 42 reloaded from disk");
    assert_eq!(from_disk.status_code, 200);
    assert!(
        from_disk.headers.contains("image/png"),
        "expected image/png header from disk cache, got {}",
        from_disk.headers
    );
    assert_eq!(read_response_bytes(from_disk), expected_png);
    assert!(
        win.cached_media().stats().disk_hits >= 1,
        "expected at least 1 disk hit"
    );

    // 4. Uncached photo ID falls back to SVG placeholder cleanly.
    let missing = win
        .resource_bridge()
        .serve_sync("rustgram-internal://iv/media/photo/99999", None)
        .expect("missing photo placeholder");
    assert_eq!(missing.status_code, 200);
    assert!(missing.headers.contains("image/svg+xml"));

    // 5. Closing the window releases RAM payloads while keeping disk cache files intact.
    win.close();
    assert!(!win.is_open());
    assert_eq!(win.cached_media().stats().entry_count, 0);
    assert!(dir.path().join("photo_42.bin").is_file());
}

// ============================================================================
// 2. `egui_kittest` tests for Instant View Find Bar (`IvSearchController`)
// ============================================================================

struct IvFindTestState {
    win: InstantViewWindow,
    backend: MockWebViewBackend,
    external: NoopExternalOpener,
    clipboard: NoopClipboard,
    palette: tg_theme::Palette,
}

fn build_iv_harness(page: Page, size: Vec2) -> egui_kittest::Harness<'static, IvFindTestState> {
    let win = InstantViewWindow::new(page, Arc::new(MemoryMediaStore::new()), false, None);
    let state = IvFindTestState {
        win,
        backend: MockWebViewBackend::new(),
        external: NoopExternalOpener,
        clipboard: NoopClipboard,
        palette: tg_palette_for_mode(ThemeMode::Day),
    };
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            let pal = tg_palette_for_mode(ThemeMode::Day);
            ctx.set_visuals(egui_visuals_from_tg_palette(&pal, false));
            ctx.all_styles_mut(|s| {
                s.visuals.text_cursor.blink = false;
                s.animation_time = 0.0;
            });
        })
        .build_ui_state(
            |ui, st: &mut IvFindTestState| {
                let title = st.win.window_title();
                let _ = st.win.render_window_contents(
                    ui,
                    &st.palette,
                    &title,
                    &mut st.backend,
                    &st.external,
                    &st.clipboard,
                );
            },
            state,
        )
}

#[test]
fn iv_find_bar_ctrl_f_next_prev_count_and_escape_in_kittest() {
    let mut harness = build_iv_harness(fixture_iv_page(), Vec2::new(820.0, 560.0));
    harness.step();
    assert!(!harness.state().win.is_search_open());

    // 1. Ctrl+F opens the find bar immediately.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    harness.step();
    harness.step();
    assert!(harness.state().win.is_search_open());

    // 2. Type query "Instant View" (4 matches across 3 blocks, case-insensitive).
    harness
        .input_mut()
        .events
        .push(Event::Text("Instant View".to_owned()));
    harness.step();
    harness.step();
    assert_eq!(harness.state().win.search_controller().match_count(), 4);
    assert_eq!(
        harness.state().win.search_controller().display_position(),
        (1, 4)
    );
    assert!(
        harness.query_by_label("1 / 4").is_some(),
        "find bar must render match counter '1 / 4'"
    );

    // 3. Click "Next match" -> advances to 2 / 4.
    harness.get_by_label("Next match").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().win.search_controller().display_position(),
        (2, 4)
    );
    assert!(harness.query_by_label("2 / 4").is_some());

    // 4. Click "Previous match" twice -> wraps around from 1 / 4 to 4 / 4.
    harness.get_by_label("Previous match").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().win.search_controller().display_position(),
        (1, 4)
    );
    harness.get_by_label("Previous match").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().win.search_controller().display_position(),
        (4, 4)
    );
    assert!(harness.query_by_label("4 / 4").is_some());

    // 5. Escape closes the find bar without closing the Instant View window.
    harness.key_press_modifiers(Modifiers::NONE, Key::Escape);
    harness.step();
    assert!(!harness.state().win.is_search_open());
    assert!(
        harness.state().win.is_open(),
        "first Escape must close only the find bar, not the IV window"
    );

    // 6. Second Escape closes the Instant View window.
    harness.key_press_modifiers(Modifiers::NONE, Key::Escape);
    harness.step();
    assert!(!harness.state().win.is_open());
}

// ============================================================================
// 3. `RichEditorWindow` tests: `MathBoxState`, `clipboard_import`, `html_export`
// ============================================================================

#[test]
fn rich_editor_math_box_validation_and_separate_line_insert_in_kittest() {
    let editor = RichEditorWindow::new_compose(PeerId::from_user(42));
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(820.0, 620.0))
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            ctx.all_styles_mut(|s| {
                s.visuals.text_cursor.blink = false;
                s.animation_time = 0.0;
            });
        })
        .build_ui_state(
            |ui, ed: &mut RichEditorWindow| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let _ = ed.show_contents(ui, false);
                });
            },
            editor,
        );
    harness.step();

    // Open MathBoxState via toolbar button.
    harness.get_by_label("Math").click();
    harness.step();
    assert!(harness.state().math_box().is_some());
    assert!(
        harness
            .state()
            .math_box()
            .expect("math box open")
            .preview
            .using_sample,
        "empty MathBoxState must display a sample formula preview"
    );

    // Malformed LaTeX (unclosed `{`) triggers MathValidationError and blocks submit.
    let input = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .expect("math input visible");
    input.click();
    harness.step();
    harness
        .input_mut()
        .events
        .push(Event::Text(r"\frac{a}{b".to_owned()));
    harness.step();
    assert!(
        harness
            .state()
            .math_box()
            .expect("math box")
            .preview
            .validation_error
            .is_some(),
        "unclosed brace must surface MathValidationError"
    );
    assert!(!harness.state().math_box().expect("math box").is_valid());

    // Close the unclosed brace and switch to Separate Line Formula.
    harness.input_mut().events.push(Event::Text("}".to_owned()));
    harness.step();
    assert!(harness.state().math_box().expect("math box").is_valid());

    harness
        .get_by_label(&tg_lang::tr::lng_formatting_math_separate_line())
        .click();
    harness.step();

    harness.get_by_label("Insert Formula").click();
    harness.step();
    assert!(harness.state().math_box().is_none());
    assert!(
        harness
            .state()
            .state()
            .rich_page()
            .blocks
            .iter()
            .any(|b| b.kind == RichBlockKind::Math && b.formula == r"\frac{a}{b}"),
        "separate-line submission must insert a display RichBlockKind::Math block"
    );
}

#[test]
fn rich_editor_clipboard_import_and_html_export_with_tempdir() {
    let dir = TempDir::new().expect("temp dir for html export");
    let mut editor = RichEditorWindow::new_compose(PeerId::from_user(7));
    editor.set_export_dir(Some(dir.path().to_path_buf()));

    // 1. Paste HTML containing XSS vectors + valid blocks via clipboard_import.
    let html_paste = r#"
        <script>alert('xss')</script>
        <h2>Exported Article</h2>
        <p><strong>Bold</strong> and <a href="javascript:alert(1)">blocked</a> and <a href="https://example.org">safe</a>.</p>
        <ul><li>First item</li><li>Second item</li></ul>
    "#;
    let imported = editor.paste_clipboard_payload(
        ClipboardPayload {
            html: Some(html_paste.to_owned()),
            ..ClipboardPayload::default()
        },
        1.0,
    );
    assert!(imported, "HTML clipboard import must succeed");
    let page: &RichPage = editor.state().rich_page();
    assert!(
        page.blocks.iter().any(|b| b.kind == RichBlockKind::Heading),
        "expected imported Heading block"
    );
    assert!(
        page.blocks.iter().any(|b| b.kind == RichBlockKind::List),
        "expected imported List block"
    );

    // 2. HTML export sanitizes output and strips scripts/javascript:.
    let exported = editor.export_html();
    assert!(exported.html.contains("Exported Article"));
    assert!(!exported.html.contains("<script"));
    assert!(!exported.html.contains("javascript:"));
    assert!(exported.html.contains("https://example.org"));

    // 3. Export to temp directory with IvCachedMedia provider writes HTML file on disk.
    let cache = IvCachedMedia::with_defaults();
    let disk = editor
        .export_html_to_dir(dir.path(), Some(&cache))
        .expect("export HTML to temp dir");
    assert!(disk.html_path.is_file());
    let written = std::fs::read_to_string(&disk.html_path).expect("read exported html");
    assert!(written.contains("Exported Article"));

    // 4. Verify clipboard HTML helper returns sanitized document.
    let clip_html = editor.copy_clipboard_html(Some(&cache));
    assert!(clip_html.contains("Exported Article"));
    assert!(!clip_html.contains("<script"));

    // 5. Verify UI "Save as HTML" button works in kittest with configured temp dir.
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(820.0, 580.0))
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, ed: &mut RichEditorWindow| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let _ = ed.show_contents(ui, false);
                });
            },
            editor,
        );
    harness.step();
    harness.get_by_label("Markdown").click();
    harness.step();
    assert_eq!(harness.state().active_tab(), RichEditorTab::Markdown);
    harness
        .get_by_label(&tg_lang::tr::lng_context_save_html())
        .click();
    harness.step();
    assert!(
        harness.state().last_html_export().is_some(),
        "clicking Save as HTML must export to configured temp dir"
    );
}

// ============================================================================
// 4. Headless screenshot capture helper (`RUSTGRAM_SNAPSHOT_DIR` / temp dir)
// ============================================================================

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wire2_media_snapshots() {
    let _guard = WGPU_MEDIA_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_WIRE2_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_WIRE2_SNAP_STAGE"))
        .unwrap_or_else(|_| "after".to_owned());
    let out_dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wire2"));
    let sizes = [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ];

    for (tag, size) in sizes {
        // 1. Instant View window (with find bar active and matched query for "after" stage)
        let backend = MockWebViewBackend::new();
        let bundle = backend
            .fetch_iv_page(SAMPLE_IV_URL, None)
            .expect("fetch sample IV")
            .expect("sample IV bundle");
        let mut win = InstantViewWindow::new(bundle.page, bundle.media, false, None);
        if stage != "before" {
            win.open_search();
            let _ = win.set_search_query("Instant View");
        }
        let palette = tg_palette_for_mode(ThemeMode::Day);
        let state = IvFindTestState {
            win,
            backend,
            external: NoopExternalOpener,
            clipboard: NoopClipboard,
            palette,
        };
        let mut harness = HarnessBuilder::default()
            .with_size(size)
            .with_step_dt(1.0 / 30.0)
            .with_render_cursor(false)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .wgpu()
            .with_context_setup(|ctx| {
                register_tdesktop_fonts(ctx);
                let pal = tg_palette_for_mode(ThemeMode::Day);
                ctx.set_visuals(egui_visuals_from_tg_palette(&pal, false));
                ctx.all_styles_mut(|s| {
                    s.visuals.text_cursor.blink = false;
                    s.animation_time = 0.0;
                });
            })
            .build_ui_state(
                |ui, st: &mut IvFindTestState| {
                    let title = st.win.window_title();
                    let _ = st.win.render_window_contents(
                        ui,
                        &st.palette,
                        &title,
                        &mut st.backend,
                        &st.external,
                        &st.clipboard,
                    );
                },
                state,
            );
        harness.step();
        harness.step();
        let img = harness.render().expect("render IV window snapshot");
        save_image_png(&img, &out_dir.join(format!("iv_window_{tag}_{stage}.png")));

        // 2. Rich Editor window (with MathBoxState open & previewing a live formula for "after")
        let mut editor = RichEditorWindow::new_compose(PeerId::from_user(1));
        let _ = editor.state_mut().insert_text("Instant View Rich Editor");
        if stage != "before" {
            editor.open_math_box(
                r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
                false,
                Some(false),
            );
        }
        let mut ed_harness = HarnessBuilder::default()
            .with_size(size)
            .with_step_dt(1.0 / 30.0)
            .with_render_cursor(false)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .wgpu()
            .with_context_setup(|ctx| {
                register_tdesktop_fonts(ctx);
                let pal = tg_palette_for_mode(ThemeMode::Day);
                ctx.set_visuals(egui_visuals_from_tg_palette(&pal, false));
                ctx.all_styles_mut(|s| {
                    s.visuals.text_cursor.blink = false;
                    s.animation_time = 0.0;
                });
            })
            .build_ui_state(
                |ui, ed: &mut RichEditorWindow| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let _ = ed.show_contents(ui, false);
                    });
                },
                editor,
            );
        ed_harness.step();
        ed_harness.step();
        let ed_img = ed_harness.render().expect("render Rich Editor snapshot");
        save_image_png(
            &ed_img,
            &out_dir.join(format!("rich_editor_{tag}_{stage}.png")),
        );
    }
}
