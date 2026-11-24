//! Opt-in real-network smoke test against Telegram's official TEST data centers only.
//!
//! Ignored by default and additionally gated by `RUSTGRAM_TEST_DC=1` (or legacy `TGFORK_TEST_DC=1`) so routine CI / `cargo test`
//! runs never touch the network.

#[allow(dead_code)]
#[path = "../examples/test_dc_smoke.rs"]
mod test_dc_smoke;

#[tokio::test]
#[ignore = "connects to Telegram official TEST DCs only; requires RUSTGRAM_TEST_DC=1"]
async fn live_telegram_test_dc_smoke() {
    let enabled = std::env::var("RUSTGRAM_TEST_DC").as_deref() == Ok("1")
        || std::env::var("TGFORK_TEST_DC").as_deref() == Ok("1");
    if !enabled {
        eprintln!("Skipping live_telegram_test_dc_smoke: set RUSTGRAM_TEST_DC=1 to run");
        return;
    }
    test_dc_smoke::run_smoke()
        .await
        .expect("Telegram test-DC smoke test failed");
}
