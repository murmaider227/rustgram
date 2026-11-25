//! `tg-mtproto`: Tokio-based Rust port of Telegram Desktop's MTProto 2.0 session layer.
//!
//! Ported from `Telegram/SourceFiles/mtproto/` (`session.cpp`, `session_private.cpp`,
//! `mtp_instance.cpp`, `mtproto_dc_options.cpp`, `mtproto_config.cpp`, `config_loader.cpp`,
//! `special_config_request.cpp`, `mtproto_response.cpp`, `facade.h`, `sender.h`,
//! `dedicated_file_loader.cpp`, and `details/mtproto_*`).
//!
//! Integrated with the real workspace crates [`tg_crypto`] (`crypto`), [`tg_tl`] (`tl`),
//! and [`tg_transport`] (`transport`).

pub mod auth_key;
pub mod client;
pub mod config;
pub mod config_loader;
pub mod dc_options;
pub mod error;
pub mod file_loader;
pub mod keys_destroyer;
pub mod message;
mod qdatastream;
pub mod received_ids;
pub mod salts;
pub mod session;
pub mod special_config;
pub mod time;
pub mod transport_adapter;
pub mod web_proxy;

/// Real `tg-crypto` crate re-export.
pub use tg_crypto as crypto;
/// Real `tg-tl` crate re-export (Layer 233 API + MTProto generated schemas).
pub use tg_tl as tl;
/// Real `tg-transport` crate re-export.
pub use tg_transport as transport;

pub use auth_key::{AuthKeyType, DcKeyRequest, DcKeyResult, Dcenter, ManagedAuthKey};
pub use client::{Client, InvokeOptions, RequestBuilder, RpcClient, Sender};
pub use config::{ClientConfig, ClientProxyInfo, Config, ConfigFields};
pub use config_loader::{
    CONFIG_BECOMES_OLD, CONFIG_BECOMES_OLD_FOR_BLOCKED, ConfigLoader, ConfigLoaderOptions,
    ConfigSchedulerHandle, ENUMERATE_DC_TIMEOUT, SPECIAL_REQUEST_TIMEOUT,
};
pub use dc_options::{
    DcEndpoint, DcId, DcOptionFlags, DcOptions, DcType, Environment, ShiftedDcId,
    destroy_key_next_dc_id, temporary_dc_id_from_real,
};
pub use error::{InvocationError, MigrateKind, RpcError};
pub use file_loader::{DedicatedFileDownloader, DownloadProgress};
pub use keys_destroyer::{KeyDestroyOutcome, KeysDestroyResult, KeysDestroyer};
pub use message::{
    MAX_RECEIVE_ALLOC_BYTES, MAX_RECEIVE_MESSAGES, MAX_RECEIVE_NESTING, ReceiveBudget, RequestId,
};
pub use salts::SaltManager;
pub use session::{
    ConnectionState, HTTP_WAIT_MAX_DELAY_MS, HTTP_WAIT_MAX_WAIT_MS, HTTP_WAIT_WAIT_AFTER_MS,
    SessionAction, SessionEvent, SessionHandle, SessionState, UpdatesMsg,
};
pub use special_config::{
    DnsEntry, DohProvider, SpecialConfigError, SpecialEndpoint, check_phone_by_prefixes_rules,
    concatenate_dns_txt_fields, decrypt_simple_config, extract_special_endpoints,
    fetch_special_config, parse_dns_response, parse_http_date, sync_http_unixtime,
};
pub use tg_crypto::AuthKey;
pub use tg_tl::{
    Boxed, Cursor, Deserializable, DeserializeError, Identifiable, LAYER, RawObject, RemoteCall,
    Serializable,
};
pub use tg_transport::{
    Connection, ConnectionConfig, Error as TransportError, HttpConnection, HttpConnectionConfig,
    ProxyData, ProxySettings, ProxyType, RaceCandidate, RaceEndpoint, RaceOptions, RacedConnection,
    RacedTransport, TransportKind, race_connections,
};
pub use time::MtprotoClock;
pub use transport_adapter::{
    FakeConnection, FakeServerPeer, FakeTransportFactory, HTTP_WAIT_TRANSPORT_TIMEOUT,
    PacketConnection, TgHttpConnection, TgTcpConnection, TgTransportFactory, TransportFactory,
    build_race_candidates,
};
pub use web_proxy::{
    BridgeEvent, BridgeOutbound, DATA_FRAME_SIZE, FRAME_HEADER_SIZE, Frame, FrameType,
    HttpRequestAction, INITIAL_STREAM_WINDOW, MAX_BATCH_FRAMES, MAX_FRAME_PAYLOAD,
    MAX_LOCAL_WEBSOCKET_PAYLOAD, MAX_STREAM_ID, OutboundNotifier, StreamEvent,
    WebProxyBridgeServer, WebProxyByteStream, WebProxyError, WebProxyPacketConnection,
    WebProxySession, WebProxyState, WebProxyTransportFactory, WebSocketMessage, WebviewBridgeState,
    active_web_proxy_bridge, bridge_init_script, check_web_proxy, compute_websocket_accept,
    connect_over_web_proxy_bridge, connect_via_active_web_proxy, decode_server_websocket_frames,
    decode_websocket_client_frames, decode_window_payload, encode_masked_websocket_frame,
    encode_websocket_frame, encode_window_payload, generate_bridge_nonce, parse_frames,
    parse_local_http_request, probe_web_proxy_http_url, probe_web_proxy_ws_addr,
    register_active_web_proxy_bridge, serialize_frame, unregister_active_web_proxy_bridge,
    web_proxy_bridge_capability, web_proxy_bridge_path, web_proxy_bridge_url,
    web_proxy_webview_url,
};
