//! WP-77 verification suite (fake-only, zero real network traffic):
//! 1. Unread reactions: `HistoryBackend::mark_visible_reactions_read` queues
//!    `messages.readMessageContents` (DM/group, applying `AffectedMessages` PTS) or
//!    `channels.readMessageContents` (supergroup/channel), clears local `HAS_UNREAD_REACTION` /
//!    recent-reactor unread flags, decrements/zeros `unread_things_counts` (floating "♥ N" button)
//!    and clears the chat-row heart badge while preserving unrelated `MEDIA_IS_UNREAD`.
//! 2. Reaction strip order: `messages.getTopReactions` wired via `tg_api::cached_lists::ReactionsSync`
//!    (cached by 64-bit hash, `NotModified` preservation, error hash reset) and exposed via
//!    `HistoryBackend::top_reactions` and `HistoryBackend::reaction_strip_emojis`.
//! 3. Dialogs backend: `mute_dialog_for` (timed / forever / unmute), `dialog_mute_until`,
//!    `dialog_notify_sound`, `set_dialog_sound_enabled`, `set_dialog_notify_sound`,
//!    `dialog_suggests_report_spam`, `report_spam_and_block`, `delete_chat`, and
//!    `unblock_and_restart_bot`.
//! 4. Profile: `PeerProfile.group_call` (from `ChatFull.call` / `ChannelFull.call`, `CALL_ACTIVE`,
//!    `canManageGroupCall`, and live `updateGroupCall` active/discarded updates) and non-blocking
//!    async `InfoBackend::shared_media_thumb` for photo and video thumbnails.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::convert::{apply_tl_chat_full, apply_tl_users_and_chats};
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::history::backend::{HistoryBackend, UnreadThingsScope};
use tg_app_core::info::backend::{InfoBackend, ProfileGroupCallState, SharedMediaThumb};
use tg_app_core::notifications::settings::NotifySound;
use tg_app_core::stand_in::types::{MsgId as UiMsgId, PeerId as UiPeerId};
use tg_data::{
    AllowedReactions, AllowedReactionsType, BlockStatus, BotInfo, Channel, ChannelFlags,
    ChannelId as DataChannelId, ChatAdminRights, DocumentData, DocumentId, DocumentKind,
    FullMsgId as DataFullMsgId, LoadedStatus, Message, MessageContent, MessageFlags, MessageMedia,
    MessageReactionCount, MessageReactionsSummary, MsgId as DataMsgId, PeerBarSettings,
    PeerId as DataPeerId, PeerUpdateFlags, PhotoData, PhotoId, PhotoSize, ReactionId,
    RecentReactor, TextWithEntities, User, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::{Notify, broadcast};

use crate::session::LiveSession;

// =============================================================================
// Gated FakeRpcClient
// =============================================================================

type RecordedRpc = (u32, Vec<u8>);
type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp77FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    upload_gate: Arc<Mutex<Option<Arc<Notify>>>>,
    upload_started: Arc<Mutex<Option<Arc<Notify>>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp77FakeRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            upload_gate: Arc::new(Mutex::new(None)),
            upload_started: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn set_upload_gate(&self, gate: Option<Arc<Notify>>, started: Option<Arc<Notify>>) {
        *self.upload_gate.lock().expect("lock gate") = gate;
        *self.upload_started.lock().expect("lock started") = started;
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded_decoded<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == R::CONSTRUCTOR_ID)
            .map(|(_, bytes)| R::from_bytes(bytes).expect("decode recorded RPC"))
            .collect()
    }
}

impl RpcClient for Wp77FakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let started = self.upload_started.lock().expect("lock started").clone();
            let gate = self.upload_gate.lock().expect("lock gate").clone();
            if let Some(s) = started {
                s.notify_one();
            }
            if let Some(g) = gate {
                g.notified().await;
            }
        }

        let reply = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };

        let raw = match reply {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(e),
            None => return Err(InvocationError::Dropped),
        };
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn make_live(is_premium: bool) -> (tokio::runtime::Runtime, Wp77FakeRpc, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio rt");
    let rpc = Wp77FakeRpc::new();
    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        tg_app_core::core_types::erased_rpc::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            init_updates_state: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start runtime");
    let live = LiveSession::new(Arc::new(runtime), 77_001);
    live.write_session(|s| {
        let mut me = User::new(DataUserId(100), true);
        me.first_name = "Self".to_owned();
        if is_premium {
            me.flags.insert(UserFlags::PREMIUM);
        }
        s.upsert_user(me, PeerUpdateFlags::NAME);
    });
    (rt, rpc, live)
}

fn synthetic_tl_doc(id: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 13,
        file_reference: vec![1, 2, 3],
        date: 1_700_000_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 128,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_000,
        seq: 0,
    })
}

// =============================================================================
// 1. Unread reactions (`mark_visible_reactions_read`)
// =============================================================================

