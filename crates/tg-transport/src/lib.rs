//! `tg-transport`: MTProto TCP/HTTP framing, Obfuscated2, proxies (SOCKS5, HTTP CONNECT,
//! MTProxy plain/`dd`/`ee` fake-TLS), and priority connection racing.
//!
//! Port of `mtproto/connection_abstract.*`, `mtproto/connection_tcp.*`,
//! `mtproto/connection_http.*`, `mtproto/connection_resolving.*`,
//! `mtproto/mtproto_proxy_data.*`, `mtproto/details/mtproto_abstract_socket.*`,
//! `mtproto/details/mtproto_tcp_socket.*`, `mtproto/details/mtproto_tls_socket.*`,
//! and connection racing from `mtproto/session_private.cpp`.

pub mod connection;
pub mod error;
pub mod framing;
pub mod http_transport;
pub mod obfuscated;
pub mod proxy;
pub mod proxy_dial;
pub mod race;
pub mod tls;

pub use connection::{
    CONSTRUCTOR_REQ_PQ, CONSTRUCTOR_REQ_PQ_MULTI, CONSTRUCTOR_RES_PQ, Connection, ConnectionConfig,
    FULL_CONNECTION_TIMEOUT, ReceivedFrame, ResPqProbe, TEST_MODE_DC_ID_SHIFT,
    build_unencrypted_req_pq_multi, build_unencrypted_req_pq_multi_with_msg_id,
    parse_unencrypted_res_pq, protocol_dc_id,
};
pub use error::{ERROR_CODE_OTHER, Error, Socks5Reply, TransportErrorCode};
pub use framing::{
    Abridged, AnyTransport, FrameKind, Full, Intermediate, MAX_PACKET_SIZE, PLAIN_INIT_ABRIDGED,
    PaddedIntermediate, TAG_ABRIDGED, TAG_INTERMEDIATE, TAG_PADDED_INTERMEDIATE, Transport,
    TransportKind, UnpackedOffset,
};
pub use http_transport::{FORCE_HTTP_PORT, HttpConnection, HttpConnectionConfig};
pub use obfuscated::{OBFUSCATED_HEADER_SIZE, Obfuscated2, is_good_start_nonce};
pub use proxy::{
    MtprotoSecret, ProxyData, ProxySettings, ProxyStatus, ProxyType,
    compute_web_proxy_bridge_capability, decode_mtproto_secret, decode_web_proxy_link_secret,
    encode_web_proxy_link_secret, normalize_web_proxy_base_path, normalize_web_proxy_host,
    valid_web_proxy_base_path, validate_secret_bytes,
};
pub use proxy_dial::{
    ONE_CONNECTION_TIMEOUT, PrefixedStream, dial_tcp, dial_through_proxy, format_host_port,
    http_connect_handshake, socks5_handshake,
};
pub use race::{
    RaceCandidate, RaceEndpoint, RaceOptions, RacedConnection, RacedTransport,
    WAIT_FOR_BETTER_TIMEOUT, compute_priority, race_connections,
};
pub use tls::{
    CLIENT_HEADER, CLIENT_HELLO_LIMIT, CLIENT_PART_SIZE, CLIENT_PREFIX, ClientHello,
    HELLO_DIGEST_LENGTH, MAX_GREASE, MAX_SERVER_HELLO_LENGTH, SERVER_HEADER,
    SERVER_HELLO_DIGEST_POSITION, SERVER_HELLO_PART1, SERVER_HELLO_PART3, TlsBlock, TlsStream,
    default_client_hello_rules, generate_client_hello, generate_client_hello_with_rules,
    generate_x25519_public_key, prepare_greases, verify_server_hello,
};

#[cfg(test)]
mod fuzz_untrusted;
