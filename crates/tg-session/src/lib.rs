//! `tg-session`: runtime account lifecycle, MTProto-to-API RPC glue, and session actor.
//!
//! Port of the runtime parts of `Telegram/SourceFiles/main/main_account.cpp`,
//! `main/main_session.cpp`, and `main/main_domain.cpp`:
//! - [`rpc`]: [`MtprotoRpc`] newtype implementing [`tg_api::RpcClient`] over [`tg_mtproto::Client`],
//!   error mapping, and updates / `new_session_created` broadcast bridging.
//! - [`config`]: Runtime `RUSTGRAM_API_ID` / `RUSTGRAM_API_HASH` environment loading and
//!   `initConnection` parameter assembly from `tg-platform`, `tg-core`, and `tg-lang`.
//! - [`account`]: Working directory opening via `tg-core`, passcode unlock, per-account
//!   [`tg_storage::MtpAuthorization`] load/save, session resume, login completion,
//!   logout (`auth.logOut` + [`tg_mtproto::KeysDestroyer`] + storage cleanup), and
//!   multi-account add/switch/remove.
//! - [`runtime`]: [`SessionRuntime`] owning a dedicated Tokio runtime thread,
//!   [`tg_data::Session`], [`tg_api::UpdatesHandler`], [`tg_api::DialogsLoader`],
//!   [`tg_api::HistoryLoader`], [`tg_api::MessageSender`], [`tg_api::ReadHistoryManager`],
//!   and [`tg_api::SendProgressManager`], exposing a channel-based UI API plus
//!   [`tg_data::ChangeEvent`] notifications.

pub mod account;
pub mod config;
pub mod error;
pub mod proxy;
pub mod rpc;
pub mod runtime;
pub mod updates_store;

pub use account::{
    AccountManager, AccountSlotInfo, LiveAccount, LoginFlow, MAX_ACCOUNTS, PREMIUM_MAX_ACCOUNTS,
    WorkingDirOpenOptions,
};
pub use config::{
    ENV_API_HASH, ENV_API_ID, build_mtproto_client_config, core_env_to_mtproto,
    load_api_credentials_from_env, load_api_credentials_from_vars, mtproto_env_to_core,
};
pub use error::{Result, SessionError};
pub use proxy::{
    DEFAULT_PROXY_CHECK_TIMEOUT, PROXY_ROTATION_CHECK_INTERVAL, PROXY_ROTATION_CHECK_LIFETIME,
    PROXY_ROTATION_MAX_ACTIVE_CHECKS, ProxyCheckFn, ProxyCheckFuture, ProxyRotatedCallback,
    ProxyRotationRunner, ProxyRotationState, WebProxyCheckTarget, check_proxy,
    check_web_proxy_endpoint, core_proxy_to_transport, load_proxy_settings_from_tdata,
    parse_system_proxy_url, resolve_effective_proxy, resolve_system_proxy,
    save_proxy_settings_to_tdata, start_web_proxy_bridge, transport_proxy_to_core,
    web_proxy_transport_factory,
};
pub use rpc::{
    DecodedMtpPush, MtprotoRpc, decode_updates_msg, map_invocation_error, map_rpc_error,
};
pub use runtime::{
    DialogSummary, SessionEvent, SessionRuntime, SessionRuntimeConfig, SessionSnapshot,
};
pub use tg_data::PersistedUpdatesState;
pub use tg_mtproto::{
    BridgeEvent, BridgeOutbound, ConnectionState, WebProxyBridgeServer, WebProxyPacketConnection,
    WebProxyState, WebProxyTransportFactory, active_web_proxy_bridge, generate_bridge_nonce,
    register_active_web_proxy_bridge, unregister_active_web_proxy_bridge, web_proxy_bridge_url,
};
pub use tg_storage::{
    AutoDownloadOverride, AutoDownloadSource as StoredAutoDownloadSource,
    AutoDownloadType as StoredAutoDownloadType, StoredAutoDownload, StoredReactionId,
    StoredSessionSettings, StoredThreadId, StoredUnreviewedAuth,
};
pub use updates_store::{
    FileUpdatesStateStore, InMemoryUpdatesStateStore, UPDATES_STATE_PREFS_KEY, UpdatesStateStore,
};
