//! Sending a recorded voice message (TGB-3015): tdesktop's `VoiceRecordBar` send ->
//! `ApiWrap::sendVoiceMessage(bytes, waveform, duration, action)`. The recorded OGG/Opus file
//! travels with the request; a failed send is shown like any other failed send.

use tg_app_core::history::backend::{HistoryBackend, HistoryMedia, SendMessageExtended};
use tg_app_core::stand_in::types::{PeerId, PeerKind};

use super::HistoryWidget;
use super::compose::ComposeMode;
use super::compose_extras::RecordedVoice;

impl HistoryWidget {
    /// Sends `voice` to `peer_id` with the compose bar's reply target, silent flag and send-as
    /// peer (`prepareSendAction`). Returns `true` when the message went out.
    pub(crate) fn send_recorded_voice(
        &mut self,
        peer_id: PeerId,
        kind: PeerKind,
        voice: RecordedVoice,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> bool {
        let (reply_to, reply_quote) = match &self.compose.mode {
            ComposeMode::Reply { msg_id, quote, .. } => (Some(*msg_id), quote.clone()),
            _ => (None, None),
        };
        let request = SendMessageExtended {
            reply_to,
            reply_quote,
            silent: self.compose.silent_mode,
            send_as: self.compose.send_as.selected,
            media: Some(HistoryMedia::Voice(voice.attachment)),
            voice_ogg: Some(voice.ogg),
            ..SendMessageExtended::default()
        };
        match backend.send_message_extended(peer_id, request) {
            Ok(sent) => {
                if reply_to.is_some() {
                    self.compose.cancel_mode();
                }
                self.inner.append_message(sent, kind);
                true
            }
            Err(err) => {
                self.show_send_failure(&err, kind, now_secs);
                false
            }
        }
    }
}
