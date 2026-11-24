//! Live Telegram data export RPC adapter over `tg-export`'s [`ExportRpc`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/export/export_manager.cpp`
//! - `Telegram/SourceFiles/export/export_api_wrap.cpp`
//! - `Telegram/SourceFiles/export/view/export_view_panel_controller.cpp`

use std::sync::Arc;

use tg_api::RpcClient;
use tg_api::convert::input_peer_for;
use tg_data::Session;
use tg_export::stand_in::InputPeer;
use tg_export::{ExportRpc, RpcCallError};
use tg_tl::{Identifiable, RemoteCall, enums as tl_enums};

use crate::bridge::LiveBridgeState;
use crate::convert::{peer_display_title, to_data_peer};
use crate::media::block_on_runtime;
use crate::session::LiveSession;
use tg_app_core::stand_in::types::PeerId;

/// Adapter bridging `tg-api`'s [`RpcClient`] into `tg-export`'s [`ExportRpc`] trait.
///
/// Routes requests with `dc_id > 0` through [`RpcClient::invoke_in_dc`] (for cross-DC
/// file downloads and `FILE_MIGRATE_X` redirects) and main-DC requests through
/// [`RpcClient::invoke`], preserving exact Telegram RPC error tokens such as
/// `TAKEOUT_INIT_DELAY_<seconds>`, `TAKEOUT_INVALID`, `FILE_REFERENCE_*`, and
/// `TAKEOUT_FILE_EMPTY`.
#[derive(Clone)]
pub struct LiveExportRpc<C: RpcClient + Clone + 'static> {
    rpc: C,
    bridge: Option<Arc<LiveBridgeState>>,
}

impl<C: RpcClient + Clone + 'static> LiveExportRpc<C> {
    /// Creates a new [`LiveExportRpc`] wrapping `rpc`.
    #[must_use]
    pub fn new(rpc: C) -> Self {
        Self { rpc, bridge: None }
    }

    /// Creates a new [`LiveExportRpc`] that also wakes the egui UI via `bridge`
    /// whenever export RPC calls complete.
    #[must_use]
    pub fn with_bridge(rpc: C, bridge: Arc<LiveBridgeState>) -> Self {
        Self {
            rpc,
            bridge: Some(bridge),
        }
    }

    /// Returns a reference to the underlying RPC client.
    #[must_use]
    pub const fn rpc(&self) -> &C {
        &self.rpc
    }
}

impl<C: RpcClient + Clone + 'static> ExportRpc for LiveExportRpc<C> {
    async fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, RpcCallError> {
        let res = if dc_id > 0 {
            self.rpc.invoke_in_dc(dc_id, request).await
        } else {
            self.rpc.invoke(request).await
        };
        if let Some(ref bridge) = self.bridge {
            bridge.notify_ui();
        }
        res.map_err(|err| {
            let inv: tg_api::rpc::InvocationError = err.into();
            match inv {
                tg_api::rpc::InvocationError::Rpc(rpc_err) => {
                    let msg = format!("RPC {} {}", rpc_err.code, rpc_err.name);
                    RpcCallError::new(rpc_err.code, rpc_err.name, msg)
                }
                other => RpcCallError::new(-1, "TRANSPORT_ERROR", other.to_string()),
            }
        })
    }
}

/// Configuration options for constructing a live `ExportController` run.
#[derive(Debug, Clone)]
pub struct LiveExportOptions {
    /// Local UTC offset in seconds used for exported media filename timestamps.
    pub utc_offset_secs: i32,
    /// When `true`, writes directly into `settings.path` instead of appending
    /// a `DataExport_YYYY-MM-DD` / `ChatExport_YYYY-MM-DD` subdirectory.
    pub skip_path_normalization: bool,
    /// Internal links domain written into exported HTML/JSON (default `"https://t.me/"`).
    pub internal_links_domain: String,
}

impl Default for LiveExportOptions {
    fn default() -> Self {
        Self {
            utc_offset_secs: 0,
            skip_path_normalization: false,
            internal_links_domain: "https://t.me/".to_owned(),
        }
    }
}

fn read_session_for_export<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

impl LiveSession {
    /// Creates a [`LiveExportRpc`] backed by this session's RPC client and UI bridge.
    #[must_use]
    pub fn live_export_rpc(
        &self,
    ) -> LiveExportRpc<tg_app_core::core_types::erased_rpc::ErasedRpcClient> {
        LiveExportRpc::with_bridge(
            self.runtime().rpc().clone(),
            Arc::clone(self.bridge_state()),
        )
    }

    /// Resolves a UI [`PeerId`] into its TL [`InputPeer`] (with real `access_hash`)
    /// and display title for single-chat export from the Info pane.
    #[must_use]
    pub fn resolve_export_peer(&self, peer_id: PeerId) -> Option<(InputPeer, String)> {
        let data_peer = to_data_peer(peer_id);
        read_session_for_export(self, |session| {
            let input = input_peer_for(session, data_peer);
            if matches!(input, tl_enums::InputPeer::Empty) {
                return None;
            }
            let title = {
                let raw = peer_display_title(session, data_peer);
                if raw.trim().is_empty() {
                    format!("Chat {}", peer_id.0)
                } else {
                    raw
                }
            };
            Some((input, title))
        })
    }
}
