//! Unit and known-answer tests for `tg-payments`.

use std::cell::RefCell;

use tg_data::{ChannelId, DocumentId, PeerId, UserId};
use tg_payments::card::{
    CardBrand, CardField, CardFundingType, NativeMethodDetails, UncheckedCardDetails,
    ValidationState, YearMonth, card_field_for_local_error, card_field_for_stripe_error,
    card_number_format, detect_card_brand, format_card_number, format_cardholder_name,
    format_expire_date_input, is_valid_luhn, override_expire_date_threshold,
    parse_expire_date_mm_yy, validate_card, validate_card_details, validate_cvc,
    validate_expire_date, validate_parsed_expire_date,
};
use tg_payments::checkout::{
    CheckoutAction, CheckoutMode, CheckoutResult, CheckoutStateMachine, SubmitState,
    VerificationFlowState,
};
use tg_payments::credits::{
    CreditsAmount, CreditsBalanceTracker, CreditsHistoryFilter, CreditsHistoryPaginator,
    CreditsHistoryPeerType, CreditsType, MessageMoneyRestriction, PaidReactionConfig,
    build_get_stars_transactions, stars_giveaway_options_from_tl, stars_status_from_tl,
    stars_topup_options_from_tl,
};
use tg_payments::currency::{
    CREDITS_CURRENCY, MoneyFieldState, clean_money_state, fill_amount_and_currency,
    format_money_field_value, known_currency, lookup_currency_rule, parse_money_field_value,
    postprocess_money_result,
};
use tg_payments::form::{
    FormError, FormErrorKind, FormLoadEvent, InformationField, InvoiceTarget, NewCredentials,
    PaymentForm, PostAddress, RequestedInformation, SendPaymentAction,
};
use tg_payments::premium::{
    BoostsOffsetToken, GiveawayState, LevelLimits, PremiumLimits, boosts_list_from_tl,
    boosts_status_from_tl, checked_gift_code_from_tl, giveaway_info_from_tl, my_boosts_from_tl,
    premium_promo_from_tl, premium_subscription_options_from_tl,
};
use tg_payments::providers::{
    CardParams, HttpClient, HttpMethod, HttpRequest, HttpResponse, HttpTransportError,
    SmartGlocalClient, SmartGlocalConfig, SmartGlocalErrorCode, StripeClient, StripeConfig,
    StripeErrorCode, build_smartglocal_token_request, build_stripe_token_request,
    compute_smartglocal_api_url, smartglocal_last4, stripe_form_encode,
};
use tg_payments::star_gifts::{
    GiftAttributeId, GiftAttributeIdType, ResaleGiftsFilter, ResaleGiftsSort, RgbColor,
    StarGiftLifecycleState, UniqueGiftAttribute, UniqueGiftRarity, build_get_resale_star_gifts,
    resale_star_gifts_from_tl, saved_star_gift_from_tl, star_gift_from_tl,
};
use tg_tl::{Deserializable, Serializable, enums as tl_enums, types as tl_types};

struct MockHttpClient {
    captured: RefCell<Vec<HttpRequest>>,
    response: Result<HttpResponse, HttpTransportError>,
}

impl MockHttpClient {
    fn ok(status_code: u16, body: &str) -> Self {
        Self {
            captured: RefCell::new(Vec::new()),
            response: Ok(HttpResponse {
                status_code,
                body: body.as_bytes().to_vec(),
            }),
        }
    }

    fn err(code: i32, message: &str) -> Self {
        Self {
            captured: RefCell::new(Vec::new()),
            response: Err(HttpTransportError {
                code,
                message: message.to_string(),
            }),
        }
    }
}

impl HttpClient for MockHttpClient {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        self.captured.borrow_mut().push(request.clone());
        self.response.clone()
    }
}

// ============================================================================
// Card Luhn, Brand Detection, Expiry, CVC, Country/ZIP Validation
// ============================================================================

#[test]
fn luhn_and_card_brand_known_answers() {
    // Standard Stripe test card numbers (known Luhn-valid test vectors).
    let vectors = [
        ("4242424242424242", CardBrand::Visa, vec![4, 4, 4, 4]),
        ("4136000000008", CardBrand::Visa, vec![4, 4, 4, 4]), // 13-digit Visa BIN
        ("5555555555554444", CardBrand::MasterCard, vec![4, 4, 4, 4]),
        ("2223003122003222", CardBrand::MasterCard, vec![4, 4, 4, 4]),
        ("378282246310005", CardBrand::Amex, vec![4, 6, 5]),
        ("371449635398431", CardBrand::Amex, vec![4, 6, 5]),
        ("30569309025904", CardBrand::DinersClub, vec![4, 4, 4, 4]),
        ("36123456789013", CardBrand::DinersClub, vec![4, 6, 4]),
        ("6011111111111117", CardBrand::Discover, vec![4, 4, 4, 4]),
        ("3530111333300000", CardBrand::Jcb, vec![4, 4, 4, 4]),
        ("6200000000000005", CardBrand::UnionPay, vec![4, 4, 4, 4]),
    ];

    for (number, expected_brand, expected_fmt) in vectors {
        assert!(is_valid_luhn(number), "expected Luhn valid for {number}");
        let res = validate_card(number);
        assert_eq!(res.state, ValidationState::Valid, "card {number}");
        assert_eq!(res.brand, expected_brand, "brand for {number}");
        assert_eq!(detect_card_brand(number), expected_brand);
        assert_eq!(card_number_format(number), expected_fmt);
    }

    // Invalid Luhn (last digit flipped)
    assert!(!is_valid_luhn("4242424242424241"));
    let bad = validate_card("4242 4242 4242 4241");
    assert_eq!(bad.state, ValidationState::Invalid);
    assert_eq!(bad.brand, CardBrand::Visa);

    // Incomplete (< 13 digits)
    let short = validate_card("424242");
    assert_eq!(short.state, ValidationState::Incomplete);
    assert_eq!(short.brand, CardBrand::Visa);

    // Too long (> 19 digits)
    let long = validate_card("42424242424242424242");
    assert_eq!(long.state, ValidationState::Invalid);

    // Non-numeric
    assert_eq!(validate_card("4242abcd").state, ValidationState::Invalid);
    assert_eq!(validate_card("").state, ValidationState::Incomplete);

    // Formatting groups
    assert_eq!(format_card_number("378282246310005"), "3782 822463 10005");
    assert_eq!(format_card_number("36123456789016"), "3612 345678 9016");
    assert_eq!(
        format_card_number("4242424242424242"),
        "4242 4242 4242 4242"
    );
}

