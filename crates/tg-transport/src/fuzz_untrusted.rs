//! Panic regression and fuzz tests over untrusted-input entry points (TGB-3065).

#[test]
fn proxy_link_with_multibyte_char_at_a_scheme_boundary_does_not_panic() {
    // Old code sliced `s[..prefix.len()]` before checking the char boundary.
    for link in [
        "tg:/\u{e9}x",
        "https://t.me/prox\u{e9}",
        "http:/\u{e9}",
        "https://t.\u{e9}",
    ] {
        assert!(crate::proxy::ProxyData::from_link(link).is_err());
    }
}

use tg_fuzz::Fuzz;

use crate::framing::{PaddedIntermediate, Transport, TransportKind};

const ITERATIONS: usize = 5_000;

#[test]
fn wire_frames_and_server_hello_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7A45_0001, ITERATIONS);
    let kinds = [
        TransportKind::Abridged,
        TransportKind::Intermediate,
        TransportKind::PaddedIntermediate,
        TransportKind::Full,
    ];
    for i in 0..fuzz.iterations() {
        let kind = *fuzz.rng().pick(&kinds);
        // `pack` takes our own non-empty, 4-byte-aligned payloads; only `unpack` sees wire data.
        let payload_len = 4 * (1 + fuzz.rng().below(300));
        let payload: Vec<u8> = (0..payload_len)
            .map(|_| fuzz.rng().next_u64() as u8)
            .collect();
        let mut packed = Vec::new();
        kind.build().pack(&payload, &mut packed);
        let input = if fuzz.rng().one_in(4) {
            fuzz.bytes(64)
        } else {
            fuzz.mutate(&packed)
        };
        fuzz.check("unpack", i, &(kind, &input), || {
            let mut transport = kind.build();
            let mut offset = 0;
            while offset < input.len() {
                let Ok(unpacked) = transport.unpack(&input[offset..]) else {
                    break;
                };
                let _ = unpacked.slice(&input[offset..]);
                if unpacked.next_offset == 0 {
                    break;
                }
                offset += unpacked.next_offset;
            }
            let _ = PaddedIntermediate::strip_mtproto_padding(&input);
            let _ = crate::connection::parse_unencrypted_res_pq(&input);
            let _ = crate::tls::verify_server_hello(&input, &[7; 32], &[9; 16]);
            let _ = crate::proxy::MtprotoSecret::from_bytes(&input);
        });
    }
    fuzz.finish();
}

#[test]
fn proxy_links_and_secrets_do_not_panic() {
    const TOKENS: &[&str] = &[
        "tg://proxy?",
        "tg://socks?",
        "https://t.me/proxy?",
        "t.me/webproxy?",
        "server=",
        "port=",
        "secret=",
        "user=",
        "pass=",
        "&",
        "ee",
        "dd",
        "0123456789abcdef",
        "%",
        "%2",
        "=",
        "-",
        "_",
        "example.com",
        "1.2.3.4",
        "[::1]",
        "0x",
        "xn--",
        "65536",
        "-1",
    ];
    let mut fuzz = Fuzz::new(0x7A45_0002, ITERATIONS * 4);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(TOKENS, 14);
        fuzz.check("proxy strings", i, &s, || {
            if let Ok(proxy) = crate::proxy::ProxyData::from_link(&s) {
                let _ = proxy.parsed_mtproto_secret();
                let _ = proxy.secret_from_mtproto_password();
            }
            let _ = crate::proxy::decode_mtproto_secret(&s);
            let _ = crate::proxy::decode_web_proxy_link_secret(&s, true);
            let _ = crate::proxy::decode_web_proxy_link_secret(&s, false);
            let _ = crate::proxy::normalize_web_proxy_host(&s);
        });
    }
    fuzz.finish();
}