#[test]
fn unread_reactions_dm_visible_marks_read_sends_messages_read_contents_feeds_pts_and_clears_badges()
{
    let (_rt, rpc, mut live) = make_live(false);
    let data_peer = DataPeerId::from_user(DataUserId(201));
    let ui_peer = UiPeerId::from_user(201);

    live.write_session(|s| {
        let mut u = User::new(DataUserId(201), false);
        u.first_name = "Alice".to_owned();
        u.access_hash = 0x201;
        s.upsert_user(u, PeerUpdateFlags::NAME);

        for mid in [10i64, 11i64] {
            let mut m = Message::new_text(
                data_peer,
                DataMsgId(mid),
                Some(DataPeerId::from_user(DataUserId(100))),
                1_750_000_000 + mid as i32,
                TextWithEntities::plain("hi"),
            );
            m.flags |= MessageFlags::OUTGOING | MessageFlags::HAS_UNREAD_REACTION;
            m.reactions = Some(MessageReactionsSummary {
                results: vec![MessageReactionCount {
                    id: ReactionId::Emoji("❤️".to_owned()),
                    count: 1,
                    chosen_order: None,
                }],
                recent_reactors: vec![RecentReactor {
                    peer_id: data_peer,
                    reaction: ReactionId::Emoji("❤️".to_owned()),
                    date: 1_750_000_050,
                    unread: true,
                    my: false,
                }],
                ..Default::default()
            });
            s.insert_message(m);
        }
        let h = s.ensure_history(data_peer);
        h.in_chat_list = true;
        h.unread_reactions_count = 2;
        h.unread_reactions.insert(DataMsgId(10));
        h.unread_reactions.insert(DataMsgId(11));
        s.sync_dialog_lists(data_peer);
    });
    live.seed_updates_pts(200, None);

    let scope = UnreadThingsScope::history(ui_peer);
    assert_eq!(live.unread_things_counts(scope).reactions, 2);
    assert!(
        live.dialog_by_peer(ui_peer)
            .expect("dialog present")
            .badges
            .reaction,
        "chat-row heart badge must be visible before reading"
    );

    // Marking visible message 10 sends `messages.readMessageContents([10])` and feeds PTS.
    rpc.enqueue_ok(
        tl_fns::messages::ReadMessageContents::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 201,
                pts_count: 1,
            },
        ),
    );

    let marked = live
        .mark_visible_reactions_read(ui_peer, &[UiMsgId(10)])
        .expect("mark_visible_reactions_read ok");
    // The request goes out on the background queue; wait for it before checking the server side.
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(marked, vec![UiMsgId(10)]);
    assert_eq!(live.updates_common_pts(), 201);
    assert_eq!(live.unread_things_counts(scope).reactions, 1);

    let (m10_unread, m10_recent_unread, m11_unread) = live.read_session(|s| {
        let m10 = s
            .message(DataFullMsgId::new(data_peer, DataMsgId(10)))
            .expect("msg 10");
        let m11 = s
            .message(DataFullMsgId::new(data_peer, DataMsgId(11)))
            .expect("msg 11");
        (
            m10.has_unread_reaction(),
            m10.reactions
                .as_ref()
                .is_some_and(|r| r.recent_reactors.iter().any(|rr| rr.unread)),
            m11.has_unread_reaction(),
        )
    });
    assert!(!m10_unread, "msg 10 HAS_UNREAD_REACTION must be cleared");
    assert!(
        !m10_recent_unread,
        "msg 10 recent_reactors.unread must be cleared"
    );
    assert!(m11_unread, "msg 11 must remain unread until shown");

    // Re-calling on already-read msg 10 is idempotent (zero extra RPCs).
    let repeat = live
        .mark_visible_reactions_read(ui_peer, &[UiMsgId(10)])
        .expect("idempotent call");
    assert!(repeat.is_empty());
    assert_eq!(
        rpc.recorded_decoded::<tl_fns::messages::ReadMessageContents>()
            .len(),
        1
    );

    // Now showing msg 11 clears the remaining unread reaction, floating "♥ N" button, and row badge.
    rpc.enqueue_ok(
        tl_fns::messages::ReadMessageContents::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 202,
                pts_count: 1,
            },
        ),
    );
    let marked2 = live
        .mark_visible_reactions_read(ui_peer, &[UiMsgId(11)])
        .expect("mark msg 11");
    // The request goes out on the background queue; wait for it before checking the server side.
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(marked2, vec![UiMsgId(11)]);
    assert_eq!(live.updates_common_pts(), 202);
    assert_eq!(
        live.unread_things_counts(scope).reactions,
        0,
        "floating ♥ N button count must reach 0"
    );
    assert!(
        !live
            .dialog_by_peer(ui_peer)
            .expect("dialog present")
            .badges
            .reaction,
        "chat-row heart badge must clear once all unread reactions are read"
    );

    let reqs = rpc.recorded_decoded::<tl_fns::messages::ReadMessageContents>();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].id, vec![10]);
    assert_eq!(reqs[1].id, vec![11]);
    assert!(
        rpc.recorded_decoded::<tl_fns::channels::ReadMessageContents>()
            .is_empty()
    );
}