#[test]
fn expiry_cvc_and_billing_validation_rules() {
    let threshold = YearMonth::new(2026, 6);

    assert_eq!(
        validate_expire_date("06/26", threshold).state,
        ValidationState::Valid
    );
    assert!(validate_expire_date("12/28", threshold).finished);
    assert_eq!(
        validate_expire_date("05/26", threshold).state,
        ValidationState::Invalid
    );
    assert_eq!(
        validate_expire_date("13/28", threshold).state,
        ValidationState::Invalid
    );
    assert_eq!(
        validate_expire_date("00/28", threshold).state,
        ValidationState::Invalid
    );
    assert_eq!(
        validate_expire_date("0", threshold).state,
        ValidationState::Incomplete
    );
    assert_eq!(
        validate_parsed_expire_date(12, 2027, threshold),
        ValidationState::Valid
    );
    assert_eq!(
        validate_parsed_expire_date(12, 1999, threshold),
        ValidationState::Invalid
    );

    // Russian +7 phone override threshold -> 2022-02
    let ru = override_expire_date_threshold("+79991234567").unwrap();
    assert_eq!(ru, YearMonth::new(2022, 2));
    assert_eq!(
        validate_expire_date("03/22", ru).state,
        ValidationState::Valid
    );
    assert_eq!(override_expire_date_threshold("+14155551234"), None);

    // Incremental MM/YY input formatting
    assert_eq!(format_expire_date_input("5"), "05/");
    assert_eq!(format_expire_date_input("1228"), "12/28");
    assert_eq!(format_expire_date_input("15"), "15");
    assert_eq!(parse_expire_date_mm_yy("08/29"), Some((8, 2029)));
    assert_eq!(format_cardholder_name("Ada Lovelace"), "ADA LOVELACE");

    // CVC: 3 digits for Visa, up to 4 digits for Amex
    let visa_cvc = validate_cvc("4242424242424242", "123");
    assert_eq!(visa_cvc.state, ValidationState::Valid);
    assert!(visa_cvc.finished);
    assert_eq!(
        validate_cvc("4242424242424242", "1234").state,
        ValidationState::Invalid
    );
    let amex_cvc3 = validate_cvc("378282246310005", "123");
    assert_eq!(amex_cvc3.state, ValidationState::Valid);
    assert!(!amex_cvc3.finished);
    let amex_cvc4 = validate_cvc("378282246310005", "1234");
    assert_eq!(amex_cvc4.state, ValidationState::Valid);
    assert!(amex_cvc4.finished);

    // Full card details with billing requirements
    let native = NativeMethodDetails {
        supported: true,
        need_cardholder_name: true,
        need_country: true,
        need_zip: true,
        ..NativeMethodDetails::default()
    };
    let mut details = UncheckedCardDetails {
        number: "4242424242424242".to_string(),
        cvc: "123".to_string(),
        expire_month: 12,
        expire_year: 2029,
        cardholder_name: String::new(),
        address_country: "US".to_string(),
        address_zip: "94107".to_string(),
    };
    assert_eq!(
        validate_card_details(&details, &native, threshold),
        Err("LOCAL_CARD_HOLDER_NAME_INVALID")
    );
    details.cardholder_name = "ADA".to_string();
    details.address_country = "USA".to_string();
    assert_eq!(
        validate_card_details(&details, &native, threshold),
        Err("LOCAL_CARD_BILLING_COUNTRY_INVALID")
    );
    details.address_country = "US".to_string();
    details.address_zip.clear();
    assert_eq!(
        validate_card_details(&details, &native, threshold),
        Err("LOCAL_CARD_BILLING_ZIP_INVALID")
    );
    details.address_zip = "94107".to_string();
    assert!(validate_card_details(&details, &native, threshold).is_ok());

    assert_eq!(
        card_field_for_local_error("LOCAL_CARD_CVC_INVALID"),
        Some(CardField::Cvc)
    );
    assert_eq!(
        card_field_for_stripe_error("ExpiredCard"),
        Some(CardField::ExpireDate)
    );
}

// ============================================================================
// Currency Formatting & Money Input Field
// ============================================================================

#[test]
fn currency_rules_and_money_field() {
    assert!(known_currency("USD"));
    assert!(known_currency("EUR"));
    assert!(known_currency("JPY"));
    assert!(known_currency("BHD"));
    assert!(!known_currency("ZZZ"));

    assert_eq!(fill_amount_and_currency(123456, "USD", false), "$1,234.56");
    assert_eq!(fill_amount_and_currency(10000, "USD", true), "$100");
    assert_eq!(fill_amount_and_currency(500, "JPY", false), "\u{00A5}500");
    assert_eq!(
        fill_amount_and_currency(12500, "EUR", false),
        "125,00 \u{20AC}"
    );
    assert_eq!(
        fill_amount_and_currency(250, CREDITS_CURRENCY, false),
        "\u{2B50}250"
    );

    let usd = lookup_currency_rule("USD");
    let cleaned = clean_money_state(
        usd,
        MoneyFieldState {
            value: "0012345.678".to_string(),
            position: 11,
        },
    );
    assert_eq!(cleaned.value, "12345.67");
    let formatted = postprocess_money_result(usd, cleaned);
    assert_eq!(formatted.value, "12 345.67");
    assert_eq!(parse_money_field_value(&formatted.value, "USD"), 1234567);
    assert_eq!(format_money_field_value(1234567, "USD"), "12 345.67");
}

// ============================================================================
// Stripe & SmartGlocal Tokenization (Mocked HTTP)
// ============================================================================

