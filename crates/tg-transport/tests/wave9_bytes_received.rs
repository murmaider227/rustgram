//! Wave 9-U (TGB-3147): `Connection::bytes_received` counts socket reads before a packet is
//! complete, so the session's receive watchdog sees a large reply that is still arriving.

use tg_transport::{Connection, ConnectionConfig, TransportKind};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn bytes_received_grows_while_a_packet_is_still_arriving() {
    let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);
    let config = ConnectionConfig {
        transport: TransportKind::Intermediate,
        obfuscated: false,
        ..ConnectionConfig::default()
    };
    let mut conn = Connection::from_stream(client_io, config)
        .await
        .expect("connection over an in-memory stream");
    assert_eq!(conn.bytes_received(), 0);

    let payload: Vec<u8> = (0..64u8).collect();
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    let (head, tail) = frame.split_at(4 + 20);
    server_io.write_all(head).await.expect("write head");

    // Poll `recv` once: it reads what is buffered and then waits for the rest.
    tokio::select! {
        biased;
        res = conn.recv() => panic!("a partial packet was returned: {res:?}"),
        () = std::future::ready(()) => {}
    }
    assert_eq!(conn.bytes_received(), head.len() as u64);

    server_io.write_all(tail).await.expect("write tail");
    let got = conn.recv().await.expect("complete packet");
    assert_eq!(got, payload);
    assert_eq!(conn.bytes_received(), frame.len() as u64);
}
