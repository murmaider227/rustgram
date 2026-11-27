//! Panic fuzz over card fields, money amounts, currencies and payment provider JSON (TGB-3065).

use tg_fuzz::Fuzz;

use crate::card::YearMonth;

const ITERATIONS: usize = 3_000;

const TOKENS: &[&str] = &[
    "4",
    "42",
    "4242",
    "5",
    "34",
    "37",
    "6011",
    "35",
    "0",
    "1",
    "9",
    " ",
    "/",
    "-",
    ".",
    ",",
    "12",
    "13",
    "00",
    "99",
    "\u{a0}",
    "\u{e9}",
    "\u{663}",
    "USD",
    "JPY",
    "EUR",
    "BHD",
    "XTR",
    "TON",
    "xx",
    "{",
    "}",
    ":",
    "\"",
    "\"publishable_key\"",
    "\"need_country\"",
    "\"need_zip\"",
    "\"public_token\"",
    "\"tokenize_url\"",
    "\"apple_pay_merchant_id\"",
    "true",
    "null",
    "stripe",
    "smartglocal",
];

#[test]
fn card_money_and_provider_input_does_not_panic() {
    let mut fuzz = Fuzz::new(0x9A11_0001, ITERATIONS * 3);
    for i in 0..fuzz.iterations() {
        let a = fuzz.text(TOKENS, 10);
        let b = fuzz.text(TOKENS, 6);
        let amount = fuzz.rng().int(1 << 40);
        let threshold = YearMonth {
            year: 2000 + fuzz.rng().below(60) as u32,
            month: fuzz.rng().below(14) as u32,
        };
        fuzz.check("payments", i, &(&a, &b, amount), || {
            let _ = crate::card::possible_brands_for_number(&a);
            let _ = crate::card::is_valid_luhn(&a);
            let _ = crate::card::validate_card(&a);
            let _ = crate::card::validate_expire_date(&a, threshold);
            let _ = crate::card::parse_expire_date_mm_yy(&a);
            let _ = crate::card::validate_cvc(&a, &b);
            let _ = crate::card::card_number_format(&a);
            let _ = crate::card::format_card_number(&a);
            let _ = crate::currency::lookup_currency_rule(&b);
            let _ = crate::currency::currency_name(&b);
            let _ = crate::currency::fill_amount_and_currency(amount, &b, true);
            let _ = crate::currency::format_money_field_value(amount, &b);
            let _ = crate::currency::parse_money_field_value(&a, &b);
            let _ = crate::star_gift_craft::parse_craft_too_early_error(&a);
            let _ = crate::form::parse_native_payment_method(&b, &a, "UA", true);
        });
    }
    fuzz.finish();
}