#[test]
fn stripe_tokenization_request_and_response() {
    let config = StripeConfig {
        publishable_key: "pk_test_12345".to_string(),
        company_name: "Telegram".to_string(),
    };
    let card = CardParams {
        number: "4242424242424242".to_string(),
        exp_month: 12,
        exp_year: 2029,
        cvc: "123".to_string(),
        name: "Ada Lovelace".to_string(),
        address_zip: "94107".to_string(),
        address_country: "US".to_string(),
        ..CardParams::default()
    };

    let req = build_stripe_token_request(&config, &card);
    assert_eq!(req.method, HttpMethod::Post);
    assert_eq!(req.url, "https://api.stripe.com/v1/tokens");
    assert!(req.headers.contains(&(
        "Authorization".to_string(),
        "Bearer pk_test_12345".to_string()
    )));
    assert!(
        req.headers
            .contains(&("Stripe-Version".to_string(), "2015-10-12".to_string()))
    );

    let body_str = String::from_utf8(req.body.clone()).unwrap();
    // Sorted keys, percent-encoded brackets & space
    assert!(body_str.contains("card%5Baddress_country%5D=US"));
    assert!(body_str.contains("card%5Bname%5D=Ada%20Lovelace"));
    assert!(body_str.contains("card%5Bnumber%5D=4242424242424242"));
    assert!(!body_str.contains("address_line1")); // empty fields omitted

    let encoded_direct = stripe_form_encode("card", &card.form_field_values());
    assert_eq!(encoded_direct, req.body);

    let mock_ok = MockHttpClient::ok(
        200,
        r#"{
            "id": "tok_visa_test",
            "livemode": false,
            "created": 1700000000,
            "card": {
                "id": "card_1",
                "last4": "4242",
                "brand": "Visa",
                "funding": "credit",
                "exp_month": 12,
                "exp_year": 2029
            }
        }"#,
    );
    let client = StripeClient::new(config.clone(), mock_ok);
    let token = client.create_token_with_card(&card).unwrap();
    assert_eq!(token.token_id, "tok_visa_test");
    assert!(!token.livemode);
    let tok_card = token.card.as_ref().unwrap();
    assert_eq!(tok_card.brand, CardBrand::Visa);
    assert_eq!(tok_card.funding, CardFundingType::Credit);
    assert_eq!(tok_card.title(), "visa *4242");
    let creds_json: serde_json::Value =
        serde_json::from_slice(&token.to_credentials_json()).unwrap();
    assert_eq!(creds_json["type"], "card");
    assert_eq!(creds_json["id"], "tok_visa_test");

    // Stripe card error decoding
    let mock_err = MockHttpClient::ok(
        402,
        r#"{"error":{"type":"card_error","code":"expired_card","message":"Card expired","param":"exp_year"}}"#,
    );
    let err = StripeClient::new(config.clone(), mock_err)
        .create_token_with_card(&card)
        .unwrap_err();
    assert_eq!(err.code, StripeErrorCode::Card);
    assert_eq!(err.description, "ExpiredCard");
    assert_eq!(err.parameter, "expYear");

    // Transport error
    let mock_net = MockHttpClient::err(7, "connection refused");
    let net_err = StripeClient::new(config, mock_net)
        .create_token_with_card(&card)
        .unwrap_err();
    assert_eq!(net_err.code, StripeErrorCode::Network);
}

#[test]
fn smartglocal_url_validation_and_tokenization() {
    // Valid custom tokenize_url
    let custom = SmartGlocalConfig {
        public_token: "pub_tok_1".to_string(),
        tokenize_url: "https://custom.smart-glocal.com/cds/v1/tokenize/card".to_string(),
        is_test: false,
    };
    assert_eq!(
        compute_smartglocal_api_url(&custom),
        "https://custom.smart-glocal.com/cds/v1/tokenize/card"
    );

    // Invalid custom URLs (wrong scheme, evil host, extra query, port) must fall back safely!
    for bad_url in [
        "http://tgb.smart-glocal.com/cds/v1/tokenize/card",
        "https://evil.example.com/cds/v1/tokenize/card",
        "https://tgb.smart-glocal.com:8443/cds/v1/tokenize/card",
        "https://tgb.smart-glocal.com/cds/v1/tokenize/card?x=1",
        "https://tgb.smart-glocal.com/wrong/path",
    ] {
        let cfg = SmartGlocalConfig {
            public_token: "pub_tok_1".to_string(),
            tokenize_url: bad_url.to_string(),
            is_test: true,
        };
        assert_eq!(
            compute_smartglocal_api_url(&cfg),
            "https://tgb-playground.smart-glocal.com/cds/v1/tokenize/card",
            "should reject {bad_url}"
        );
    }

    let live_default = SmartGlocalConfig {
        public_token: "pub_tok_live".to_string(),
        tokenize_url: String::new(),
        is_test: false,
    };
    assert_eq!(
        compute_smartglocal_api_url(&live_default),
        "https://tgb.smart-glocal.com/cds/v1/tokenize/card"
    );

    let card = CardParams {
        number: "4242424242424242".to_string(),
        exp_month: 4,
        exp_year: 2028,
        cvc: "999".to_string(),
        ..CardParams::default()
    };
    let req = build_smartglocal_token_request(&live_default, &card);
    assert!(
        req.headers
            .contains(&("X-PUBLIC-TOKEN".to_string(), "pub_tok_live".to_string()))
    );
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["card"]["expiration_month"], "04");
    assert_eq!(body["card"]["expiration_year"], "28");

    assert_eq!(smartglocal_last4("424242******4242"), "4242");
    assert_eq!(smartglocal_last4("12345678"), ""); // no non-digit prefix

    let mock_ok = MockHttpClient::ok(
        200,
        r#"{
            "status": "ok",
            "data": {
                "token": "sg_tok_abc",
                "info": {
                    "card_type": "VISA",
                    "card_network": "visa",
                    "masked_card_number": "424242******4242"
                }
            }
        }"#,
    );
    let tok = SmartGlocalClient::new(live_default.clone(), mock_ok)
        .create_token_with_card(&card)
        .unwrap();
    assert_eq!(tok.token_id, "sg_tok_abc");
    assert_eq!(tok.card.as_ref().unwrap().title(), "visa *4242");

    let mock_err = MockHttpClient::ok(
        400,
        r#"{"status":"error","error":{"code":"INVALID_CARD","description":"Bad card"}}"#,
    );
    let err = SmartGlocalClient::new(live_default, mock_err)
        .create_token_with_card(&card)
        .unwrap_err();
    assert_eq!(err.code, SmartGlocalErrorCode::Unknown);
    assert_eq!(err.description, "INVALID_CARD");
}