#[test]
fn unread_reactions_channel_visible_sends_channels_read_contents_and_preserves_media_unread() {
    let (_rt, rpc, mut live) = make_live(false);
    let ch_id = DataChannelId(5001);
    let data_peer = DataPeerId::from_channel(ch_id);
    let ui_peer = UiPeerId::from_channel(5001);

    live.write_session(|s| {
        let mut ch = Channel::new(ch_id);
        ch.common.name = "Dev Group".to_owned();
        ch.access_hash = 0x5001;
        ch.flags = ChannelFlags::MEGAGROUP;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);

        let mut m = Message::new_text(
            data_peer,
            DataMsgId(50),
            Some(DataPeerId::from_user(DataUserId(201))),
            1_750_000_100,
            TextWithEntities::plain("voice with reaction"),
        );
        m.flags |= MessageFlags::HAS_UNREAD_REACTION | MessageFlags::MEDIA_IS_UNREAD;
        s.insert_message(m);

        let h = s.ensure_history(data_peer);
        h.in_chat_list = true;
        h.unread_reactions_count = 1;
        h.unread_reactions.insert(DataMsgId(50));
        s.sync_dialog_lists(data_peer);
    });

    rpc.enqueue_ok(tl_fns::channels::ReadMessageContents::CONSTRUCTOR_ID, &true);

    // Include a negative pending local msg id (-5) which must be ignored.
    let marked = live
        .mark_visible_reactions_read(ui_peer, &[UiMsgId(-5), UiMsgId(50), UiMsgId(50)])
        .expect("channel mark_visible_reactions_read ok");
    // The request goes out on the background queue; wait for it before checking the server side.
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(marked, vec![UiMsgId(50)]);

    let ch_reqs = rpc.recorded_decoded::<tl_fns::channels::ReadMessageContents>();
    assert_eq!(ch_reqs.len(), 1);
    assert_eq!(ch_reqs[0].id, vec![50]);
    assert!(
        rpc.recorded_decoded::<tl_fns::messages::ReadMessageContents>()
            .is_empty()
    );

    let (has_unread_reaction, media_still_unread) = live.read_session(|s| {
        let m = s
            .message(DataFullMsgId::new(data_peer, DataMsgId(50)))
            .expect("msg 50");
        (
            m.has_unread_reaction(),
            m.flags.contains(MessageFlags::MEDIA_IS_UNREAD),
        )
    });
    assert!(!has_unread_reaction);
    assert!(
        media_still_unread,
        "marking visible reactions read must not clear unlistened voice MEDIA_IS_UNREAD"
    );
    assert_eq!(
        live.unread_things_counts(UnreadThingsScope::history(ui_peer))
            .reactions,
        0
    );
    assert!(
        !live
            .dialog_by_peer(ui_peer)
            .expect("channel dialog")
            .badges
            .reaction
    );
}

// =============================================================================
// 2. Reaction strip order (`messages.getTopReactions`)
// =============================================================================

