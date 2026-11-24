//! WP-38b integration & headless `egui_kittest` UI tests:
//! 1. `SendFilesBox` (`AttachFilesBoxState`) **"Stash files"** button -> `ComposeStash::take_from_box`:
//!    verifies clicking "Stash files" moves the box's files + caption into `ComposeStash`, closes
//!    the box, displays the floating corner stash button, restores stashed files on exchange,
//!    enforces tdesktop's file count & size limits, and swaps any previous draft back to composer.
//! 2. Server interactive emoji animation pack (`EmojiStickersPack`, fetched via
//!    `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)` + `upload.getFile` `.tgs`):
//!    verifies real `tg-lottie` frame playback from a trivial local Lottie fixture, keycap variant
//!    indexing (`1️⃣..9️⃣`, `🔟`), colored-heart normalization, horizontal mirroring for incoming
//!    plays, and seamless procedural-burst fallback when the server pack is not loaded or corrupt.
//!
//! All tests run strictly against in-process `FakeRpcClient` and locally constructed fixtures
//! (no Telegram assets, no network calls, no real accounts).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Message, MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags,
    TextWithEntities as DataText, User, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::history::compose_boxes::AttachFilesBoxState;
use tg_app::history::compose_stash::{
    ComposeStash, ComposeStashKey, STASH_FILES_LIMIT, STASH_FILES_MEMORY_LIMIT,
};
use tg_app::history::emoji_interactions::HistoryViewEmojiInteractions;
use tg_app::history::rich_compose::RichThreadDraft;
use tg_app::live::LiveSession;
use tg_app::live::chat_l233::apply_updates_for_live_session_at;
use tg_app::live::emoji_pack::{
    EmojiStickersPack, ensure_emoji_animation_downloaded, fetch_emoji_interactions_pack,
    index_from_emoticon, snapshot_emoji_stickers_pack,
};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::{Palette, ThemeMode};
use tg_app::stand_in::types::{MsgId, PeerId};

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = request.to_bytes();
        let ctor = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, raw));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(expected, _)| *expected == ctor)
                .and_then(|idx| q.remove(idx))
        };
        match next {
            Some((_expected, outcome)) => {
                let bytes = outcome?;
                R::Return::from_bytes(&bytes)
                    .map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("NO_FAKE_RESPONSE_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, PeerId) {
    let rpc = FakeRpcClient::new(2);
    let self_uid = DataUserId(10_001);
    let friend_uid = 41_800u64;
    let friend_dp = DataPeerId::from_user(DataUserId(friend_uid));

    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let rt = SessionRuntime::start(
        self_uid,
        tg_app::live::ErasedRpcClient::new(rpc.clone()),
        cfg,
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(rt), 1);

    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;

        let mut self_u = User::new(self_uid, true);
        self_u.first_name = "LocalTester".to_owned();
        self_u.access_hash = 111;
        self_u.flags.insert(UserFlags::SELF);
        s.upsert_user(self_u, PeerUpdateFlags::NAME);

        let mut friend_u = User::new(DataUserId(friend_uid), false);
        friend_u.first_name = "DemoContact".to_owned();
        friend_u.access_hash = 222;
        s.upsert_user(friend_u, PeerUpdateFlags::NAME);

        // Mark history as loaded at top & bottom so load_slice_around doesn't try GetHistory RPC.
        let h = s.ensure_history(friend_dp);
        h.loaded_ranges.add_slice(
            std::iter::empty(),
            tg_data::MsgRange::new(DataMsgId(0), tg_data::SERVER_MAX_MSG_ID),
            Some(0),
        );

        let emoji_msg = Message::new_text(
            friend_dp,
            DataMsgId(77),
            Some(friend_dp),
            1_735_000_100,
            DataText::plain("🎆"),
        );
        s.insert_message(emoji_msg);
    });

    (live, rpc, PeerId::from_user(friend_uid))
}