// ============================================================================
// Payment Form & Checkout State Machine (incl. 3-D Secure)
// ============================================================================

fn sample_fiat_payment_form_tl() -> tl_enums::payments::PaymentForm {
    tl_enums::payments::PaymentForm::PaymentForm(tl_types::payments::PaymentForm {
        can_save_credentials: true,
        password_missing: false,
        form_id: 998877,
        bot_id: 1001,
        title: "Coffee Subscription".to_string(),
        description: "Fresh beans monthly".to_string(),
        photo: None,
        invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
            test: true,
            name_requested: true,
            phone_requested: false,
            email_requested: true,
            shipping_address_requested: true,
            flexible: true,
            phone_to_provider: false,
            email_to_provider: false,
            recurring: false,
            currency: "USD".to_string(),
            prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                tl_types::LabeledPrice {
                    label: "Beans".to_string(),
                    amount: 1500,
                },
            )],
            max_tip_amount: Some(1000),
            suggested_tip_amounts: Some(vec![100, 300, 500]),
            terms_url: Some("https://shop.example/terms".to_string()),
            subscription_period: None,
        }),
        provider_id: 2002,
        url: "https://pay.example/form".to_string(),
        native_provider: Some("stripe".to_string()),
        native_params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data:
                r#"{"publishable_key":"pk_test_xyz","need_zip":true,"need_cardholder_name":true}"#
                    .to_string(),
        })),
        additional_methods: None,
        saved_info: Some(tl_enums::PaymentRequestedInfo::PaymentRequestedInfo(
            tl_types::PaymentRequestedInfo {
                name: Some("Ada Lovelace".to_string()),
                phone: None,
                email: Some("ada@example.com".to_string()),
                shipping_address: Some(tl_enums::PostAddress::PostAddress(tl_types::PostAddress {
                    street_line1: "10 Downing St".to_string(),
                    street_line2: String::new(),
                    city: "London".to_string(),
                    state: String::new(),
                    country_iso2: "GB".to_string(),
                    post_code: "SW1A 2AA".to_string(),
                })),
            },
        )),
        saved_credentials: Some(vec![tl_enums::PaymentSavedCredentials::Card(
            tl_types::PaymentSavedCredentialsCard {
                id: "saved_1".to_string(),
                title: "visa *4242".to_string(),
            },
        )]),
        users: Vec::new(),
    })
}

#[test]
fn checkout_state_machine_full_flow_with_3ds_verification() {
    let input_invoice = InvoiceTarget::Slug("coffee_slug".to_string())
        .to_input_invoice()
        .unwrap();
    let mut sm = CheckoutStateMachine::new(
        CheckoutMode::Payment,
        PaymentForm::new("+4412345678", "GB"),
        input_invoice,
    );

    // 1. Load form -> triggers initial silent validation because saved_info satisfies local checks
    let actions = sm.on_payment_form_loaded(&sample_fiat_payment_form_tl());
    assert!(matches!(
        actions.as_slice(),
        [CheckoutAction::SendValidateRequestedInfo { silent: true, .. }]
    ));

    // 2. Server returns ValidatedRequestedInfo with 2 shipping options
    let validated_tl = tl_enums::payments::ValidatedRequestedInfo::ValidatedRequestedInfo(
        tl_types::payments::ValidatedRequestedInfo {
            id: Some("req_info_42".to_string()),
            shipping_options: Some(vec![
                tl_enums::ShippingOption::ShippingOption(tl_types::ShippingOption {
                    id: "std".to_string(),
                    title: "Standard".to_string(),
                    prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                        tl_types::LabeledPrice {
                            label: "Shipping".to_string(),
                            amount: 500,
                        },
                    )],
                }),
                tl_enums::ShippingOption::ShippingOption(tl_types::ShippingOption {
                    id: "exp".to_string(),
                    title: "Express".to_string(),
                    prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                        tl_types::LabeledPrice {
                            label: "Express".to_string(),
                            amount: 1200,
                        },
                    )],
                }),
            ]),
        },
    );
    let actions = sm.on_validate_finished(&validated_tl, None);
    assert_eq!(actions, vec![CheckoutAction::ShowForm]);
    assert_eq!(sm.submit_state(), SubmitState::Validated);

    // 3. Tips clamped to tips_max (1000)
    sm.form_mut().set_tips(2500);
    assert_eq!(sm.form().invoice.tips_selected, 1000);

    // 4. Submit before choosing shipping -> ChooseShippingOption
    assert_eq!(
        sm.on_submit(None),
        vec![CheckoutAction::ChooseShippingOption]
    );
    sm.form_mut().set_shipping_option("exp");
    assert_eq!(
        sm.form()
            .invoice
            .total_amount(sm.form().shipping_options.selected()),
        1500 + 1200 + 1000
    );

    // 5. Submit before accepting terms -> RequestTermsAcceptance
    let actions = sm.on_submit(None);
    assert!(matches!(
        actions.as_slice(),
        [CheckoutAction::RequestTermsAcceptance { .. }]
    ));

    // 6. Accept terms & submit with saved card but no tmp_password -> RequestTmpPassword
    let actions = sm.on_accept_terms_and_submit(None);
    assert_eq!(actions, vec![CheckoutAction::RequestTmpPassword]);

    // 7. Submit with tmp_password before bot trusted -> RequestBotTrust
    let actions = sm.on_submit(Some(b"tmp_pass_bytes"));
    assert!(matches!(
        actions.as_slice(),
        [CheckoutAction::RequestBotTrust {
            bot_id,
            provider_id
        }] if *bot_id == UserId(1001) && *provider_id == UserId(2002)
    ));

    // 8. Trust bot & submit -> SendPaymentForm (verify TL round-trip!)
    let actions = sm.on_trust_and_submit(Some(b"tmp_pass_bytes"));
    assert_eq!(sm.submit_state(), SubmitState::Finishing);
    assert!(sm.is_payment_registered());
    let send_req = match actions.as_slice() {
        [CheckoutAction::SendPaymentForm(req)] => req.clone(),
        other => panic!("expected SendPaymentForm, got {other:?}"),
    };
    assert_eq!(send_req.form_id, 998877);
    assert_eq!(send_req.requested_info_id.as_deref(), Some("req_info_42"));
    assert_eq!(send_req.shipping_option_id.as_deref(), Some("exp"));
    assert_eq!(send_req.tip_amount, Some(1000));
    let wire = send_req.to_bytes();
    let decoded = tg_tl::functions::payments::SendPaymentForm::from_bytes(&wire).unwrap();
    assert_eq!(decoded, send_req);

    // 9. Server demands 3-D Secure verification URL
    let ver_tl = tl_enums::payments::PaymentResult::PaymentVerificationNeeded(
        tl_types::payments::PaymentVerificationNeeded {
            url: "https://3ds.bank.example/auth".to_string(),
        },
    );
    let actions = sm.on_payment_result(ver_tl);
    assert_eq!(
        sm.verification_state(),
        &VerificationFlowState::Pending {
            url: "https://3ds.bank.example/auth".to_string()
        }
    );
    assert!(matches!(
        actions.as_slice(),
        [CheckoutAction::OpenVerificationUrl { url, .. }] if url == "https://3ds.bank.example/auth"
    ));

    // 10. Non-local navigation inside 3DS allowed; Telegram local redirect closes as Paid!
    let (allow, nav_actions) = sm.on_verification_navigation_attempt(false);
    assert!(allow);
    assert!(nav_actions.is_empty());

    let (allow_local, paid_actions) = sm.on_verification_navigation_attempt(true);
    assert!(!allow_local);
    assert_eq!(sm.verification_state(), &VerificationFlowState::Completed);
    assert_eq!(
        paid_actions,
        vec![CheckoutAction::Close(CheckoutResult::Paid)]
    );
}