#[test]
fn top_reactions_cached_by_hash_not_modified_and_strip_order_respects_favorite_and_peer_allowed() {
    let (_rt, rpc, live) = make_live(false);

    // Seed a channel whose allowed reactions are restricted to Some(["🎉", "👎"]).
    let ch_id = DataChannelId(6001);
    let ch_ui = UiPeerId::from_channel(6001);
    live.write_session(|s| {
        let mut ch = Channel::new(ch_id);
        ch.common.name = "Restricted Reactions".to_owned();
        ch.access_hash = 0x6001;
        ch.flags = ChannelFlags::MEGAGROUP;
        ch.common.loaded_status = LoadedStatus::Full;
        ch.allowed_reactions = AllowedReactions {
            kind: AllowedReactionsType::Some,
            some: vec![
                ReactionId::Emoji("🎉".to_owned()),
                ReactionId::Emoji("👎".to_owned()),
            ],
            max_count: 11,
            paid_enabled: false,
        };
        s.upsert_channel(ch, PeerUpdateFlags::FULL_INFO);
    });

    // Enqueue GetTopReactions (server order: 🔥, 🎉, ❤️) and GetAvailableReactions.
    rpc.enqueue_ok(
        tl_fns::messages::GetTopReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::Reactions(tl_types::messages::Reactions {
            hash: 9_876_543_210,
            reactions: vec![
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "🔥".to_owned(),
                }),
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "🎉".to_owned(),
                }),
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "❤️".to_owned(),
                }),
            ],
        }),
    );
    let make_avail = |emoji: &str, premium: bool| {
        tl_enums::AvailableReaction::AvailableReaction(tl_types::AvailableReaction {
            inactive: false,
            premium,
            reaction: emoji.to_owned(),
            title: emoji.to_owned(),
            static_icon: synthetic_tl_doc(1),
            appear_animation: synthetic_tl_doc(2),
            select_animation: synthetic_tl_doc(3),
            activate_animation: synthetic_tl_doc(4),
            effect_animation: synthetic_tl_doc(5),
            around_animation: None,
            center_icon: None,
        })
    };
    rpc.enqueue_ok(
        tl_fns::messages::GetAvailableReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::AvailableReactions::AvailableReactions(
            tl_types::messages::AvailableReactions {
                hash: 777,
                reactions: vec![
                    make_avail("👍", false),
                    make_avail("👎", false),
                    make_avail("❤️", false),
                    make_avail("🔥", false),
                    make_avail("🎉", false),
                    make_avail("🐳", true),
                ],
            },
        ),
    );

    // 1. First top_reactions() fetches GetTopReactions(limit=14, hash=0).
    let top = live.top_reactions();
    assert_eq!(top, vec!["🔥", "🎉", "❤️"]);

    // 2. Second top_reactions() hits cache (zero extra RPCs).
    let top_cached = live.top_reactions();
    assert_eq!(top_cached, vec!["🔥", "🎉", "❤️"]);
    assert_eq!(
        rpc.recorded_decoded::<tl_fns::messages::GetTopReactions>()
            .len(),
        1
    );

    // 3. Explicit refresh sends cached hash and preserves cache on NotModified.
    rpc.enqueue_ok(
        tl_fns::messages::GetTopReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::NotModified,
    );
    let refreshed = live.refresh_top_reactions_live(true);
    assert_eq!(refreshed, vec!["🔥", "🎉", "❤️"]);

    // 4. Reaction strip order: favorite ("👍") rotated to front, then top ("🔥", "🎉", "❤️"),
    //    then remaining active non-premium available ("👎"), excluding premium "🐳".
    let strip = live.reaction_strip_emojis(None);
    assert_eq!(strip, vec!["👍", "🔥", "🎉", "❤️", "👎"]);

    // 5. Peer with AllowedReactionsType::Some(["🎉", "👎"]) keeps top order before available.
    let peer_strip = live.reaction_strip_emojis(Some(ch_ui));
    assert_eq!(peer_strip, vec!["🎉", "👎"]);

    // 6. RPC error resets top_hash = 0 on the subsequent refresh (`data_message_reactions.cpp:989`).
    rpc.enqueue_err(
        tl_fns::messages::GetTopReactions::CONSTRUCTOR_ID,
        InvocationError::Dropped,
    );
    let _ = live.refresh_top_reactions_live(true);
    rpc.enqueue_ok(
        tl_fns::messages::GetTopReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::NotModified,
    );
    let _ = live.refresh_top_reactions_live(true);

    let top_reqs = rpc.recorded_decoded::<tl_fns::messages::GetTopReactions>();
    assert_eq!(top_reqs.len(), 4);
    assert_eq!(top_reqs[0].limit, 14);
    assert_eq!(top_reqs[0].hash, 0);
    assert_eq!(top_reqs[1].hash, 9_876_543_210);
    assert_eq!(top_reqs[2].hash, 9_876_543_210);
    assert_eq!(
        top_reqs[3].hash, 0,
        "failed refresh must reset top_hash to 0"
    );
}

// =============================================================================
// 3. Dialogs backend: mute_dialog_for, sound on/off, per-chat tone,
//    report_spam_and_block, delete_chat, unblock_and_restart_bot
// =============================================================================