/// Locally created trivial Lottie JSON (a single pulsing circle; no external or Telegram assets).
fn local_trivial_lottie_json(name: &str) -> String {
    format!(
        r#"{{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":128,"h":128,"nm":"{name}","ddd":0,"assets":[],"layers":[{{"ddd":0,"ind":1,"ty":4,"nm":"dot","sr":1,"ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":0,"k":[64,64,0]}},"a":{{"a":0,"k":[0,0,0]}},"s":{{"a":1,"k":[{{"t":0,"s":[40,40,100],"e":[100,100,100]}},{{"t":30,"s":[100,100,100]}}]}}}},"ao":0,"shapes":[{{"ty":"el","d":1,"s":{{"a":0,"k":[60,60]}},"p":{{"a":0,"k":[0,0]}}}},{{"ty":"fl","c":{{"a":0,"k":[0.95,0.55,0.2,1]}},"o":{{"a":0,"k":100}}}}],"ip":0,"op":30,"st":0}}]}}"#
    )
}

/// Wraps raw bytes in a valid RFC 1952 gzip stream (stored deflate blocks) for `.tgs` fixtures.
fn gzip_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF]);
    let mut offset = 0;
    loop {
        let remaining = raw.len().saturating_sub(offset);
        let chunk_len = remaining.min(65_535);
        let is_final = offset + chunk_len >= raw.len();
        out.push(if is_final { 0x01 } else { 0x00 });
        let len_u16 = chunk_len as u16;
        out.extend_from_slice(&len_u16.to_le_bytes());
        out.extend_from_slice(&(!len_u16).to_le_bytes());
        out.extend_from_slice(&raw[offset..offset + chunk_len]);
        offset += chunk_len;
        if is_final {
            break;
        }
    }
    let crc = tg_theme::crc32(raw);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out
}

fn make_tl_tgs_doc(id: i64, alt: &str) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 17,
        file_reference: vec![9, 8, 7, (id & 0xFF) as u8],
        date: 1_735_000_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 512,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 512,
            }),
            tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                mask: false,
                alt: alt.to_owned(),
                stickerset: tl_enums::InputStickerSet::Empty,
                mask_coords: None,
            }),
        ],
    })
}

fn run_egui_frame(ctx: &egui::Context, input: egui::RawInput, draw: impl FnMut(&mut egui::Ui)) {
    let mut draw = draw;
    let mut out = ctx.run_ui(input, |ui| {
        draw(ui);
    });
    out.textures_delta.clear();
}

// =============================================================================
// 1. SendFilesBox "Stash files" button & `ComposeStash::take_from_box`
// =============================================================================

