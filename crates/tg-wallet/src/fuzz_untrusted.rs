//! Panic fuzz over TON links, addresses, BOC cells and wallet API JSON (TGB-3065).

use std::sync::Arc;

use tg_fuzz::Fuzz;

use crate::cell::CellBuilder;

const ITERATIONS: usize = 3_000;

fn boc_seed() -> Vec<u8> {
    let build = || -> Result<Vec<u8>, crate::WalletError> {
        let mut inner = CellBuilder::new();
        inner.store_text_comment("hello \u{e9}")?;
        let inner = inner.end_cell()?;
        let mut root = CellBuilder::new();
        root.store_uint(0x0f8a_7ea5, 32)?;
        root.store_uint(7, 64)?;
        root.store_coins(1_000_000_000)?;
        root.store_addr_none()?;
        root.store_ref(Arc::new(inner))?;
        crate::boc::serialize_boc(&root.end_cell()?)
    };
    build().unwrap_or_default()
}

const STRING_TOKENS: &[&str] = &[
    "ton://transfer/",
    "https://app.tonkeeper.com/transfer/",
    "tc://",
    "tonkeeper://",
    "?",
    "&",
    "amount=",
    "text=",
    "bin=",
    "jetton=",
    "init=",
    "id=",
    "r=",
    "v=",
    "ret=",
    "EQ",
    "UQ",
    "kQ",
    "0:",
    "-1:",
    "ABCDEFabcdef0123456789",
    "_-",
    "+/",
    "=",
    "%7B",
    "%22",
    "1.5",
    "0.000000001",
    "99999999999999999999",
    "-",
    ".",
    ",",
    "e",
    "data: ",
    "event: ",
    "id: ",
    "\n\n",
    "\r\n",
];

const JSON_TOKENS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    ":",
    ",",
    "\"",
    "\"balance\"",
    "\"status\"",
    "\"code\"",
    "\"error\"",
    "\"result\"",
    "\"jetton_masters\"",
    "\"jetton_wallets\"",
    "\"nft_items\"",
    "\"address\"",
    "\"decimals\"",
    "\"metadata\"",
    "\"transactions\"",
    "\"trace\"",
    "\"children\"",
    "\"in_msg\"",
    "\"out_msgs\"",
    "\"value\"",
    "\"method\"",
    "\"params\"",
    "\"id\"",
    "\"from\"",
    "\"message\"",
    "\"messages\"",
    "\"valid_until\"",
    "\"network\"",
    "\"payload\"",
    "\"stateInit\"",
    "\"amount\"",
    "-1",
    "0",
    "18446744073709551616",
    "1e400",
    "null",
    "true",
    "\"9\"",
    "\"\u{e9}\"",
];

#[test]
fn wallet_strings_and_links_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7A11_0001, ITERATIONS * 5);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(STRING_TOKENS, 12);
        let decimals = fuzz.rng().below(40) as u8;
        fuzz.check("wallet strings", i, &s, || {
            let _ = crate::address::TonAddress::parse(&s);
            let _ = crate::address::parse_transfer_link(&s);
            let _ = crate::parse_ton_amount(&s);
            let _ = crate::jettons::parse_jetton_amount(&s, decimals);
            let _ = crate::mtproto_api::parse_server_tx_hash(&s);
            let _ = crate::mtproto_api::decode_server_encrypted_comment(&s);
            let _ = crate::phrase_shares::PhraseShare::decode_string(&s);
            let _ = crate::toncenter::parse_stream_endpoint(&s);
            let _ = crate::tonconnect::manifest_url_host(&s);
            let _ = crate::tonconnect::parse_client_id(&s);
            let _ = crate::tonconnect::valid_https_url(&s);
            let _ = crate::tonconnect::parse_ton_connect_link(&s);
            let _ = crate::tonconnect::parse_sse_events(&s);
        });
    }
    fuzz.finish();
}

#[test]
fn wallet_bytes_and_json_do_not_panic() {
    let mut fuzz = Fuzz::new(0x7A11_0002, ITERATIONS);
    let boc = boc_seed();
    assert!(!boc.is_empty(), "BOC seed must serialize");
    for i in 0..fuzz.iterations() {
        let data = match fuzz.rng().below(3) {
            0 => fuzz.mutate(&boc),
            1 => fuzz.text(JSON_TOKENS, 30).into_bytes(),
            _ => fuzz.bytes(160),
        };
        fuzz.check("wallet bytes", i, &data, || {
            if let Ok(roots) = crate::boc::deserialize_boc(&data) {
                for root in &roots {
                    let _ = crate::jettons::parse_jetton_transfer_body(root);
                    let _ = crate::jettons::parse_nft_transfer_body(root);
                    let _ = crate::boc::serialize_boc(root);
                }
            }
            let _ = crate::emulate::parse_emulation_response(&data);
            let _ = crate::jettons::parse_jetton_masters(&data);
            let _ = crate::jettons::parse_jetton_wallets(&data, 10);
            let _ = crate::toncenter::parse_api_error(&data);
            let _ = crate::toncenter::parse_address_info(&data);
            let _ = crate::toncenter::parse_emulated_trace(&data);
            let _ = crate::toncenter::parse_nft_items(&data, 10);
            let _ = crate::toncenter::parse_stream_event(&data);
            let _ = crate::tonconnect::parse_app_request_json(&data);
            let _ = crate::tonconnect::TonConnectClaimStore::from_bytes(&data);
            let _ = crate::phrase_shares::PhraseShare::from_bytes(&data);
            let _ = crate::phrase_shares::words_from_seed(&data);
            let _ = crate::phrase_shares::parse_decrypted_key_part(&data);
            let _ = crate::vault::parse_device_keyring(&data);
            let _ = crate::vault::read_vault_record_key_id(&data);
        });
    }
    fuzz.finish();
}