#[test]
fn mute_dialog_for_timed_forever_unmute_sound_toggle_and_per_chat_tone() {
    let (_rt, rpc, mut live) = make_live(false);
    let data_peer = DataPeerId::from_user(DataUserId(301));
    let ui_peer = UiPeerId::from_user(301);

    live.write_session(|s| {
        let mut u = User::new(DataUserId(301), false);
        u.first_name = "Bob".to_owned();
        u.access_hash = 0x301;
        s.upsert_user(u, PeerUpdateFlags::NAME);
        let h = s.ensure_history(data_peer);
        h.in_chat_list = true;
        s.sync_dialog_lists(data_peer);
    });

    // 1. Mute for 1 hour (3600 seconds)
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    let muted_1h = live
        .mute_dialog_for(ui_peer, 3600)
        .expect("mute for 1h succeeds");
    assert!(muted_1h);
    let until_1h = live.dialog_mute_until(ui_peer).expect("mute_until set");
    assert!(
        until_1h < i32::MAX,
        "timed mute must not be i32::MAX, got {until_1h}"
    );
    assert!(live.dialog_by_peer(ui_peer).expect("dialog").muted);

    // 2. Mute forever (i32::MAX)
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    let muted_forever = live
        .mute_dialog_for(ui_peer, i32::MAX)
        .expect("mute forever succeeds");
    assert!(muted_forever);
    assert_eq!(live.dialog_mute_until(ui_peer), Some(i32::MAX));

    // 3. Unmute (seconds == 0)
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    let muted_after_unmute = live.mute_dialog_for(ui_peer, 0).expect("unmute succeeds");
    assert!(!muted_after_unmute);
    assert_eq!(live.dialog_mute_until(ui_peer), None);
    assert!(!live.dialog_by_peer(ui_peer).expect("dialog").muted);

    // 4. Set per-chat custom cloud ringtone
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    live.set_dialog_notify_sound(ui_peer, NotifySound::ringtone(55_501))
        .expect("set ringtone succeeds");
    assert_eq!(
        live.dialog_notify_sound(ui_peer),
        NotifySound::ringtone(55_501)
    );

    // 5. Disable sound (NotificationSound::None)
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    let sound_off = live
        .set_dialog_sound_enabled(ui_peer, false)
        .expect("disable sound succeeds");
    assert!(!sound_off);
    assert!(live.dialog_notify_sound(ui_peer).none);

    // 6. Re-enable sound restores the previous custom ringtone (55_501)
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    let sound_on = live
        .set_dialog_sound_enabled(ui_peer, true)
        .expect("enable sound succeeds");
    assert!(sound_on);
    assert_eq!(
        live.dialog_notify_sound(ui_peer),
        NotifySound::ringtone(55_501)
    );

    let reqs = rpc.recorded_decoded::<tl_fns::account::UpdateNotifySettings>();
    assert_eq!(reqs.len(), 6);
    fn settings_of(
        req: &tl_fns::account::UpdateNotifySettings,
    ) -> &tl_types::InputPeerNotifySettings {
        let tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(ref s) = req.settings;
        s
    }
    assert_eq!(settings_of(&reqs[0]).mute_until, Some(until_1h));
    assert_eq!(settings_of(&reqs[1]).mute_until, Some(i32::MAX));
    assert_eq!(settings_of(&reqs[2]).mute_until, Some(0));
    assert_eq!(
        settings_of(&reqs[3]).sound,
        Some(tl_enums::NotificationSound::Ringtone(
            tl_types::NotificationSoundRingtone { id: 55_501 }
        ))
    );
    assert_eq!(
        settings_of(&reqs[4]).sound,
        Some(tl_enums::NotificationSound::None)
    );
    assert_eq!(
        settings_of(&reqs[5]).sound,
        Some(tl_enums::NotificationSound::Ringtone(
            tl_types::NotificationSoundRingtone { id: 55_501 }
        ))
    );
}

#[test]
fn report_spam_and_block_with_delete_chat_and_unblock_restart_bot() {
    let (_rt, rpc, mut live) = make_live(false);
    let spammer_data = DataPeerId::from_user(DataUserId(401));
    let spammer_ui = UiPeerId::from_user(401);
    let bot_ui = UiPeerId::from_user(402);

    live.write_session(|s| {
        let mut spammer = User::new(DataUserId(401), false);
        spammer.first_name = "Spammer".to_owned();
        spammer.access_hash = 0x401;
        spammer.common.bar_settings = PeerBarSettings::REPORT_SPAM | PeerBarSettings::BLOCK_CONTACT;
        s.upsert_user(spammer, PeerUpdateFlags::NAME);

        let msg = Message::new_text(
            spammer_data,
            DataMsgId(7),
            Some(spammer_data),
            1_750_000_000,
            TextWithEntities::plain("spam msg"),
        );
        s.insert_message(msg);
        let h = s.ensure_history(spammer_data);
        h.in_chat_list = true;
        s.sync_dialog_lists(spammer_data);

        let mut bot = User::new(DataUserId(402), false);
        bot.first_name = "HelperBot".to_owned();
        bot.access_hash = 0x402;
        bot.bot_info = Some(BotInfo::default());
        bot.common.block_status = BlockStatus::Blocked;
        s.upsert_user(bot, PeerUpdateFlags::NAME);
    });
    live.seed_updates_pts(300, None);

    assert!(live.dialog_suggests_report_spam(spammer_ui));

    // Enqueue contacts.block + messages.reportSpam + paginated messages.deleteHistory (2 pages).
    rpc.enqueue_ok(tl_fns::contacts::Block::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(tl_fns::messages::ReportSpam::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 301,
                pts_count: 1,
                offset: 1,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::DeleteHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 302,
                pts_count: 1,
                offset: 0,
            },
        ),
    );

    live.report_spam_and_block(spammer_ui, true, true)
        .expect("report_spam_and_block ok");

    assert!(live.is_peer_blocked(spammer_ui));
    assert!(!live.dialog_suggests_report_spam(spammer_ui));
    assert_eq!(live.updates_common_pts(), 302);
    let in_list = live.read_session(|s| {
        s.history(spammer_data).is_some_and(|h| h.in_chat_list)
            || s.message(DataFullMsgId::new(spammer_data, DataMsgId(7)))
                .is_some()
    });
    assert!(
        !in_list,
        "deleted chat must leave chat list and clear messages"
    );

    assert_eq!(rpc.recorded_decoded::<tl_fns::contacts::Block>().len(), 1);
    assert_eq!(
        rpc.recorded_decoded::<tl_fns::messages::ReportSpam>().len(),
        1
    );
    let del_reqs = rpc.recorded_decoded::<tl_fns::messages::DeleteHistory>();
    assert_eq!(del_reqs.len(), 2);
    assert!(!del_reqs[0].just_clear);
    assert!(!del_reqs[0].revoke);

    // Unblock + restart bot sends contacts.unblock followed by messages.sendMessage("/start").
    rpc.enqueue_ok(tl_fns::contacts::Unblock::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.unblock_and_restart_bot(bot_ui)
        .expect("unblock_and_restart_bot ok");

    assert!(!live.is_peer_blocked(bot_ui));
    assert_eq!(rpc.recorded_decoded::<tl_fns::contacts::Unblock>().len(), 1);
    let sent = rpc.recorded_decoded::<tl_fns::messages::SendMessage>();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message, "/start");
}

