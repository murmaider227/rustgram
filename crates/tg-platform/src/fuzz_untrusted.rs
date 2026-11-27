//! Panic fuzz over URLs, file names, passkey (WebAuthn/CTAP/caBLE) data and instance IPC
//! (TGB-3065).

use tg_fuzz::Fuzz;

const ITERATIONS: usize = 3_000;

const URL_TOKENS: &[&str] = &[
    "https://",
    "http://",
    "ms-settings:",
    "file:",
    "tg://",
    "mailto:",
    "javascript:",
    "//",
    "/",
    "\\",
    "?",
    "#",
    "@",
    ":",
    "[",
    "]",
    "::1",
    "127.0.0.1",
    "xn--",
    "example.com",
    ".",
    "..",
    "C:",
    "con",
    "aux.txt",
    "a.exe",
    ".lnk",
    "\u{202e}",
    "\u{200b}",
    "%2e",
    "%00",
    " ",
    "\t",
    "\u{430}",
    "\u{3002}",
    "\"",
    "<",
    ">",
    "|",
    "*",
];

#[test]
fn urls_file_names_and_ipc_strings_do_not_panic() {
    let mut fuzz = Fuzz::new(0x91A7_0001, ITERATIONS);
    let base = std::env::temp_dir();
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(URL_TOKENS, 12);
        fuzz.check("platform strings", i, &s, || {
            let _ = crate::url_policy::validate_windows_settings_uri(&s);
            let _ = crate::url_policy::validate_external_url(&s);
            let _ = crate::url_policy::is_external_url_allowed(&s);
            if let Some((from, till)) = crate::url_policy::extract_url_host_range(&s) {
                let _ = s.get(from..till);
            }
            let _ = crate::url_policy::host_in_domains(&s, &["telegram.org", "t.me"]);
            let _ = crate::url_policy::hidden_url_requires_confirmation(&s);
            let _ = crate::url_policy::find_suspicious_host_ranges(&s);
            let _ = crate::file_safety::sanitize_filename(&s);
            let _ = crate::file_safety::split_stem_and_ext(&s);
            let _ = crate::file_safety::resolve_safe_path_in_dir(&base, &s);
            let _ = crate::single_instance::unescape_from_7bit(&s);
            let _ = crate::single_instance::parse_activation_commands(&s);
            let _ = crate::single_instance::parse_response(&s);
            let _ = crate::autostart::parse_command_line_args(&s);
            let _ = crate::app_id::parse_guid_bytes(&s);
            let _ = crate::notifications::parse_toast_activation(&s, &[(s.clone(), s.clone())]);
            let _ = crate::widget_sandbox::parse_inherited_handle_token(&s);
            let _ = crate::webauthn::decode_base64_any(&s);
        });
    }
    fuzz.finish();
}

#[test]
fn passkey_and_ble_bytes_do_not_panic() {
    const JSON: &[&str] = &[
        "{",
        "}",
        "[",
        "]",
        ":",
        ",",
        "\"",
        "\"type\"",
        "\"challenge\"",
        "\"origin\"",
        "\"rp\"",
        "\"id\"",
        "\"name\"",
        "\"user\"",
        "\"displayName\"",
        "\"pubKeyCredParams\"",
        "\"alg\"",
        "-7",
        "\"timeout\"",
        "\"allowCredentials\"",
        "\"excludeCredentials\"",
        "\"publicKey\"",
        "\"webauthn.create\"",
        "\"AAAA\"",
        "\"_-\"",
        "4294967296",
        "null",
        "true",
    ];
    let mut fuzz = Fuzz::new(0x91A7_0002, ITERATIONS);
    // CBOR map {1: h'0102', 2: "a", 3: [1, -1]} and a minimal authenticator-data-shaped buffer.
    let cbor_seed: Vec<u8> = vec![
        0xa3, 0x01, 0x42, 0x01, 0x02, 0x02, 0x61, 0x61, 0x03, 0x82, 0x01, 0x20,
    ];
    let mut auth_seed = vec![7u8; 32];
    auth_seed.extend_from_slice(&[0x41, 0, 0, 0, 1]);
    auth_seed.extend_from_slice(&[0u8; 16]);
    auth_seed.extend_from_slice(&[0, 4, 1, 2, 3, 4]);
    auth_seed.extend_from_slice(&cbor_seed);
    let ble_seed: Vec<u8> = vec![2, 1, 6, 7, 0x16, 0xf1, 0xff, 1, 2, 3, 4];
    for i in 0..fuzz.iterations() {
        let data = match fuzz.rng().below(5) {
            0 => fuzz.mutate(&cbor_seed),
            1 => fuzz.mutate(&auth_seed),
            2 => fuzz.mutate(&ble_seed),
            3 => fuzz.text(JSON, 30).into_bytes(),
            _ => fuzz.bytes(120),
        };
        let data_type = fuzz.rng().next_u64() as u8;
        let mut with_status = vec![0u8];
        with_status.extend_from_slice(&data);
        fuzz.check("passkey bytes", i, &data, || {
            let _ = crate::webauthn::cbor_decode(&data);
            let _ = crate::webauthn::parse_authenticator_data(&data);
            let _ = crate::webauthn::credential_id_from_auth_data(&data);
            let _ = crate::webauthn::parse_attestation_object(&data);
            let _ = crate::webauthn::parse_post_handshake_message(&data);
            let _ = crate::webauthn::parse_user_handle(&data);
            let _ = crate::webauthn::parse_client_data(&data);
            let _ = crate::webauthn::deserialize_register_data(&data);
            let _ = crate::webauthn::deserialize_login_data(&data);
            let _ = crate::webauthn::parse_ctap_make_credential_response(&with_status);
            let _ = crate::webauthn::parse_ctap_get_assertion_response(&with_status);
            let _ = crate::cable_tunnel::parse_ctap_make_credential_request(&data);
            let _ = crate::cable_tunnel::parse_ctap_get_assertion_request(&data);
            let _ = crate::cable_scanner::parse_raw_ble_ad_packet(&data);
            let _ = crate::cable_scanner::parse_cable_service_data_section(data_type, &data);
            let mut reader = std::io::Cursor::new(&data);
            let _ = crate::widget_sandbox::read_framed_from(&mut reader, 64);
        });
    }
    fuzz.finish();
}
