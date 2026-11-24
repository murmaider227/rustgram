//! Shell-host `AppExportHost` and `ExportManager` wiring for `LiveSession`.

use std::sync::Arc;
use std::time::Duration;

use tg_api::RpcClient;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::PeerId;
use tg_app_live::bridge::LiveBridgeState;
use tg_app_live::export::{LiveExportOptions, LiveExportRpc};
use tg_app_live::session::LiveSession;
use tg_export::{Environment, ExportController, TlExportSource};

use crate::export::{
    AppExportHost, ExportManager, ExportTarget, LiveExportStarter, PacedExportSource,
    PlatformFolderOpener, QtFileSettingsStore, RfdFolderChooser, default_export_download_path,
};

/// Builds a type-erased [`LiveExportStarter`] that drives [`ExportController`] over
/// [`TlExportSource<LiveExportRpc<C>>`].
#[must_use]
pub fn build_live_export_starter<C: RpcClient + Clone + 'static>(
    rpc: C,
    bridge: Arc<LiveBridgeState>,
    options: LiveExportOptions,
) -> LiveExportStarter {
    Arc::new(move |target, settings, skip_signal| {
        let export_rpc = LiveExportRpc::with_bridge(rpc.clone(), Arc::clone(&bridge));
        let source = TlExportSource::with_utc_offset(export_rpc, options.utc_offset_secs);
        let paced = PacedExportSource::new(source, Duration::ZERO, skip_signal);

        let (mut controller, handle) = match target {
            ExportTarget::SingleTopic {
                peer,
                topic_root_id,
                peer_id,
                topic_title,
            } => ExportController::with_topic(
                paced,
                peer.clone(),
                *topic_root_id,
                *peer_id,
                topic_title.clone(),
            ),
            ExportTarget::SingleChat { peer, .. } => ExportController::new(paced, peer.clone()),
            ExportTarget::FullAccount => ExportController::new(paced, settings.single_peer.clone()),
        };

        if options.skip_path_normalization {
            controller.set_skip_path_normalization(true);
        }

        let settings_owned = settings.clone();
        let mut env = Environment::default_english();
        env.internal_links_domain = options.internal_links_domain.clone();
        let bridge_done = Arc::clone(&bridge);

        let fut = Box::pin(async move {
            let final_state = controller.run(&settings_owned, &env).await;
            bridge_done.notify_ui();
            final_state
        });

        (handle, fut)
    })
}

/// Extension trait adding `ExportManager` helpers to [`LiveSession`] in the shell crate.
pub trait LiveSessionExportExt {
    /// Creates a [`LiveExportStarter`] wired to this session's live [`TlExportSource`]
    /// using default [`LiveExportOptions`].
    fn live_export_starter(&self) -> LiveExportStarter;
    /// Creates a [`LiveExportStarter`] wired to this session's live [`TlExportSource`]
    /// using custom [`LiveExportOptions`].
    fn live_export_starter_with_options(&self, options: LiveExportOptions) -> LiveExportStarter;
    /// Builds a ready-to-use [`ExportManager`] wired to this live session.
    fn build_live_export_manager(&self) -> ExportManager;
    /// Ensures `manager` is wired to this live session and opens the single-chat export panel.
    fn open_single_chat_export_in_manager(
        &self,
        manager: &mut ExportManager,
        peer_id: PeerId,
    ) -> Result<(), String>;
    /// Ensures `manager` is wired to this live session and opens the single-topic export panel.
    fn open_single_topic_export_in_manager(
        &self,
        manager: &mut ExportManager,
        peer_id: PeerId,
        topic_root_id: i32,
        topic_title: impl Into<String>,
    ) -> Result<(), String>;
}

impl LiveSessionExportExt for LiveSession {
    fn live_export_starter(&self) -> LiveExportStarter {
        self.live_export_starter_with_options(LiveExportOptions::default())
    }

    fn live_export_starter_with_options(&self, options: LiveExportOptions) -> LiveExportStarter {
        build_live_export_starter(
            self.runtime().rpc().clone(),
            Arc::clone(self.bridge_state()),
            options,
        )
    }

    fn build_live_export_manager(&self) -> ExportManager {
        let mut manager = ExportManager::with_dependencies_and_runtime(
            Box::new(RfdFolderChooser),
            Box::new(PlatformFolderOpener),
            Box::new(QtFileSettingsStore::new(QtFileSettingsStore::default_path())),
            default_export_download_path(),
            Duration::ZERO,
            Some(self.runtime().tokio_handle().clone()),
        );
        manager.set_live_export_starter(self.live_export_starter());
        manager
    }

    fn open_single_chat_export_in_manager(
        &self,
        manager: &mut ExportManager,
        peer_id: PeerId,
    ) -> Result<(), String> {
        let (input_peer, title) = self
            .resolve_export_peer(peer_id)
            .ok_or_else(|| format!("Cannot resolve peer {} for export.", peer_id.0))?;
        manager.set_tokio_handle(self.runtime().tokio_handle().clone());
        manager.set_live_export_starter(self.live_export_starter());
        manager.start_single_chat_export(input_peer, title);
        Ok(())
    }

    fn open_single_topic_export_in_manager(
        &self,
        manager: &mut ExportManager,
        peer_id: PeerId,
        topic_root_id: i32,
        topic_title: impl Into<String>,
    ) -> Result<(), String> {
        let (input_peer, _) = self
            .resolve_export_peer(peer_id)
            .ok_or_else(|| format!("Cannot resolve forum peer {} for export.", peer_id.0))?;
        manager.set_tokio_handle(self.runtime().tokio_handle().clone());
        manager.set_live_export_starter(self.live_export_starter());
        manager.start_single_topic_export(input_peer, topic_root_id, peer_id.0 as u64, topic_title);
        Ok(())
    }
}

/// Wires `live`'s [`LiveExportStarter`] and Tokio runtime handle into [`AppExportHost`].
pub fn install_live_export_hooks(ctx: &egui::Context, theme_mode: ThemeMode, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    AppExportHost::set_live_export_starter_for_session(
        ctx,
        theme_mode,
        key,
        live.live_export_starter(),
        Some(live.runtime().tokio_handle().clone()),
    );
}

/// Ensures [`AppExportHost`] is bound to `live` (idempotent O(1) check).
pub fn ensure_live_export_hooks(ctx: &egui::Context, theme_mode: ThemeMode, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    if AppExportHost::bound_session_key(ctx, theme_mode) != Some(key) {
        install_live_export_hooks(ctx, theme_mode, live);
    }
}

/// Clears the live export starter from [`AppExportHost`] (on logout, account switch, or shutdown).
pub fn clear_live_export_hooks(ctx: &egui::Context, theme_mode: ThemeMode) {
    AppExportHost::clear_live_export_starter(ctx, theme_mode);
}

/// Clears any previous live export starter and, when `live` is `Some`, installs `live`'s starter.
pub fn sync_live_export_hooks(
    ctx: Option<&egui::Context>,
    theme_mode: ThemeMode,
    live: Option<&LiveSession>,
) {
    let Some(ctx) = ctx else {
        return;
    };
    clear_live_export_hooks(ctx, theme_mode);
    if let Some(session) = live {
        install_live_export_hooks(ctx, theme_mode, session);
    }
}