// =============================================================================
// 4. Profile: group_call field & shared_media_thumb async loader
// =============================================================================

#[test]
fn profile_group_call_from_channel_full_and_update_group_call() {
    let (_rt, _rpc, live) = make_live(false);
    let ch_id = DataChannelId(7001);
    live.seed_updates_pts(100, Some((ch_id, 1)));
    let ch_data = DataPeerId::from_channel(ch_id);
    let ch_ui = UiPeerId::from_channel(7001);
    let dm_ui = UiPeerId::from_user(7002);

    live.write_session(|s| {
        let mut u = User::new(DataUserId(7002), false);
        u.first_name = "Regular User".to_owned();
        s.upsert_user(u, PeerUpdateFlags::NAME);

        let tl_ch = tl_enums::Chat::Channel(tl_types::Channel {
            creator: false,
            left: false,
            broadcast: false,
            verified: false,
            megagroup: true,
            restricted: false,
            signatures: false,
            min: false,
            scam: false,
            has_link: false,
            has_geo: false,
            slowmode_enabled: false,
            call_active: true,
            call_not_empty: true,
            fake: false,
            gigagroup: false,
            noforwards: false,
            join_to_send: false,
            join_request: false,
            forum: false,
            stories_hidden: false,
            stories_hidden_min: false,
            stories_unavailable: false,
            signature_profiles: false,
            autotranslation: false,
            broadcast_messages_allowed: false,
            monoforum: false,
            forum_tabs: false,
            in_news: false,
            forum_sidebar: false,
            id: 7001,
            access_hash: Some(0x7001),
            title: "Voice Lounge".to_owned(),
            username: None,
            photo: tl_enums::ChatPhoto::Empty,
            date: 1_750_000_000,
            restriction_reason: None,
            admin_rights: None,
            banned_rights: None,
            default_banned_rights: None,
            participants_count: Some(42),
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            emoji_status: None,
            level: None,
            subscription_until_date: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_monoforum_id: None,
            linked_community_id: None,
            pinned_msg_id: None,
        });
        apply_tl_users_and_chats(s, &[], &[tl_ch]);

        let tl_full = tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
            can_view_participants: true,
            can_set_username: false,
            can_set_stickers: false,
            hidden_prehistory: false,
            can_set_location: false,
            has_scheduled: false,
            can_view_stats: false,
            blocked: false,
            can_delete_channel: false,
            antispam: false,
            participants_hidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            view_forum_as_messages: false,
            restricted_sponsored: false,
            can_view_revenue: false,
            paid_media_allowed: false,
            can_view_stars_revenue: false,
            paid_reactions_available: false,
            stargifts_available: false,
            paid_messages_available: false,
            has_welcome_messages: false,
            id: 7001,
            about: "Active video chat".to_owned(),
            participants_count: Some(42),
            admins_count: Some(2),
            kicked_count: None,
            banned_count: None,
            online_count: Some(5),
            read_inbox_max_id: 0,
            read_outbox_max_id: 0,
            unread_count: 0,
            chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: None,
                    silent: None,
                    mute_until: None,
                    ios_sound: None,
                    android_sound: None,
                    other_sound: None,
                    stories_muted: None,
                    stories_hide_sender: None,
                    stories_ios_sound: None,
                    stories_android_sound: None,
                    stories_other_sound: None,
                },
            ),
            exported_invite: None,
            bot_info: Vec::new(),
            migrated_from_chat_id: None,
            migrated_from_max_id: None,
            pinned_msg_id: None,
            stickerset: None,
            available_min_id: None,
            folder_id: None,
            linked_chat_id: None,
            location: None,
            slowmode_seconds: None,
            slowmode_next_send_date: None,
            stats_dc: None,
            pts: 1,
            call: Some(tl_enums::InputGroupCall::InputGroupCall(
                tl_types::InputGroupCall {
                    id: 9001,
                    access_hash: 0xABCD,
                },
            )),
            ttl_period: None,
            pending_suggestions: None,
            groupcall_default_join_as: None,
            theme_emoticon: None,
            requests_pending: None,
            recent_requesters: None,
            default_send_as: None,
            available_reactions: None,
            reactions_limit: None,
            stories: None,
            wallpaper: None,
            boosts_applied: None,
            boosts_unrestrict: None,
            emojiset: None,
            bot_verification: None,
            stargifts_count: None,
            send_paid_messages_stars: None,
            main_tab: None,
            guard_bot_id: None,
            share_theme: None,
        });
        apply_tl_chat_full(s, &tl_full);
        let h = s.ensure_history(ch_data);
        h.in_chat_list = true;
        s.sync_dialog_lists(ch_data);
    });

    // Regular user never has a group call button.
    let dm_profile = live.peer_profile(dm_ui).expect("user profile");
    assert_eq!(dm_profile.group_call, ProfileGroupCallState::default());
    assert!(!dm_profile.group_call.show_button());

    // Channel with active call (non-admin member): active=true, call_id populated, show_button=true.
    let ch_profile = live.peer_profile(ch_ui).expect("channel profile");
    assert!(ch_profile.group_call.active);
    assert!(ch_profile.group_call.has_participants);
    assert_eq!(ch_profile.group_call.call_id, Some((9001, 0xABCD)));
    assert!(!ch_profile.group_call.can_manage);
    assert!(ch_profile.group_call.show_button());
    assert!(
        live.dialog_by_peer(ch_ui)
            .expect("channel dialog")
            .active_call
    );

    // Discarded `updateGroupCall` clears active call on the channel.
    let discard_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
            live_story: false,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 7001,
            })),
            call: tl_enums::GroupCall::Discarded(tl_types::GroupCallDiscarded {
                id: 9001,
                access_hash: 0xABCD,
                duration: 120,
            }),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_200,
        seq: 0,
    });
    let _ = live.apply_updates_to_session(&discard_updates);

    let after_discard = live.peer_profile(ch_ui).expect("channel profile");
    assert!(!after_discard.group_call.active);
    assert!(!after_discard.group_call.has_participants);
    assert_eq!(after_discard.group_call.call_id, None);
    assert!(
        !after_discard.group_call.show_button(),
        "non-admin without active call must hide Video chat button"
    );
    assert!(
        !live
            .dialog_by_peer(ch_ui)
            .expect("channel dialog")
            .active_call
    );

    // Granting MANAGE_CALL admin right enables `can_manage` -> `show_button() == true` even with no active call.
    live.write_session(|s| {
        if let Some(ch) = s.channel_mut(ch_id) {
            ch.admin_rights |= ChatAdminRights::MANAGE_CALL;
        }
    });
    let as_admin = live.peer_profile(ch_ui).expect("channel profile");
    assert!(!as_admin.group_call.active);
    assert!(as_admin.group_call.can_manage);
    assert!(as_admin.group_call.show_button());

    // Live `updateGroupCall` with an active call sets call_id, active, and has_participants.
    let start_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
            live_story: false,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 7001,
            })),
            call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                join_muted: false,
                can_change_join_muted: true,
                join_date_asc: false,
                schedule_start_subscribed: false,
                can_start_video: true,
                record_video_active: false,
                rtmp_stream: false,
                listeners_hidden: false,
                conference: false,
                creator: true,
                messages_enabled: false,
                can_change_messages_enabled: false,
                min: false,
                id: 9002,
                access_hash: 0xBCDE,
                participants_count: 3,
                title: Some("Standup".to_owned()),
                stream_dc_id: None,
                record_start_date: None,
                schedule_date: None,
                unmuted_video_count: None,
                unmuted_video_limit: 0,
                version: 1,
                invite_link: None,
                send_paid_messages_stars: None,
                default_send_as: None,
            }),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_300,
        seq: 0,
    });
    let _ = live.apply_updates_to_session(&start_updates);
    let restarted = live.peer_profile(ch_ui).expect("channel profile");
    assert!(restarted.group_call.active);
    assert!(restarted.group_call.has_participants);
    assert_eq!(restarted.group_call.call_id, Some((9002, 0xBCDE)));
    assert!(
        live.dialog_by_peer(ch_ui)
            .expect("channel dialog")
            .active_call
    );
}