#[test]
fn checkout_webview_credentials_and_error_mapping() {
    let input_invoice = InvoiceTarget::Slug("slug_web".to_string())
        .to_input_invoice()
        .unwrap();
    let mut form = PaymentForm::new("", "");
    form.details.password_missing = true;
    form.details.can_save_credentials = false;
    let mut sm = CheckoutStateMachine::new(CheckoutMode::Payment, form, input_invoice);

    let msg = r#"["payment_form_submit","{\"title\":\"Visa *1111\",\"credentials\":{\"type\":\"card\",\"id\":\"tok_1\"}}"]"#;
    let actions = sm.on_webview_message(msg, true);
    assert_eq!(
        actions,
        vec![CheckoutAction::ShowForm, CheckoutAction::RequestSetPassword]
    );
    assert_eq!(sm.form().payment_method.new_credentials.title, "Visa *1111");
    assert!(sm.form().has_changes());

    // Error routing
    assert_eq!(
        sm.on_error(FormError::new(
            FormErrorKind::Validate,
            "ADDRESS_POSTCODE_INVALID"
        )),
        vec![CheckoutAction::ShowInformationError(
            InformationField::ShippingPostcode
        )]
    );
    assert_eq!(
        sm.on_error(FormError::new(FormErrorKind::Stripe, "IncorrectCVC")),
        vec![CheckoutAction::ShowCardError(CardField::Cvc)]
    );
    assert_eq!(
        sm.on_error(FormError::new(FormErrorKind::Send, "TMP_PASSWORD_INVALID")),
        vec![CheckoutAction::RequestTmpPassword]
    );
    assert_eq!(
        sm.on_close_sure(),
        vec![CheckoutAction::Close(CheckoutResult::Failed)]
    );

    // Validate local info checks
    let mut info_form = PaymentForm::new("", "");
    info_form.invoice.is_shipping_address_requested = true;
    info_form.invoice.is_name_requested = true;
    let empty_info = RequestedInformation::default();
    assert_eq!(
        info_form
            .validate_information_local(&empty_info)
            .unwrap_err()
            .id,
        "ADDRESS_STREET_LINE1_INVALID"
    );
    let good_info = RequestedInformation {
        name: "Ada".to_string(),
        shipping_address: PostAddress {
            address1: "1 Main St".to_string(),
            city: "London".to_string(),
            country_iso2: "GB".to_string(),
            ..PostAddress::default()
        },
        ..RequestedInformation::default()
    };
    assert!(info_form.validate_information_local(&good_info).is_ok());

    // Direct SendPaymentAction with new credentials
    info_form.set_payment_credentials(NewCredentials {
        title: "visa *4242".to_string(),
        data: r#"{"id":"tok_1","type":"card"}"#.to_string(),
        save_on_server: false,
    });
    info_form.trust_bot();
    let act = info_form
        .build_send_payment_form_request(
            InvoiceTarget::Slug("x".into()).to_input_invoice().unwrap(),
            None,
        )
        .unwrap();
    assert!(matches!(act, SendPaymentAction::Send(_)));
}

#[test]
fn stars_payment_form_and_receipt_loading() {
    let mut form = PaymentForm::new("", "");
    let stars_tl = tl_enums::payments::PaymentForm::Stars(tl_types::payments::PaymentFormStars {
        form_id: 55,
        bot_id: 777,
        title: "Sticker Pack".to_string(),
        description: "Animated".to_string(),
        photo: None,
        invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
            test: false,
            name_requested: false,
            phone_requested: false,
            email_requested: false,
            shipping_address_requested: false,
            flexible: false,
            phone_to_provider: false,
            email_to_provider: false,
            recurring: false,
            currency: CREDITS_CURRENCY.to_string(),
            prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                tl_types::LabeledPrice {
                    label: "Pack".to_string(),
                    amount: 250,
                },
            )],
            max_tip_amount: None,
            suggested_tip_amounts: None,
            terms_url: None,
            subscription_period: Some(2592000),
        }),
        users: Vec::new(),
    });
    match form.apply_payment_form_tl(&stars_tl) {
        FormLoadEvent::CreditsPaymentStarted(data) => {
            assert_eq!(data.credits, 250);
            assert_eq!(data.subscription_period, 2592000);
            assert!(!data.star_gift_form);
        }
        other => panic!("expected CreditsPaymentStarted, got {other:?}"),
    }
}

// ============================================================================
// Telegram Stars, Star Gifts, Premium, Boosts & Giveaways
// ============================================================================