#[test]
fn wp38b_send_files_box_stash_files_button_moves_files_and_closes_box() {
    // 1. Unit verification of `ComposeStash::take_from_box` and tdesktop limits.
    let mut unit_box = AttachFilesBoxState::new(false, false);
    unit_box.caption = "  Quarterly deck  ".to_owned();
    unit_box.spoiler = true;
    let stashed_unit =
        tg_app::history::compose_stash::take_stash_from_box(&mut unit_box).expect("take_from_box");
    assert_eq!(stashed_unit.files.len(), 2);
    assert_eq!(stashed_unit.files[0].name, "workspace_overview.jpg");
    assert_eq!(stashed_unit.files[1].name, "architecture_diagram.png");
    assert_eq!(stashed_unit.draft.text.text, "Quarterly deck");
    assert!(unit_box.prepared.files.is_empty());
    assert!(unit_box.candidates.is_empty());
    assert!(!unit_box.spoiler);
    assert!(unit_box.caption.is_empty());
    assert!(
        tg_app::history::compose_stash::take_stash_from_box(&mut unit_box).is_err(),
        "empty box must return error"
    );

    // Count limit (> 100 files) & size limit (> 256 MiB) must be rejected without draining box.
    let mut over_count_box = AttachFilesBoxState::new(false, false);
    for i in 0..STASH_FILES_LIMIT {
        over_count_box.add_sample_document(&format!("doc_{i}.txt"), "text/plain", 128);
    }
    assert!(over_count_box.prepared.files.len() > STASH_FILES_LIMIT);
    assert!(tg_app::history::compose_stash::take_stash_from_box(&mut over_count_box).is_err());
    assert!(
        over_count_box.prepared.files.len() > STASH_FILES_LIMIT,
        "failed stash must leave files untouched"
    );

    let mut over_size_box = AttachFilesBoxState::new(false, false);
    over_size_box.add_sample_document(
        "huge_archive.bin",
        "application/octet-stream",
        (STASH_FILES_MEMORY_LIMIT + 1) as i64,
    );
    assert!(tg_app::history::compose_stash::take_stash_from_box(&mut over_size_box).is_err());

    // 2. Live UI click test via `egui_kittest::Harness` on `HistoryWidget`.
    let (live, fake, friend_peer) = make_live_session();
    let stash_key = ComposeStashKey::main(friend_peer);

    // Seed a previous text stash so we also verify tdesktop's swap-back on `takeFromBox`.
    let previous_text = "Previously stashed note";
    let mut initial_live = live;
    initial_live.set_compose_stash(
        stash_key,
        Some(ComposeStash {
            draft: RichThreadDraft {
                text: TextWithEntities::simple(previous_text),
                ..RichThreadDraft::default()
            },
            ..ComposeStash::default()
        }),
    );

    let mut widget = HistoryWidget::new();
    widget.open_attach_box(false);
    {
        let box_state = widget.attach_box_mut().expect("attach box open");
        box_state.add_sample_document("release_notes.pdf", "application/pdf", 340_000);
        box_state.caption = "Attach stash caption".to_owned();
    }
    assert!(widget.is_attach_box_open());

    struct UiHarnessState {
        widget: HistoryWidget,
        live: LiveSession,
        peer_id: PeerId,
        palette: Palette,
        strings: EnglishStrings,
    }

    let state = UiHarnessState {
        widget,
        live: initial_live,
        peer_id: friend_peer,
        palette: Palette::for_mode(ThemeMode::Day),
        strings: EnglishStrings,
    };

    let mut harness = Harness::builder()
        .with_size(Vec2::new(800.0, 640.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UiHarnessState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let _ = st.widget.show_with_backend(
                            ui,
                            st.peer_id,
                            &mut st.live,
                            false,
                            false,
                            false,
                            &st.palette,
                            &st.strings,
                        );
                    });
            },
            state,
        );

    harness.step();
    // Enqueue `messages.saveDraft` OK responses for clearing composer + restoring previous draft.
    fake.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    // Locate and click the "Stash files" button inside the live SendFilesBox modal.
    harness.get_by_label("Stash files").click();
    harness.step();
    harness.step();

    // Box must be closed, files moved into ComposeStash, and previous text restored to composer.
    {
        let st = harness.state_mut();
        assert!(
            !st.widget.is_attach_box_open(),
            "clicking 'Stash files' must close SendFilesBox"
        );
        let saved_stash = st
            .live
            .compose_stash(stash_key)
            .expect("compose stash must hold stashed files");
        assert!(saved_stash.has_files());
        assert_eq!(saved_stash.files.len(), 3);
        assert_eq!(saved_stash.files[0].name, "workspace_overview.jpg");
        assert_eq!(saved_stash.files[1].name, "architecture_diagram.png");
        assert_eq!(saved_stash.files[2].name, "release_notes.pdf");
        assert_eq!(saved_stash.files[2].mime_type, "application/pdf");
        assert_eq!(saved_stash.files[2].size_bytes, 340_000);
        assert_eq!(saved_stash.draft.text.text, "Attach stash caption");
        assert_eq!(
            st.widget.compose_mut().text,
            previous_text,
            "previous stashed draft must be restored to composer when stashing files"
        );
    }

    // Floating corner stash button should now be visible in the live window.
    assert!(
        harness.query_by_label("Restore stashed draft").is_some(),
        "corner stash button should appear after stashing files"
    );

    // Swapping back via `exchange_compose_stash` re-opens SendFilesBox with the 3 stashed files.
    fake.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    let st_mut = harness.state_mut();
    assert!(
        st_mut
            .widget
            .exchange_compose_stash(friend_peer, &mut st_mut.live, 5.0)
    );
    assert!(
        st_mut.widget.is_attach_box_open(),
        "exchanging a file stash must re-open SendFilesBox"
    );
    let restored_box = st_mut.widget.attach_box_mut().expect("restored attach box");
    assert_eq!(restored_box.prepared.files.len(), 3);
    assert_eq!(restored_box.caption, "Attach stash caption");
}

// =============================================================================
// 2. Server `EmojiStickersPack` Lottie Interactions & Procedural Fallback
// =============================================================================

