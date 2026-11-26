//! Live Telegram session facade combining `tg-app-live` with shell-level `live_host` wiring.

pub use tg_app_live::{
    DiscussionReadState, ErasedRpcClient, ForumTopicEdit, LiveAudioDataSource, LiveBridgeState,
    LiveForwardOptions, LiveLangPackHandle, LiveMediaCache, LivePlaylistProvider, LivePollOptions,
    LiveProxyController, LiveRuntimeBridge, LiveSession, LiveSessionHandle, ProlongWebViewHandle,
    ProxyIndicatorState, RawRpcClient, ResolvedMediaLocation, RuntimeBridgedRpc,
    SetLanguageLinkOutcome, TelegramBroadcastChunkLoader, TelegramMediaInvoker,
    TelegramRangeLoader, UpdatedFileReferences, document_thumb_location, download_file_bytes,
    download_file_bytes_async, download_file_bytes_with_progress_async,
    download_file_bytes_with_refresh_async, format_proxy_connection_status,
    is_file_reference_error, map_mtproto_connection_state, proxy_indicator_state,
    refresh_file_reference_async, start_mtproto_erased_on, tl_page_to_iv_page,
    tl_webpage_to_iv_page,
};
pub use tg_app_live::{
    admin, bots, bridge, business, calls, chat_helpers, chat_import, chat_l233, chatlists,
    communities, compose_extras, convert, deep_links, emoji_pack, geo_map, history_sections,
    langpack, layer233_extras, media, media_cache, passport_upload, payments, profile_extras,
    proxy, read_metrics, rich_message, search_and_info, session, settings_extra, settings_sync,
    star_gifts, stats, stories, suggestions, toggling_media, transcribe, upload, userpic_images,
    video_userpic, web_proxy_carrier, webview, wp40_polish,
};

pub use crate::live_host::host;
pub use crate::live_host::host::{
    LiveActiveWorkspace, LiveAppHost, LiveBootstrapPrefetch, LiveHostState,
};
pub use crate::live_host::multi_account;
pub use crate::live_host::multi_account::{
    ManagedAccountEntry, ManagedAccountSummary, MultiAccountFrameAction, MultiAccountNotifyBackend,
    MultiAccountNotifyState, MultiAccountWorkspaceHost, PendingAccountChatOpen,
};
pub use crate::live_host::video_stream_loop;
pub use crate::window::shortcuts_wiring;

pub mod dice_pack {
    pub use crate::live_host::video_stream_loop::populate_video_stream_metadata;
    pub use tg_app_live::dice_pack::*;
}

pub mod export {
    pub use crate::live_host::export_host::*;
    pub use tg_app_live::export::*;
}

pub mod themes {
    pub use crate::live_host::themes_host::*;
    pub use tg_app_live::themes::*;
}

pub mod wallet {
    pub use crate::live_host::wallet_host::*;
    pub use tg_app_live::wallet::*;
}

pub mod compose_ai {
    pub use crate::media::round_video_capture::{
        CapturedRoundVideo, ROUND_VIDEO_SIDE_PX, RoundVideoPreviewFrame, RoundVideoWebcamCapture,
        available_round_video_cameras, export_round_video_mp4, frame_to_square_rgba,
        is_headless_camera_mode,
    };
    pub use tg_app_live::compose_ai::*;
}

pub mod passkeys {
    pub use crate::intro::passkey_rpc::{
        build_login_credential_tl, finish_passkey_login_async, init_passkey_login_async,
    };
    pub use tg_app_live::passkeys::*;
}

pub mod widgets {
    pub use crate::webview::widget_host::LiveWidgetHost;
    pub use tg_app_live::widgets::*;
}