#[test]
fn credits_amount_tracker_and_pagination() {
    // Normalization & negative nanos
    let a = CreditsAmount::new(1, 1_500_000_000, CreditsType::Stars);
    assert_eq!(a.whole(), 2);
    assert_eq!(a.nano(), 500_000_000);

    let b = CreditsAmount::stars(0) - CreditsAmount::new(0, 250_000_000, CreditsType::Stars);
    assert_eq!(b.whole(), -1);
    assert_eq!(b.nano(), 750_000_000);
    assert!((b.value() - (-0.25)).abs() < 1e-9);
    assert_eq!(
        b.abs(),
        CreditsAmount::new(0, 250_000_000, CreditsType::Stars)
    );

    // TL round-trip for Stars and negative TON
    let ton_neg = CreditsAmount::from_nanoton(-1_500_000_000);
    assert!(ton_neg.is_ton());
    assert!((ton_neg.value() - (-1.5)).abs() < 1e-9);
    assert_eq!(CreditsAmount::from_tl(&ton_neg.to_tl()), ton_neg);

    // Balance tracker lock / unlock / withdraw
    let mut tracker = CreditsBalanceTracker::new();
    assert!(!tracker.lock(CreditsAmount::stars(10))); // not loaded yet
    tracker.apply_balance(CreditsAmount::stars(500));
    assert!(tracker.lock(CreditsAmount::stars(120)));
    assert_eq!(tracker.available_balance(), CreditsAmount::stars(380));
    assert!(!tracker.lock(CreditsAmount::stars(400))); // exceeds remaining
    assert!(tracker.unlock(CreditsAmount::stars(20)));
    assert_eq!(tracker.available_balance(), CreditsAmount::stars(400));
    assert!(tracker.withdraw_locked(CreditsAmount::stars(100)));
    assert_eq!(tracker.total_balance(), CreditsAmount::stars(400));
    assert_eq!(tracker.locked_balance(), CreditsAmount::ZERO_STARS);

    // Pagination + deduplication + TL conversions
    let owner = PeerId::from_user(UserId(1));
    let page1_tl = tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
        balance: CreditsAmount::stars(400).to_tl(),
        subscriptions: Some(vec![tl_enums::StarsSubscription::StarsSubscription(
            tl_types::StarsSubscription {
                canceled: false,
                can_refulfill: false,
                missing_balance: false,
                bot_canceled: false,
                id: "sub_1".to_string(),
                peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 99 }),
                until_date: 2_000_000_000,
                pricing: tl_enums::StarsSubscriptionPricing::StarsSubscriptionPricing(
                    tl_types::StarsSubscriptionPricing {
                        period: 2592000,
                        amount: 100,
                    },
                ),
                chat_invite_hash: None,
                title: Some("VIP Channel".to_string()),
                photo: None,
                invoice_slug: None,
            },
        )]),
        subscriptions_next_offset: None,
        subscriptions_missing_balance: None,
        history: Some(vec![tl_enums::StarsTransaction::StarsTransaction(
            tl_types::StarsTransaction {
                refund: false,
                pending: false,
                failed: false,
                gift: false,
                reaction: true,
                stargift_upgrade: false,
                business_transfer: false,
                stargift_resale: false,
                posts_search: false,
                stargift_prepaid_upgrade: false,
                stargift_drop_original_details: false,
                phonegroup_message: false,
                stargift_auction_bid: false,
                offer: false,
                id: "tx_1".to_string(),
                amount: CreditsAmount::stars(50).to_tl(),
                date: 1_700_000_100,
                peer: tl_enums::StarsTransactionPeer::StarsTransactionPeer(
                    tl_types::StarsTransactionPeer {
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 42 }),
                    },
                ),
                title: None,
                description: None,
                photo: None,
                transaction_date: None,
                transaction_url: None,
                bot_payload: None,
                msg_id: Some(12),
                extended_media: None,
                subscription_period: None,
                giveaway_post_id: None,
                stargift: None,
                floodskip_number: None,
                starref_commission_permille: None,
                starref_peer: None,
                starref_amount: None,
                paid_messages: None,
                premium_gift_months: None,
                ads_proceeds_from_date: None,
                ads_proceeds_to_date: None,
            },
        )]),
        next_offset: Some("cursor_2".to_string()),
        chats: Vec::new(),
        users: Vec::new(),
    });

    let slice1 = stars_status_from_tl(&page1_tl, owner, 1_700_000_500);
    assert!(!slice1.all_loaded);
    assert_eq!(slice1.subscriptions.len(), 1);
    assert!(!slice1.subscriptions[0].expired);
    assert_eq!(slice1.list[0].peer_type, CreditsHistoryPeerType::Peer);
    assert_eq!(slice1.list[0].actor_id, PeerId::from_user(UserId(42)));

    let mut paginator = CreditsHistoryPaginator::new();
    paginator.apply_slice(slice1.clone());
    assert_eq!(paginator.entries().len(), 1);
    assert_eq!(paginator.next_offset(), "cursor_2");
    assert!(!paginator.all_loaded());
    // Re-applying duplicate tx_1 with empty next_offset deduplicates and marks complete
    let mut slice2 = slice1;
    slice2.token.clear();
    slice2.all_loaded = true;
    paginator.apply_slice(slice2);
    assert_eq!(paginator.entries().len(), 1);
    assert!(paginator.all_loaded());

    // Filter builder: in == out clears both flags
    let req_both = build_get_stars_transactions(
        tl_enums::InputPeer::Self_,
        CreditsHistoryFilter {
            inbound: true,
            outbound: true,
            ton: false,
            ascending: false,
        },
        "",
        0,
        None,
    );
    assert!(!req_both.inbound && !req_both.outbound);
    assert_eq!(req_both.limit, 100);

    // Top-up & giveaway options + paid reaction/message costs
    let topups = stars_topup_options_from_tl(&[tl_enums::StarsTopupOption::StarsTopupOption(
        tl_types::StarsTopupOption {
            extended: false,
            stars: 100,
            store_product: None,
            currency: "USD".to_string(),
            amount: 199,
        },
    )]);
    assert_eq!(topups[0].credits, 100);
    assert_eq!(topups[0].amount, 199);

    let giveaways =
        stars_giveaway_options_from_tl(&[tl_enums::StarsGiveawayOption::StarsGiveawayOption(
            tl_types::StarsGiveawayOption {
                extended: false,
                default: true,
                stars: 1000,
                yearly_boosts: 4,
                store_product: None,
                currency: "USD".to_string(),
                amount: 1999,
                winners: vec![
                    tl_enums::StarsGiveawayWinnersOption::StarsGiveawayWinnersOption(
                        tl_types::StarsGiveawayWinnersOption {
                            default: true,
                            users: 10,
                            per_user_stars: 100,
                        },
                    ),
                ],
            },
        )]);
    assert_eq!(giveaways[0].winners[0].per_user_stars, 100);

    let reaction_cfg = PaidReactionConfig::new(500);
    assert_eq!(reaction_cfg.default_chosen(), 50);
    assert_eq!(reaction_cfg.clamp_amount(9999), 500);

    let restriction = MessageMoneyRestriction {
        stars_per_message: 25,
        premium_required: false,
        known: true,
    };
    assert!(restriction.is_restricted());
    assert_eq!(restriction.cost_for_messages(3), 75);
}

