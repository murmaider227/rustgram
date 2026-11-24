//! Batched `messages.reportReadMetrics` live sender + re-exports of pure read-metrics tracker.
//!
//! Port origin: `Telegram/SourceFiles/history/view/history_view_read_metrics_tracker.{h,cpp}`
//! and `Telegram/SourceFiles/api/api_read_metrics.{h,cpp}`.

use tg_tl::functions as tl_fns;

use crate::session::LiveSession;
pub use tg_app_core::core_types::read_metrics::*;
use tg_app_core::stand_in::types::PeerId;

/// Sends one peer's batch of read metrics via `messages.reportReadMetrics`.
pub fn send_read_metrics_live(
    live: &LiveSession,
    peer_id: PeerId,
    metrics: &[FinalizedReadMetric],
) -> Result<bool, String> {
    if metrics.is_empty() {
        return Ok(true);
    }
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for reportReadMetrics.".to_owned());
    };
    let req = tl_fns::messages::ReportReadMetrics {
        peer: input_peer,
        metrics: metrics
            .iter()
            .copied()
            .map(FinalizedReadMetric::to_tl)
            .collect(),
    };
    live.invoke_rpc(req)
}