#[test]
fn wp38b_emoji_interactions_server_pack_lottie_and_procedural_fallback() {
    // 1. Keycap variant index parsing matches tdesktop `IndexFromEmoticon`.
    assert_eq!(index_from_emoticon("1\u{FE0F}\u{20E3}"), Some(0));
    assert_eq!(index_from_emoticon("2\u{20E3}"), Some(1));
    assert_eq!(index_from_emoticon("9\u{FE0F}\u{20E3}"), Some(8));
    assert_eq!(index_from_emoticon("\u{1F51F}"), Some(9)); // 🔟
    assert_eq!(index_from_emoticon("🎆"), None);
    assert_eq!(index_from_emoticon("1"), None);

    // 2. Fallback behavior BEFORE the server pack is loaded.
    let (mut live, fake, friend_peer) = make_live_session();
    let friend_uid = friend_peer.bare() as i64;
    let ctx = egui::Context::default();
    let colors =
        tg_app::history::theme::HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));

    let mut fallback_overlay = HistoryViewEmojiInteractions::new();
    let uncached = EmojiStickersPack::new().resolve_lottie(None, "🎆", 0);
    assert!(uncached.is_none());
    fallback_overlay.play_with_lottie(
        MsgId(77),
        "🎆".to_owned(),
        Pos2::new(220.0, 220.0),
        true,
        0,
        uncached,
        1_000,
    );
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        fallback_overlay.paint_overlays(
            ui,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            1_200,
            &colors,
        );
    });
    assert_eq!(fallback_overlay.active_lottie_count(), 0);
    assert_eq!(fallback_overlay.lottie_frames_painted(), 0);
    assert_eq!(
        fallback_overlay.fallback_frames_painted(),
        1,
        "procedural burst fallback must paint when EmojiStickersPack is not loaded"
    );

    // Corrupt Lottie JSON must also fall back safely without panicking.
    fallback_overlay.clear();
    fallback_overlay.play_with_lottie(
        MsgId(77),
        "🎆".to_owned(),
        Pos2::new(220.0, 220.0),
        false,
        0,
        Some((99999, "{not-valid-json".to_owned())),
        2_000,
    );
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        fallback_overlay.paint_overlays(
            ui,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            2_150,
            &colors,
        );
    });
    assert_eq!(fallback_overlay.active_lottie_count(), 0);
    assert_eq!(fallback_overlay.fallback_frames_painted(), 2);

    // 3. Fake RPC serving `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)`.
    let docs = vec![
        make_tl_tgs_doc(77_001, "🎆"),
        make_tl_tgs_doc(77_002, "🎆"),
        make_tl_tgs_doc(77_003, "❤"),
        make_tl_tgs_doc(77_004, "🚀"),
    ];
    let packs = vec![
        // Keycap index packs (`1️⃣` -> idx 0, `2️⃣` -> idx 1, `🔟` -> idx 9).
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "1\u{FE0F}\u{20E3}".to_owned(),
            documents: vec![77_001, 77_003],
        }),
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "2\u{FE0F}\u{20E3}".to_owned(),
            documents: vec![77_002],
        }),
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "\u{1F51F}".to_owned(),
            documents: vec![77_004],
        }),
        // Emoticon packs binding emojis to those documents.
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "🎆".to_owned(),
            documents: vec![77_001, 77_002],
        }),
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "❤️".to_owned(),
            documents: vec![77_003],
        }),
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "🚀".to_owned(),
            documents: vec![77_004],
        }),
    ];
    let sticker_set_resp =
        tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: true,
                masks: false,
                emojis: false,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 88_100,
                access_hash: 88_200,
                title: "EmojiAnimations".to_owned(),
                short_name: "AnimatedEmojiAnimations".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 4,
                hash: 54_321,
            }),
            packs,
            keywords: Vec::new(),
            documents: docs,
        });
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &sticker_set_resp,
    );

    let mapped = fetch_emoji_interactions_pack(&live).expect("fetch EmojiStickersPack");
    assert_eq!(mapped, 4);

    // Verify wire request carried `InputStickerSet::AnimatedEmojiAnimations`.
    let get_set_reqs = fake.recorded_of(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID);
    assert_eq!(get_set_reqs.len(), 1);
    let sent_req = tl_fns::messages::GetStickerSet::from_bytes(&get_set_reqs[0])
        .expect("decode GetStickerSet");
    assert!(matches!(
        sent_req.stickerset,
        tl_enums::InputStickerSet::AnimatedEmojiAnimations
    ));
    assert_eq!(sent_req.hash, 0);

    let pack_snap = snapshot_emoji_stickers_pack(&live);
    assert!(pack_snap.is_loaded_from_server());
    assert_eq!(pack_snap.variant_count("🎆"), 2);
    assert_eq!(
        pack_snap.variant_count("💙"),
        1,
        "colored heart should resolve to red heart server animation"
    );
    assert_eq!(pack_snap.variant_count("🚀"), 1);
    assert_eq!(
        live.emoji_interaction_variant_count("🎆"),
        Some(2),
        "HistoryBackend should report server pack variant count"
    );

    // 4. Download trivial local Lottie fixture (.tgs gzip) via fake `upload.getFile`.
    let lottie_json = local_trivial_lottie_json("local_fireworks_fixture");
    let tgs_bytes = gzip_stored(lottie_json.as_bytes());
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_735_000_000,
            bytes: tgs_bytes,
        }),
    );

    let (doc_id, downloaded_json) =
        ensure_emoji_animation_downloaded(&live, "🎆", 0).expect("download .tgs fixture");
    assert_eq!(doc_id, 77_001);
    assert_eq!(downloaded_json, lottie_json);
    assert_eq!(
        live.resolve_emoji_interaction_lottie("🎆", 0),
        Some((77_001, lottie_json.clone()))
    );

    // 5. End-to-end live HistoryWidget test: incoming interaction plays real server Lottie!
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(10_000, |d| d.as_millis() as u64);
    let incoming_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
            user_id: friend_uid,
            top_msg_id: None,
            action: tl_enums::SendMessageAction::SendMessageEmojiInteraction(
                tl_types::SendMessageEmojiInteraction {
                    emoticon: "🎆".to_owned(),
                    msg_id: 77,
                    interaction: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                        data: r#"{"v":1,"a":[{"i":1,"t":0.0}]}"#.to_owned(),
                    }),
                },
            ),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_000_300,
        seq: 1,
    });
    assert!(apply_updates_for_live_session_at(
        &live,
        &incoming_upd,
        now_ms
    ));

    // Enqueue `messages.setTyping` OK for the automatic `SendMessageEmojiInteractionSeen` ack.
    fake.enqueue_ok(tl_fns::messages::SetTyping::CONSTRUCTOR_ID, &true);

    let mut widget = HistoryWidget::new();
    let pal = Palette::for_mode(ThemeMode::Day);
    let strings = EnglishStrings;
    let raw_input = egui::RawInput {
        time: Some(10.0),
        ..Default::default()
    };
    run_egui_frame(&ctx, raw_input, |ui| {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 560.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let _ = widget.show_with_backend(
                ui,
                friend_peer,
                &mut live,
                false,
                false,
                false,
                &pal,
                &strings,
            );
        });
    });

    let overlay = widget.inner_mut().emoji_interactions();
    assert_eq!(
        overlay.active_lottie_count(),
        1,
        "HistoryWidget must play server Lottie animation from EmojiStickersPack"
    );
    assert!(
        overlay.lottie_frames_painted() >= 1,
        "at least one real tg-lottie frame must be painted"
    );
    assert_eq!(
        overlay.fallback_frames_painted(),
        0,
        "procedural fallback must not be used when server Lottie is loaded"
    );

    // 6. Verify non-looping Lottie overlay completes after its composition duration (30f @ 30fps = 1000ms).
    let mut timed_overlay = HistoryViewEmojiInteractions::new();
    timed_overlay.play_with_lottie(
        MsgId(77),
        "🎆".to_owned(),
        Pos2::new(240.0, 240.0),
        false, // incoming -> exercises horizontal mirroring path
        0,
        Some((77_001, lottie_json)),
        5_000,
    );
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        timed_overlay.paint_overlays(
            ui,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            5_500,
            &colors,
        );
    });
    assert_eq!(timed_overlay.active_lottie_count(), 1);
    assert_eq!(timed_overlay.lottie_frames_painted(), 1);
    run_egui_frame(&ctx, egui::RawInput::default(), |ui| {
        timed_overlay.paint_overlays(
            ui,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            6_100,
            &colors,
        );
    });
    assert_eq!(
        timed_overlay.active_count(),
        0,
        "non-looping server Lottie interaction should finish after its 1000ms duration"
    );
}