#[test]
fn star_gifts_unique_attributes_and_resale_states() {
    let unique_tl = tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: true,
        burned: false,
        crafted: false,
        id: 5001,
        gift_id: 100,
        title: "Plush Pepe".to_string(),
        slug: "PlushPepe-7".to_string(),
        num: 7,
        owner_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 })),
        owner_name: None,
        owner_address: None,
        attributes: vec![
            tl_enums::StarGiftAttribute::Model(tl_types::StarGiftAttributeModel {
                crafted: false,
                name: "Golden".to_string(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 901 }),
                rarity: tl_enums::StarGiftAttributeRarity::Legendary,
            }),
            tl_enums::StarGiftAttribute::Pattern(tl_types::StarGiftAttributePattern {
                name: "Crown".to_string(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 902 }),
                rarity: tl_enums::StarGiftAttributeRarity::StarGiftAttributeRarity(
                    tl_types::StarGiftAttributeRarity { permille: 15 },
                ),
            }),
            tl_enums::StarGiftAttribute::Backdrop(tl_types::StarGiftAttributeBackdrop {
                name: "Midnight".to_string(),
                backdrop_id: 12,
                center_color: 0x112233,
                edge_color: 0x000011,
                pattern_color: 0x445566,
                text_color: 0xFFFFFF,
                rarity: tl_enums::StarGiftAttributeRarity::Rare,
            }),
        ],
        availability_issued: 100,
        availability_total: 1000,
        gift_address: None,
        resell_amount: Some(vec![
            CreditsAmount::stars(5000).to_tl(),
            CreditsAmount::from_nanoton(25_000_000_000).to_tl(),
        ]),
        released_by: None,
        value_amount: None,
        value_currency: None,
        value_usd_amount: None,
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: Some(2500),
        craft_chance_permille: None,
    });

    let gift = star_gift_from_tl(&unique_tl);
    assert!(gift.is_unique());
    assert_eq!(gift.limited_left, 900);
    let u = gift.unique.as_ref().unwrap();
    assert_eq!(u.display_name(), "Plush Pepe #7");
    assert_eq!(u.model.attribute.rarity_type(), UniqueGiftRarity::Legendary);
    assert_eq!(u.model.attribute.rarity_text(), "Legendary");
    assert_eq!(u.pattern.attribute.rarity_text(), "1.5%");
    assert_eq!(u.backdrop.center_color, RgbColor::new(0x11, 0x22, 0x33));
    assert_eq!(u.resale_stars(), CreditsAmount::stars(5000));
    assert_eq!(u.resale_ton(), CreditsAmount::ton(25));
    assert_eq!(u.resale_asked(), CreditsAmount::stars(5000));
    assert_eq!(gift.sticker_document_id, DocumentId(901));

    let zero_permille = UniqueGiftAttribute {
        name: "Ultra".to_string(),
        rarity_value: 0,
    };
    assert_eq!(zero_permille.rarity_text(), "< 0.1%");

    // SavedStarGift lifecycle state -> ListedForResale
    let saved_tl = tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade: false,
        pinned_to_top: true,
        upgrade_separate: false,
        from_id: None,
        date: 1_700_000_000,
        gift: unique_tl.clone(),
        message: None,
        msg_id: Some(77),
        saved_id: None,
        convert_stars: None,
        upgrade_stars: None,
        can_export_at: None,
        transfer_stars: Some(25),
        can_transfer_at: Some(1_700_001_000),
        can_resell_at: None,
        collection_id: Some(vec![1, 2]),
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: Some(7),
        can_craft_at: None,
    });
    let saved = saved_star_gift_from_tl(&saved_tl, PeerId::from_user(UserId(10)), true);
    assert_eq!(
        saved.lifecycle_state(),
        StarGiftLifecycleState::ListedForResale
    );
    assert!(saved.pinned);
    assert_eq!(saved.info.unique.as_ref().unwrap().stars_for_transfer, 25);

    // Resale gifts query builder & response parsing
    let mut filter = ResaleGiftsFilter {
        sort: ResaleGiftsSort::Number,
        stars_only: true,
        ..ResaleGiftsFilter::default()
    };
    filter.attributes.insert(GiftAttributeId {
        value: 12,
        kind: GiftAttributeIdType::Backdrop,
    });
    let req = build_get_resale_star_gifts(100, &filter, "");
    assert!(req.sort_by_num);
    assert!(req.stars_only);
    assert_eq!(req.attributes.as_ref().unwrap().len(), 1);

    let resale_tl =
        tl_enums::payments::ResaleStarGifts::ResaleStarGifts(tl_types::payments::ResaleStarGifts {
            count: 1,
            gifts: vec![unique_tl],
            next_offset: None,
            attributes: None,
            attributes_hash: Some(42),
            chats: Vec::new(),
            counters: None,
            users: Vec::new(),
        });
    let resale = resale_star_gifts_from_tl(100, &resale_tl);
    assert_eq!(resale.count, 1);
    assert_eq!(resale.attributes_hash, 42);
}

