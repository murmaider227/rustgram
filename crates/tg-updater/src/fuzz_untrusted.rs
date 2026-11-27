//! Panic fuzz over downloaded release metadata and update packages (TGB-3065).

use tg_fuzz::Fuzz;

const ITERATIONS: usize = 3_000;

const TOKENS: &[&str] = &[
    "[",
    "]",
    "{",
    "}",
    ":",
    ",",
    "\"",
    "\"tag_name\"",
    "\"v1.2.3\"",
    "\"assets\"",
    "\"name\"",
    "\"browser_download_url\"",
    "\"prerelease\"",
    "\"draft\"",
    "true",
    "null",
    "v",
    "1",
    ".",
    "-",
    "alpha",
    "+",
    "0",
    "99999999999",
    "  ",
    "*",
    "\n",
    "rustgram.zip",
    "0123456789abcdef",
    "../",
    "\\",
    "C:",
    "\u{e9}",
];

#[test]
fn release_metadata_and_packages_do_not_panic() {
    let mut fuzz = Fuzz::new(0x0BDA_0001, ITERATIONS);
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 24);
        let bytes = if fuzz.rng().one_in(2) {
            fuzz.mutate(text.as_bytes())
        } else {
            fuzz.bytes(200)
        };
        fuzz.check("updater", i, &(&text, &bytes), || {
            let _ = crate::github::parse_releases_json(text.as_bytes());
            let _ = crate::github::parse_sha256_sums(&text, "rustgram.zip");
            let _ = crate::github::decode_sha256_hex(&text);
            let _ = crate::decode_public_key_hex(&text);
            let _ = crate::version::Version::parse(&text);
            let _ = crate::package::is_safe_archive_path(&text);
            let _ = crate::package::parse_envelope(&bytes);
        });
    }
    fuzz.finish();
}
