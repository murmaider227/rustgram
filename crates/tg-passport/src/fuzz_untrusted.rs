//! Panic fuzz over decrypted passport data, passport config JSON and form field validation
//! (TGB-3065).

use tg_fuzz::Fuzz;

const ITERATIONS: usize = 3_000;

const TOKENS: &[&str] = &[
    "{",
    "}",
    ":",
    ",",
    "\"",
    "\"first_name\"",
    "\"birth_date\"",
    "\"gender\"",
    "\"country_code\"",
    "\"residence_country_code\"",
    "\"expiry_date\"",
    "\"document_no\"",
    "\"street_line1\"",
    "\"post_code\"",
    "\"AU\"",
    "\"en\"",
    "01.01.2000",
    "31.02.2000",
    "99.99.9999",
    ".",
    "male",
    "female",
    "UA",
    "+",
    "0",
    "1",
    "-",
    "\u{e9}",
    "\u{1f1fa}",
    "null",
    "1e400",
    "[]",
];

#[test]
fn passport_data_and_fields_do_not_panic() {
    let mut fuzz = Fuzz::new(0x9A55_0001, ITERATIONS);
    let secret = [5u8; crate::encryption::SECRET_SIZE];
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 20);
        let padding_tail = fuzz.bytes(40);
        let hash = fuzz.rng().int(1 << 20) as i32;
        fuzz.check("passport", i, &(&text, &padding_tail), || {
            let _ = crate::encryption::deserialize_data(text.as_bytes());
            let _ = crate::types::PassportConfig::parse_json(hash, text.as_bytes());
            if let Ok(encrypted) = crate::encryption::encrypt_data_deterministic(
                text.as_bytes(),
                &secret,
                &padding_tail,
            ) {
                let _ = crate::encryption::decrypt_data(&encrypted.bytes, &encrypted.hash, &secret);
                let _ =
                    crate::encryption::decrypt_data(&encrypted.bytes, &encrypted.hash, &[1; 32]);
            }
            let _ = crate::encryption::decrypt_data(text.as_bytes(), &[0; 32], &secret);
            let _ =
                crate::encryption::decrypt_value_secret(&padding_tail, &secret, text.as_bytes());
            let _ = crate::encryption::unlock_master_secret(&padding_tail, &[3; 64], 1);
            let _ = crate::validation::native_names_visibility(&text, &text);
            let _ = crate::validation::validate_latin_name(&text);
            let _ = crate::validation::validate_native_name(&text);
            let _ = crate::validation::validate_document_no(&text);
            let _ = crate::validation::validate_date(&text);
            let _ = crate::validation::validate_gender(&text);
            let _ = crate::validation::validate_country_code(&text);
            let _ = crate::validation::validate_street(&text);
            let _ = crate::validation::validate_city(&text);
            let _ = crate::validation::validate_postcode(&text);
            let _ = crate::validation::postprocess_phone(&text);
            let _ = crate::types::validate_http_url(&text);
        });
    }
    fuzz.finish();
}
