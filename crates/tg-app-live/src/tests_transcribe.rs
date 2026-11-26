//! Unit and integration tests for voice/round-video transcription (`live/transcribe.rs`)
//! using an in-process `FakeRpcClient` (no network calls, no real accounts).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::transcribe::TranscribeLimits;
use tg_app_core::history::backend::{HistoryBackend, TranscribeToggleOutcome};
use tg_app_core::stand_in::types::{MsgId, PeerId};

type QueuedResponses = Arc<Mutex<VecDeque<Result<Vec<u8>, InvocationError>>>>;
type CapturedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeTranscribeRpc {
    responses: QueuedResponses,
    captured: CapturedCalls,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeTranscribeRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            captured: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn push_ok<T: Serializable>(&self, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back(Ok(resp.to_bytes()));
    }

    fn push_rpc_error(&self, code: i32, name: &str) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back(Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })));
    }

    fn push_update(&self, update: tl_enums::Update) {
        let _ = self
            .updates_tx
            .send(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update,
                date: 1_700_000_000,
            }));
    }

    fn captured_requests(&self) -> Vec<(u32, Vec<u8>)> {
        self.captured.lock().expect("lock captured").clone()
    }
}

impl RpcClient for FakeTranscribeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = if bytes.len() >= 4 {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            0
        };
        self.captured
            .lock()
            .expect("lock captured")
            .push((ctor, bytes));
        let next = self
            .responses
            .lock()
            .expect("lock responses")
            .pop_front()
            .ok_or(InvocationError::Dropped)??;
        R::Return::from_bytes(&next).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn build_live_session_with_peer(
    rpc: FakeTranscribeRpc,
    peer_uid: i64,
    self_premium: bool,
) -> (LiveSession, PeerId) {
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(DataUserId(100), crate::ErasedRpcClient::new(rpc), cfg)
            .expect("start SessionRuntime"),
    );

    // Seed self user and target peer into `tg_data::Session`.
    let self_tl = make_user(100, "Self", true, self_premium);
    let peer_tl = make_user(peer_uid, "Peer", false, false);
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(self_tl), tl_enums::User::User(peer_tl)],
            &[],
        );
    });

    let live = LiveSession::new(runtime, 1);
    let peer_id =
        crate::convert::from_data_peer(DataPeerId::from_user(DataUserId(peer_uid as u64)));
    (live, peer_id)
}

fn make_user(id: i64, name: &str, is_self: bool, premium: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 3),
        first_name: Some(name.to_owned()),
        last_name: None,
        username: Some(format!("user_{id}")),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

#[test]
fn transcribe_pending_to_final_update_and_rate_flow() {
    let rpc = FakeTranscribeRpc::new();
    let (mut live, peer_id) = build_live_session_with_peer(rpc.clone(), 4242, false);
    let msg_id = MsgId(501);

    // 1. Server returns pending partial transcription + decremented trial quota.
    rpc.push_ok(&tl_enums::messages::TranscribedAudio::TranscribedAudio(
        tl_types::messages::TranscribedAudio {
            pending: true,
            transcription_id: 99_887_766,
            text: "Hello...".to_owned(),
            trial_remains_num: Some(1),
            trial_remains_until_date: Some(2_000_000_000),
        },
    ));

    let outcome = live.toggle_transcribe_audio(peer_id, msg_id, 14, false);
    assert_eq!(outcome, TranscribeToggleOutcome::Pending);

    let entry = live
        .transcribe_audio_state(peer_id, msg_id)
        .expect("transcribe entry exists");
    assert!(entry.shown);
    assert!(entry.pending);
    assert_eq!(entry.text, "Hello...");
    assert!(!entry.can_rate, "cannot rate while pending");

    // Verify trial quota updated and toast was generated.
    let limits = live.transcribe_limits_snapshot();
    assert_eq!(limits.trials_count, 1);
    assert_eq!(limits.trials_refresh_at, 2_000_000_000);
    let toast = live
        .take_transcribe_trial_toast()
        .expect("trial quota toast");
    assert!(toast.contains("1 free transcription remaining"));

    // 2. Push `Update::TranscribedAudio` with final text (`pending: false`) over the live stream.
    rpc.push_update(tl_enums::Update::TranscribedAudio(
        tl_types::UpdateTranscribedAudio {
            pending: false,
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 4242 }),
            msg_id: 501,
            transcription_id: 99_887_766,
            text: "Hello from voice message!".to_owned(),
        },
    ));

    // Wait briefly for the background broadcast listener task to apply the update.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(st) = live.transcribe_audio_state(peer_id, msg_id)
            && !st.pending
        {
            assert_eq!(st.text, "Hello from voice message!");
            assert!(st.can_rate);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for UpdateTranscribedAudio"
        );
        std::thread::sleep(Duration::from_millis(15));
    }

    // 3. Toggling hides then re-shows without issuing a second TranscribeAudio RPC.
    let reqs_before = rpc.captured_requests().len();
    assert_eq!(
        live.toggle_transcribe_audio(peer_id, msg_id, 14, false),
        TranscribeToggleOutcome::Toggled { shown: false }
    );
    assert_eq!(
        live.toggle_transcribe_audio(peer_id, msg_id, 14, false),
        TranscribeToggleOutcome::Toggled { shown: true }
    );
    assert_eq!(rpc.captured_requests().len(), reqs_before);

    // 4. Rate transcription (`messages.rateTranscribedAudio`, `good = true`).
    rpc.push_ok(&true);
    live.rate_transcribed_audio(peer_id, msg_id, true)
        .expect("rate transcription");
    assert!(live.is_transcription_rated(peer_id, msg_id));
    let final_ui = live
        .transcribe_audio_state(peer_id, msg_id)
        .expect("entry after rating");
    assert!(final_ui.rated);
    assert!(!final_ui.can_rate);

    // Verify exact TL constructors invoked (`TranscribeAudio`, `RateTranscribedAudio`).
    let captured = rpc.captured_requests();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].0,
        tl_fns::messages::TranscribeAudio::CONSTRUCTOR_ID
    );
    assert_eq!(
        captured[1].0,
        tl_fns::messages::RateTranscribedAudio::CONSTRUCTOR_ID
    );
}