#[test]
fn premium_options_promo_boosts_and_giveaways() {
    let opts_tl = vec![
        tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
            tl_types::PremiumSubscriptionOption {
                current: false,
                can_purchase_upgrade: false,
                transaction: None,
                months: 1,
                currency: "USD".to_string(),
                amount: 499,
                bot_url: "https://t.me/PremiumBot?start=1m".to_string(),
                store_product: None,
            },
        ),
        tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
            tl_types::PremiumSubscriptionOption {
                current: true,
                can_purchase_upgrade: false,
                transaction: None,
                months: 12,
                currency: "USD".to_string(),
                amount: 3599,
                bot_url: "https://t.me/PremiumBot?start=12m".to_string(),
                store_product: None,
            },
        ),
    ];
    let opts = premium_subscription_options_from_tl(&opts_tl);
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0].discount_percent, 0);
    assert_eq!(opts[0].discount, "");
    // 1 - 3599 / (499 * 12) = 1 - 3599 / 5988 = 39.896% -> 40%
    assert_eq!(opts[1].discount_percent, 40);
    assert_eq!(opts[1].discount, "\u{2212}40%");
    assert!(opts[1].current);

    let promo_tl = tl_enums::help::PremiumPromo::PremiumPromo(tl_types::help::PremiumPromo {
        status_text: "Go Premium!".to_string(),
        status_entities: Vec::new(),
        video_sections: vec!["double_limits".to_string()],
        videos: vec![tl_enums::Document::Empty(tl_types::DocumentEmpty {
            id: 555,
        })],
        period_options: opts_tl,
        users: Vec::new(),
    });
    let promo = premium_promo_from_tl(&promo_tl);
    assert_eq!(promo.monthly_amount, 499);
    assert_eq!(promo.monthly_currency, "USD");
    assert_eq!(promo.videos.get("double_limits"), Some(&DocumentId(555)));

    let limits_free = PremiumLimits::new(false);
    let limits_prem = PremiumLimits::new(true);
    assert_eq!(limits_free.channels(), 500);
    assert_eq!(limits_prem.channels(), 1000);
    assert_eq!(limits_prem.max_boost_level(false), 99);
    assert_eq!(LevelLimits::default().channel_restrict_sponsored, 20);

    // Gift code & giveaway info
    let gc_tl =
        tl_enums::payments::CheckedGiftCode::CheckedGiftCode(tl_types::payments::CheckedGiftCode {
            via_giveaway: true,
            from_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 500,
            })),
            giveaway_msg_id: Some(88),
            to_id: Some(123),
            date: 1_700_000_000,
            days: 90,
            used_date: None,
            chats: Vec::new(),
            users: Vec::new(),
        });
    let gc = checked_gift_code_from_tl(&gc_tl);
    assert!(gc.is_valid() && !gc.is_used());
    assert_eq!(gc.days, 90);
    assert_eq!(gc.from, PeerId::from_channel(ChannelId(500)));

    let ga_tl =
        tl_enums::payments::GiveawayInfo::Results(tl_types::payments::GiveawayInfoResults {
            winner: true,
            refunded: false,
            start_date: 1_700_000_000,
            gift_code_slug: Some("win_slug".to_string()),
            stars_prize: Some(500),
            finish_date: 1_700_100_000,
            winners_count: 10,
            activated_count: Some(7),
        });
    let ga = giveaway_info_from_tl(&ga_tl);
    assert_eq!(ga.state, GiveawayState::Finished);
    assert_eq!(ga.gift_code, "win_slug");
    assert_eq!(ga.credits, 500);

    // Boosts status, list, and myBoosts
    let status_tl =
        tl_enums::premium::BoostsStatus::BoostsStatus(tl_types::premium::BoostsStatus {
            my_boost: true,
            level: 5,
            current_level_boosts: 20,
            boosts: 24,
            gift_boosts: Some(4),
            next_level_boosts: Some(30),
            premium_audience: Some(tl_enums::StatsPercentValue::StatsPercentValue(
                tl_types::StatsPercentValue {
                    part: 25.0,
                    total: 100.0,
                },
            )),
            boost_url: "https://t.me/boost/channel".to_string(),
            prepaid_giveaways: Some(vec![tl_enums::PrepaidGiveaway::PrepaidStarsGiveaway(
                tl_types::PrepaidStarsGiveaway {
                    id: 77,
                    stars: 2000,
                    quantity: 5,
                    boosts: 8,
                    date: 1_700_000_000,
                },
            )]),
            my_boost_slots: Some(vec![1, 2]),
        });
    let status = boosts_status_from_tl(&status_tl, false);
    assert_eq!(status.overview.level, 5);
    assert_eq!(status.overview.mine, 2);
    assert!((status.overview.premium_member_percentage - 25.0).abs() < 1e-9);
    assert_eq!(status.prepaid_giveaways[0].credits, 2000);

    let list_tl = tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
        count: 1,
        boosts: vec![tl_enums::Boost::Boost(tl_types::Boost {
            gift: true,
            giveaway: false,
            unclaimed: false,
            id: "b1".to_string(),
            user_id: Some(99),
            giveaway_msg_id: None,
            date: 1_700_000_000,
            expires: 1_700_000_000 + 3 * 30 * 86_400,
            used_gift_slug: Some("abc".to_string()),
            multiplier: Some(4),
            stars: None,
        })],
        next_offset: None,
        users: Vec::new(),
    });
    let list = boosts_list_from_tl(&list_tl, PeerId::from_channel(ChannelId(500)), true);
    assert!(list.all_loaded);
    assert_eq!(list.list[0].expires_after_months, 3);
    assert_eq!(
        list.list[0].gift_code_link.as_ref().unwrap().link,
        "https://t.me/giftcode/abc"
    );
    assert_eq!(
        list.token,
        BoostsOffsetToken {
            next: String::new(),
            gifts: true
        }
    );

    let my_tl = tl_enums::premium::MyBoosts::MyBoosts(tl_types::premium::MyBoosts {
        my_boosts: vec![tl_enums::MyBoost::MyBoost(tl_types::MyBoost {
            slot: 1,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 500,
            })),
            date: 1_700_000_000,
            expires: 1_710_000_000,
            cooldown_until_date: Some(1_700_086_400),
        })],
        chats: Vec::new(),
        users: Vec::new(),
    });
    let slots = my_boosts_from_tl(&my_tl);
    assert!(!slots[0].can_reassign(1_700_000_100));
    assert!(slots[0].can_reassign(1_700_086_400));
}
