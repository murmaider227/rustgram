//! Request failures in the chat (TGB-2977, TGB-2978): a failed send shows what tdesktop's
//! `sendMessageFail` shows, and a failed transcription stays in the bubble.

use tg_app_core::history::backend::{HistoryLiveUpdate, TranscribeToggleOutcome};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};
use tg_app_core::user_error::{SendTarget, UserFacingError};
use tg_text::TextWithEntities;

use super::HistoryWidget;
use super::inner::InnerEvent;
use super::mock::MockHistoryStore;
use crate::history::backend::{
    DraftMessage, HistoryBackend, HistoryMessage, HistoryPeerInfo, HistorySlice,
    PinnedMessageSummary, ReactionCounter, SendMessageRequest, TypingAction,
};

fn rpc(code: i32, name: &str) -> String {
    tg_api::ApiError::Invocation(tg_api::RpcError::new(code, name).into()).to_string()
}

/// [`MockHistoryStore`] plus one queued live update and one transcription outcome.
struct FailingBackend {
    inner: MockHistoryStore,
    updates: Vec<HistoryLiveUpdate>,
    transcribe: Option<TranscribeToggleOutcome>,
}

impl HistoryBackend for FailingBackend {
    fn self_peer(&self) -> (PeerId, String) {
        self.inner.self_peer()
    }
    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        self.inner.peer_info(peer_id)
    }
    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        self.inner
            .load_slice_around(peer_id, around_id, limit_before, limit_after)
    }
    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        self.inner.load_slice_before(peer_id, before_id, limit)
    }
    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        self.inner.load_slice_after(peer_id, after_id, limit)
    }
    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        self.inner.send_message(peer_id, request)
    }
    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        self.inner.edit_message(peer_id, msg_id, text)
    }
    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        revoke_for_all: bool,
    ) -> Result<(), String> {
        self.inner.delete_messages(peer_id, msg_ids, revoke_for_all)
    }
    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.inner.forward_messages(from_peer, msg_ids, to_peer)
    }
    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId) {
        self.inner.mark_history_read(peer_id, max_id);
    }
    fn send_typing(&mut self, peer_id: PeerId, action: TypingAction) {
        self.inner.send_typing(peer_id, action);
    }
    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        self.inner.toggle_reaction(peer_id, msg_id, emoji)
    }
    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        self.inner.pin_message(peer_id, msg_id, unpin)
    }
    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        self.inner.pinned_messages(peer_id)
    }
    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        self.inner.vote_poll(peer_id, msg_id, option_indices)
    }
    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        self.inner.save_draft(peer_id, draft);
    }
    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        self.inner.load_draft(peer_id)
    }
    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        self.inner.dismiss_contact_status(peer_id);
    }
    fn add_contact(&mut self, peer_id: PeerId) {
        self.inner.add_contact(peer_id);
    }
    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        self.inner.toggle_block_peer(peer_id);
    }
    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.inner.toggle_mute(peer_id)
    }
    fn drain_history_updates(&mut self, _peer_id: PeerId) -> Vec<HistoryLiveUpdate> {
        std::mem::take(&mut self.updates)
    }
    fn toggle_transcribe_audio(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _duration_secs: u32,
        _is_round_video: bool,
    ) -> TranscribeToggleOutcome {
        self.transcribe
            .take()
            .unwrap_or(TranscribeToggleOutcome::Pending)
    }
}

fn backend() -> (FailingBackend, PeerId, HistoryPeerInfo) {
    let inner = MockHistoryStore::new();
    // The mock's first one-to-one chat (`MockHistoryStore::new`).
    let peer = PeerId::from_user(1001);
    let info = inner.peer_info(peer).expect("mock peer info");
    (
        FailingBackend {
            inner,
            updates: Vec::new(),
            transcribe: None,
        },
        peer,
        info,
    )
}

fn failed_send(error: &str) -> HistoryLiveUpdate {
    HistoryLiveUpdate::LocalMessageFailed {
        local_id: MsgId::local((1 << 29) + 1).expect("local id"),
        error: Some(rpc(400, error)),
    }
}

#[test]
fn peer_flood_on_send_opens_the_spambot_box_not_a_toast() {
    let (mut backend, peer, info) = backend();
    backend.updates.push(failed_send("PEER_FLOOD"));
    let mut widget = HistoryWidget::new();
    let ctx = egui::Context::default();
    widget.sync_live_history_updates(peer, &info, &mut backend, &ctx);
    assert_eq!(widget.active_toast(), None, "the old code toasted the type");
    let errors = widget.take_user_errors();
    assert!(
        matches!(errors.as_slice(), [UserFacingError::InformRich { .. }]),
        "{errors:?}"
    );
    assert!(widget.take_user_errors().is_empty(), "drained once");
}

#[test]
fn schedule_limit_on_send_is_a_chat_toast_and_premium_required_opens_premium() {
    let (mut backend, peer, info) = backend();
    backend.updates.push(failed_send("SCHEDULE_TOO_MUCH"));
    backend
        .updates
        .push(failed_send("PREMIUM_ACCOUNT_REQUIRED"));
    let mut widget = HistoryWidget::new();
    let ctx = egui::Context::default();
    widget.sync_live_history_updates(peer, &info, &mut backend, &ctx);
    assert_eq!(
        widget.active_toast(),
        Some(tg_lang::tr::lng_error_schedule_limit().as_str())
    );
    assert_eq!(
        widget.take_user_errors(),
        vec![UserFacingError::PremiumScreen {
            referral: "premium_stickers".to_owned()
        }]
    );
}

#[test]
fn send_target_follows_the_chat_kind() {
    assert_eq!(
        super::user_errors::send_target(PeerKind::User),
        SendTarget::User
    );
    assert_eq!(
        super::user_errors::send_target(PeerKind::Channel),
        SendTarget::Channel
    );
}

#[test]
fn failed_transcription_shows_no_toast() {
    let (mut backend, peer, info) = backend();
    backend.transcribe = Some(TranscribeToggleOutcome::Failed(rpc(
        400,
        "MSG_VOICE_TOO_LONG",
    )));
    let mut widget = HistoryWidget::new();
    let ctx = egui::Context::default();
    let _ = widget.handle_inner_events(
        &ctx,
        vec![InnerEvent::ToggleTranscribe {
            msg_id: MsgId(7),
            duration_secs: 30,
            is_round_video: false,
        }],
        peer,
        &info,
        None,
        &mut backend,
        0.0,
        0,
    );
    assert_eq!(widget.active_toast(), None, "the bubble shows the failure");
}

#[test]
fn trial_length_check_toasts_tdesktop_text() {
    let (mut backend, peer, info) = backend();
    backend.transcribe = Some(TranscribeToggleOutcome::TooLong { max_secs: 120 });
    let mut widget = HistoryWidget::new();
    let ctx = egui::Context::default();
    let _ = widget.handle_inner_events(
        &ctx,
        vec![InnerEvent::ToggleTranscribe {
            msg_id: MsgId(7),
            duration_secs: 300,
            is_round_video: false,
        }],
        peer,
        &info,
        None,
        &mut backend,
        0.0,
        0,
    );
    assert_eq!(
        widget.active_toast(),
        Some(tg_lang::tr::lng_audio_transcribe_long().as_str())
    );
}