#[test]
fn profile_shared_media_thumb_loads_photo_and_video_thumbs_async_and_caches() {
    let (rt, rpc, live) = make_live(false);
    let data_peer = DataPeerId::from_user(DataUserId(8001));
    let ui_peer = UiPeerId::from_user(8001);

    let photo_stripped = vec![0x01, 0x10, 0x10, 0xAA, 0xBB];
    let video_stripped = vec![0x01, 0x12, 0x12, 0xCC, 0xDD];

    live.write_session(|s| {
        let mut u = User::new(DataUserId(8001), false);
        u.first_name = "Media Friend".to_owned();
        u.access_hash = 0x8001;
        s.upsert_user(u, PeerUpdateFlags::NAME);

        let mut photo_msg = Message::new_text(
            data_peer,
            DataMsgId(101),
            Some(data_peer),
            1_750_000_010,
            TextWithEntities::plain("sunset"),
        );
        photo_msg.content = MessageContent::Regular {
            text: TextWithEntities::plain("sunset"),
            media: MessageMedia::Photo(Box::new(PhotoData {
                id: PhotoId(9101),
                access_hash: 0x9101,
                file_reference: vec![9, 1, 0, 1],
                dc_id: 2,
                date: 1_750_000_010,
                has_stickers: false,
                spoiler: false,
                sizes: vec![
                    PhotoSize {
                        kind: "s".to_owned(),
                        width: 100,
                        height: 75,
                        size: 1_200,
                    },
                    PhotoSize {
                        kind: "m".to_owned(),
                        width: 320,
                        height: 240,
                        size: 8_400,
                    },
                    PhotoSize {
                        kind: "y".to_owned(),
                        width: 1280,
                        height: 960,
                        size: 95_000,
                    },
                ],
                stripped_thumb: Some(photo_stripped.clone()),
                ..Default::default()
            })),
        };
        s.insert_message(photo_msg);

        let mut video_msg = Message::new_text(
            data_peer,
            DataMsgId(102),
            Some(data_peer),
            1_750_000_020,
            TextWithEntities::plain("clip"),
        );
        video_msg.content = MessageContent::Regular {
            text: TextWithEntities::plain("clip"),
            media: MessageMedia::Document(Box::new(DocumentData {
                id: DocumentId(9102),
                access_hash: 0x9102,
                file_reference: vec![9, 1, 0, 2],
                dc_id: 2,
                date: 1_750_000_020,
                mime_type: "video/mp4".to_owned(),
                size: 5_000_000,
                kind: DocumentKind::Video,
                file_name: "clip.mp4".to_owned(),
                width: Some(1280),
                height: Some(720),
                duration: Some(12.0),
                thumbs: vec![PhotoSize {
                    kind: "m".to_owned(),
                    width: 320,
                    height: 180,
                    size: 6_200,
                }],
                stripped_thumb: Some(video_stripped.clone()),
                ..Default::default()
            })),
        };
        s.insert_message(video_msg);
    });

    // Missing message returns Unavailable without sending any RPC.
    assert_eq!(
        live.shared_media_thumb(ui_peer, 999),
        SharedMediaThumb::Unavailable
    );

    // 1. Photo thumb: gate upload.getFile to prove non-blocking UI return & deduplication.
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    rpc.set_upload_gate(Some(Arc::clone(&gate)), Some(Arc::clone(&started)));
    rpc.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileJpeg,
            mtime: 1_750_000_010,
            bytes: b"PHOTO_THUMB_M_BYTES".to_vec(),
        }),
    );

    let t0 = Instant::now();
    let first = live.shared_media_thumb(ui_peer, 101);
    assert!(
        t0.elapsed() < Duration::from_millis(200),
        "shared_media_thumb must not block the UI thread"
    );
    assert_eq!(
        first,
        SharedMediaThumb::Loading {
            stripped_thumb: Some(photo_stripped.clone())
        }
    );

    // Polling again while in flight deduplicates (no second RPC).
    let second = live.shared_media_thumb(ui_peer, 101);
    assert_eq!(
        second,
        SharedMediaThumb::Loading {
            stripped_thumb: Some(photo_stripped)
        }
    );

    // Wait until the background task actually invokes `upload.getFile`, then release the gate.
    rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), started.notified()).await })
        .expect("background upload.getFile task must start");
    gate.notify_one();
    let deadline = Instant::now() + Duration::from_secs(3);
    let ready_photo = loop {
        if let SharedMediaThumb::Ready { bytes } = live.shared_media_thumb(ui_peer, 101) {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for photo thumb"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(ready_photo, b"PHOTO_THUMB_M_BYTES");

    // 2. Video thumb: downloads document thumbnail size "m" and caches it.
    rpc.set_upload_gate(None, None);
    rpc.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileJpeg,
            mtime: 1_750_000_020,
            bytes: b"VIDEO_THUMB_M_BYTES".to_vec(),
        }),
    );
    let _ = live.shared_media_thumb(ui_peer, 102);
    let deadline = Instant::now() + Duration::from_secs(3);
    let ready_video = loop {
        if let SharedMediaThumb::Ready { bytes } = live.shared_media_thumb(ui_peer, 102) {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for video thumb"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(ready_video, b"VIDEO_THUMB_M_BYTES");

    // Verify exact TL locations requested: photo chose grid thumb "m" (not full "y"), video chose thumb "m".
    let get_files = rpc.recorded_decoded::<tl_fns::upload::GetFile>();
    assert_eq!(get_files.len(), 2, "each thumbnail must be fetched once");
    match &get_files[0].location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(loc) => {
            assert_eq!(loc.id, 9101);
            assert_eq!(loc.thumb_size, "m");
        }
        other => panic!("expected InputPhotoFileLocation, got {other:?}"),
    }
    match &get_files[1].location {
        tl_enums::InputFileLocation::InputDocumentFileLocation(loc) => {
            assert_eq!(loc.id, 9102);
            assert_eq!(loc.thumb_size, "m");
        }
        other => panic!("expected InputDocumentFileLocation, got {other:?}"),
    }
}