#[test]
fn transcribe_premium_trial_limits_cooldown_and_too_long() {
    let rpc = FakeTranscribeRpc::new();
    let (mut live, peer_id) = build_live_session_with_peer(rpc.clone(), 5555, false);

    // Exhausted weekly trials with cooldown far in the future -> locked.
    live.configure_transcribe_limits(TranscribeLimits {
        is_premium: false,
        trials_support: true,
        trials_count: 0,
        trials_refresh_at: i32::MAX - 10,
        trials_max_length_ms: 120_000,
        group_free_transcribe_level: 0,
    });
    let view = live.transcribe_limits_view(Some(peer_id));
    assert!(view.locked);
    assert_eq!(
        live.toggle_transcribe_audio(peer_id, MsgId(601), 15, true),
        TranscribeToggleOutcome::PremiumRequired {
            refresh_at: i32::MAX - 10
        }
    );
    assert!(
        rpc.captured_requests().is_empty(),
        "locked request must not invoke RPC"
    );

    // Boosted group grants free transcription even when personal trials are 0.
    live.set_peer_free_transcribe(peer_id, true);
    assert!(!live.transcribe_limits_view(Some(peer_id)).locked);
    rpc.push_ok(&tl_enums::messages::TranscribedAudio::TranscribedAudio(
        tl_types::messages::TranscribedAudio {
            pending: false,
            transcription_id: 77_001,
            text: "Round video transcript".to_owned(),
            trial_remains_num: None,
            trial_remains_until_date: None,
        },
    ));
    let round_out = live.toggle_transcribe_audio(peer_id, MsgId(602), 25, true);
    assert!(matches!(round_out, TranscribeToggleOutcome::Ready { .. }));
    let round_entry = live
        .transcribe_audio_state(peer_id, MsgId(602))
        .expect("round video entry");
    assert!(round_entry.round_view);
    assert_eq!(round_entry.text, "Round video transcript");

    // Remove free-peer override, grant 1 trial, test duration limit (> 120s).
    live.set_peer_free_transcribe(peer_id, false);
    live.configure_transcribe_limits(TranscribeLimits {
        is_premium: false,
        trials_support: true,
        trials_count: 1,
        trials_refresh_at: 0,
        trials_max_length_ms: 120_000,
        group_free_transcribe_level: 0,
    });
    assert_eq!(
        live.toggle_transcribe_audio(peer_id, MsgId(603), 180, false),
        TranscribeToggleOutcome::TooLong { max_secs: 120 }
    );

    // Server-side `MSG_VOICE_TOO_LONG` error marks `entry.too_long = true`; the bubble shows
    // it, so the outcome is a plain failure, not the local length-check toast (TGB-2978).
    rpc.push_rpc_error(400, "MSG_VOICE_TOO_LONG");
    let srv_too_long = live.toggle_transcribe_audio(peer_id, MsgId(604), 60, false);
    assert!(
        matches!(&srv_too_long, TranscribeToggleOutcome::Failed(err) if err.contains("MSG_VOICE_TOO_LONG")),
        "{srv_too_long:?}"
    );
    let err_entry = live
        .transcribe_audio_state(peer_id, MsgId(604))
        .expect("failed entry stored");
    assert!(err_entry.failed);
    assert!(err_entry.too_long);
}
