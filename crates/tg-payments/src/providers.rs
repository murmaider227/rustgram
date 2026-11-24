//! Stripe and SmartGlocal native card tokenization request builders and response parsers.
//!
//! Port of `payments/stripe/*` (`stripe_api_client.cpp`, `stripe_card.cpp`, `stripe_card_params.cpp`,
//! `stripe_error.cpp`, `stripe_form_encoder.cpp`, `stripe_token.cpp`) and
//! `payments/smartglocal/*` (`smartglocal_api_client.cpp`, `smartglocal_card.cpp`,
//! `smartglocal_error.cpp`, `smartglocal_token.cpp`).
//!
//! Network I/O is abstracted behind [`HttpClient`] so callers can plug in any HTTP client
//! and tests never make real network requests.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use url::Url;

use crate::card::{CardBrand, CardFundingType};

/// Stripe API base URL (`APIURLBase` in `stripe_api_client.cpp`).
pub const STRIPE_API_URL_BASE: &str = "https://api.stripe.com/v1";
/// Stripe token creation endpoint (`TokenEndpoint`).
pub const STRIPE_TOKEN_ENDPOINT: &str = "tokens";
/// Stripe API version header (`StripeAPIVersion`).
pub const STRIPE_API_VERSION: &str = "2015-10-12";
/// Stripe SDK version string (`SDKVersion`).
pub const STRIPE_SDK_VERSION: &str = "9.1.0";

/// SmartGlocal production API base URL.
pub const SMARTGLOCAL_LIVE_API_URL_BASE: &str = "tgb.smart-glocal.com/cds/v1";
/// SmartGlocal test/playground API base URL.
pub const SMARTGLOCAL_TEST_API_URL_BASE: &str = "tgb-playground.smart-glocal.com/cds/v1";
/// SmartGlocal tokenization endpoint path suffix.
pub const SMARTGLOCAL_TOKEN_ENDPOINT: &str = "tokenize/card";
/// Allowed SmartGlocal host suffix for custom `tokenize_url`.
pub const SMARTGLOCAL_HOST_SUFFIX: &str = ".smart-glocal.com";
/// Required SmartGlocal path for custom `tokenize_url`.
pub const SMARTGLOCAL_REQUIRED_PATH: &str = "/cds/v1/tokenize/card";

/// HTTP method for tokenization requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// HTTP `POST`.
    Post,
}

/// Outgoing HTTP request produced by provider tokenization builders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: HttpMethod,
    /// Full target URL.
    pub url: String,
    /// HTTP headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Raw request body bytes.
    pub body: Vec<u8>,
}

/// HTTP response returned by an [`HttpClient`] implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code (e.g., `200`, `400`, `402`).
    pub status_code: u16,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

/// Transport-level error from [`HttpClient`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HTTP transport error (code {code}): {message}")]
pub struct HttpTransportError {
    /// Numeric error code (platform or client defined).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
}

/// Synchronous HTTP transport trait for Stripe / SmartGlocal tokenization.
pub trait HttpClient {
    /// Executes an HTTP request and returns its raw response or a transport error.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpTransportError>;
}

/// Card parameters passed to Stripe or SmartGlocal tokenization (`Stripe::CardParams`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CardParams {
    /// Card primary account number (digits).
    pub number: String,
    /// Expiration month (`1..=12`).
    pub exp_month: u32,
    /// Expiration year (4-digit, e.g., `2028`).
    pub exp_year: u32,
    /// Security code (CVC/CVV).
    pub cvc: String,
    /// Cardholder name (optional).
    pub name: String,
    /// Billing address line 1 (optional).
    pub address_line1: String,
    /// Billing address line 2 (optional).
    pub address_line2: String,
    /// Billing city (optional).
    pub address_city: String,
    /// Billing state (optional).
    pub address_state: String,
    /// Billing postal/ZIP code (optional).
    pub address_zip: String,
    /// Billing ISO2 country code (optional).
    pub address_country: String,
    /// Currency code (optional).
    pub currency: String,
}

impl CardParams {
    /// Returns the form field map matching `Stripe::CardParams::formFieldValues`.
    pub fn form_field_values(&self) -> BTreeMap<&'static str, String> {
        let mut map = BTreeMap::new();
        map.insert("number", self.number.clone());
        map.insert("cvc", self.cvc.clone());
        map.insert("name", self.name.clone());
        map.insert("address_line1", self.address_line1.clone());
        map.insert("address_line2", self.address_line2.clone());
        map.insert("address_city", self.address_city.clone());
        map.insert("address_state", self.address_state.clone());
        map.insert("address_zip", self.address_zip.clone());
        map.insert("address_country", self.address_country.clone());
        map.insert("exp_month", self.exp_month.to_string());
        map.insert("exp_year", self.exp_year.to_string());
        map.insert("currency", self.currency.clone());
        map
    }
}

/// Percent-encodes UTF-8 bytes matching Qt's `QUrl::toPercentEncoding` (unreserved RFC 3986 chars
/// `A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, `~` are preserved; everything else is `%XX` uppercase).
pub fn percent_encode_rfc3986(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Form-encodes an object's key-value pairs matching `Stripe::FormEncoder::formEncodedDataForObject`.
///
/// Empty values are omitted, keys are sorted lexicographically, and keys are wrapped as
/// `root[key]` when `root` is non-empty.
pub fn stripe_form_encode(root: &str, values: &BTreeMap<&str, String>) -> Vec<u8> {
    let mut out = String::new();
    for (key, value) in values {
        if value.is_empty() {
            continue;
        }
        let full_key = if root.is_empty() {
            (*key).to_string()
        } else {
            format!("{root}[{key}]")
        };
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&percent_encode_rfc3986(&full_key));
        out.push('=');
        out.push_str(&percent_encode_rfc3986(value));
    }
    out.into_bytes()
}

// ============================================================================
// Stripe
// ============================================================================

/// Configuration for [`StripeClient`] (`Stripe::PaymentConfiguration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeConfig {
    /// Publishable API key (`pk_live_...` or `pk_test_...`).
    pub publishable_key: String,
    /// Company name (`"Telegram"`).
    pub company_name: String,
}

/// Decoded card metadata inside a Stripe token (`Stripe::Card`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StripeCard {
    /// Stripe card ID (`card_...`).
    pub card_id: String,
    /// Cardholder name.
    pub name: String,
    /// Last 4 digits of the card number.
    pub last4: String,
    /// Detected card brand.
    pub brand: CardBrand,
    /// Card funding type.
    pub funding: CardFundingType,
    /// Stripe card fingerprint.
    pub fingerprint: String,
    /// Card issuing country ISO2.
    pub country: String,
    /// Card currency.
    pub currency: String,
    /// Expiration month.
    pub exp_month: u32,
    /// Expiration year.
    pub exp_year: u32,
    /// Billing address line 1.
    pub address_line1: String,
    /// Billing address line 2.
    pub address_line2: String,
    /// Billing city.
    pub address_city: String,
    /// Billing state.
    pub address_state: String,
    /// Billing postal/ZIP code.
    pub address_zip: String,
    /// Billing country.
    pub address_country: String,
}

impl StripeCard {
    /// Formats the saved-credentials card title matching `CardTitle(const Stripe::Card&)`.
    pub fn title(&self) -> String {
        format!("{} *{}", self.brand.as_str().to_lowercase(), self.last4)
    }

    /// Decodes a [`StripeCard`] from a Stripe JSON `card` object.
    pub fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        for req in ["id", "last4", "brand", "exp_month", "exp_year"] {
            if !obj.contains_key(req) {
                return None;
            }
        }
        let get_str = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let card_id = get_str("id");
        if card_id.is_empty() {
            return None;
        }
        Some(Self {
            card_id,
            last4: get_str("last4"),
            brand: CardBrand::from_stripe_str(&get_str("brand")),
            exp_month: obj.get("exp_month").and_then(Value::as_u64).unwrap_or(0) as u32,
            exp_year: obj.get("exp_year").and_then(Value::as_u64).unwrap_or(0) as u32,
            name: get_str("name"),
            funding: CardFundingType::from_stripe_str(&get_str("funding")),
            fingerprint: get_str("fingerprint"),
            country: get_str("country"),
            currency: get_str("currency"),
            address_line1: get_str("address_line1"),
            address_line2: get_str("address_line2"),
            address_city: get_str("address_city"),
            address_state: get_str("address_state"),
            address_zip: get_str("address_zip"),
            address_country: get_str("address_country"),
        })
    }
}

/// Decoded Stripe tokenization response (`Stripe::Token`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeToken {
    /// Token ID (`tok_...`).
    pub token_id: String,
    /// `true` if created in live mode, `false` for test mode.
    pub livemode: bool,
    /// Creation Unix timestamp in seconds.
    pub created: i64,
    /// Associated card details, if present.
    pub card: Option<StripeCard>,
}

impl StripeToken {
    /// Decodes a [`StripeToken`] from a Stripe JSON response object.
    pub fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        for req in ["id", "livemode", "created"] {
            if !obj.contains_key(req) {
                return None;
            }
        }
        let token_id = obj.get("id")?.as_str()?.to_string();
        if token_id.is_empty() {
            return None;
        }
        let livemode = obj
            .get("livemode")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let created = obj
            .get("created")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0);
        let card = obj
            .get("card")
            .and_then(Value::as_object)
            .and_then(StripeCard::from_json);
        Some(Self {
            token_id,
            livemode,
            created,
            card,
        })
    }

    /// Formats the compact Telegram payment credentials JSON (`{"id":"tok_...","type":"card"}`).
    pub fn to_credentials_json(&self) -> Vec<u8> {
        let payload = json!({
            "type": "card",
            "id": self.token_id,
        });
        serde_json::to_vec(&payload).unwrap_or_default()
    }
}

/// Stripe error category (`Stripe::Error::Code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripeErrorCode {
    /// Network/transport failure.
    Network,
    /// Invalid JSON syntax in response.
    JsonParse,
    /// Valid JSON, but unexpected schema/root type.
    JsonFormat,
    /// Stripe `api_error`.
    Api,
    /// Stripe `card_error`.
    Card,
    /// Stripe `invalid_request_error`.
    InvalidRequest,
    /// Other Stripe error type.
    Unknown,
}

/// Stripe tokenization error (`Stripe::Error`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Stripe error ({code:?}) {description}: {message}")]
pub struct StripeError {
    /// Error category.
    pub code: StripeErrorCode,
    /// Short error identifier (e.g., `"InvalidNumber"`, `"CardDeclined"`, `"GenericError"`).
    pub description: String,
    /// Human-readable message from Stripe or parser.
    pub message: String,
    /// Camel-cased parameter name (`param`), if any.
    pub parameter: String,
}

impl StripeError {
    /// Parses a Stripe `{"error": {...}}` object if present (`Error::DecodedObjectFromResponse`).
    pub fn from_json(root: &serde_json::Map<String, Value>) -> Option<Self> {
        let error = root.get("error")?.as_object()?;
        let get_str = |k: &str| error.get(k).and_then(Value::as_str).unwrap_or("");
        let err_type = get_str("type");
        let message = get_str("message");
        let param_snake = get_str("param");

        if message.is_empty() || err_type.is_empty() {
            return Some(Self {
                code: StripeErrorCode::Api,
                description: "GenericError".to_string(),
                message: "Could not interpret the error response that was returned from Stripe."
                    .to_string(),
                parameter: String::new(),
            });
        }

        let parameter = snake_to_camel(param_snake);
        match err_type {
            "api_error" => Some(Self {
                code: StripeErrorCode::Api,
                description: "GenericError".to_string(),
                message: message.to_string(),
                parameter,
            }),
            "invalid_request_error" => Some(Self {
                code: StripeErrorCode::InvalidRequest,
                description: "GenericError".to_string(),
                message: message.to_string(),
                parameter,
            }),
            "card_error" => {
                let code = get_str("code");
                let description = match code {
                    "incorrect_number" => "IncorrectNumber",
                    "invalid_number" => "InvalidNumber",
                    "invalid_expiry_month" => "InvalidExpiryMonth",
                    "invalid_expiry_year" => "InvalidExpiryYear",
                    "invalid_cvc" => "InvalidCVC",
                    "expired_card" => "ExpiredCard",
                    "incorrect_cvc" => "IncorrectCVC",
                    "card_declined" => "CardDeclined",
                    "processing_error" => "ProcessingError",
                    other => other,
                };
                Some(Self {
                    code: StripeErrorCode::Card,
                    description: description.to_string(),
                    message: message.to_string(),
                    parameter,
                })
            }
            other => Some(Self {
                code: StripeErrorCode::Unknown,
                description: other.to_string(),
                message: message.to_string(),
                parameter,
            }),
        }
    }
}

fn snake_to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    for (idx, word) in snake.split('_').filter(|w| !w.is_empty()).enumerate() {
        if idx == 0 {
            out.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                for ch in first.to_uppercase() {
                    out.push(ch);
                }
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

/// Builds the HTTP request for `Stripe::APIClient::createTokenWithCard`.
pub fn build_stripe_token_request(config: &StripeConfig, card: &CardParams) -> HttpRequest {
    let user_agent = json!({
        "lang": "objective-c",
        "bindings_version": STRIPE_SDK_VERSION,
    })
    .to_string();
    let body = stripe_form_encode("card", &card.form_field_values());
    HttpRequest {
        method: HttpMethod::Post,
        url: format!("{STRIPE_API_URL_BASE}/{STRIPE_TOKEN_ENDPOINT}"),
        headers: vec![
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("X-Stripe-User-Agent".to_string(), user_agent),
            ("Stripe-Version".to_string(), STRIPE_API_VERSION.to_string()),
            (
                "Authorization".to_string(),
                format!("Bearer {}", config.publishable_key),
            ),
        ],
        body,
    }
}

/// Parses a raw HTTP response (or transport error) from Stripe's `/v1/tokens` endpoint.
pub fn parse_stripe_token_response(
    response: Result<HttpResponse, HttpTransportError>,
) -> Result<StripeToken, StripeError> {
    let response = response.map_err(|err| StripeError {
        code: StripeErrorCode::Network,
        description: format!("RequestError{}", err.code),
        message: err.message,
        parameter: String::new(),
    })?;

    let mut parsed_obj: Option<serde_json::Map<String, Value>> = None;
    if !response.body.is_empty() {
        let value: Value = serde_json::from_slice(&response.body).map_err(|err| StripeError {
            code: StripeErrorCode::JsonParse,
            description: "InvalidJson".to_string(),
            message: err.to_string(),
            parameter: String::new(),
        })?;
        let obj = value.as_object().ok_or_else(|| StripeError {
            code: StripeErrorCode::JsonFormat,
            description: "InvalidJsonRoot".to_string(),
            message: "Not an object in JSON reply.".to_string(),
            parameter: String::new(),
        })?;
        if let Some(api_err) = StripeError::from_json(obj) {
            return Err(api_err);
        }
        parsed_obj = Some(obj.clone());
    }

    if !(200..300).contains(&response.status_code) {
        return Err(StripeError {
            code: StripeErrorCode::Network,
            description: format!("RequestError{}", response.status_code),
            message: format!("HTTP status {}", response.status_code),
            parameter: String::new(),
        });
    }

    let obj = parsed_obj.ok_or_else(|| StripeError {
        code: StripeErrorCode::JsonFormat,
        description: "InvalidTokenJson".to_string(),
        message: "Could not parse token.".to_string(),
        parameter: String::new(),
    })?;

    StripeToken::from_json(&obj).ok_or_else(|| StripeError {
        code: StripeErrorCode::JsonFormat,
        description: "InvalidTokenJson".to_string(),
        message: "Could not parse token.".to_string(),
        parameter: String::new(),
    })
}

/// Stripe tokenization client backed by an abstract [`HttpClient`].
pub struct StripeClient<H> {
    config: StripeConfig,
    http: H,
}

impl<H: HttpClient> StripeClient<H> {
    /// Creates a new [`StripeClient`].
    pub fn new(config: StripeConfig, http: H) -> Self {
        Self { config, http }
    }

    /// Tokenizes a payment card through the injected [`HttpClient`].
    pub fn create_token_with_card(&self, card: &CardParams) -> Result<StripeToken, StripeError> {
        let request = build_stripe_token_request(&self.config, card);
        parse_stripe_token_response(self.http.send(&request))
    }
}

// ============================================================================
// SmartGlocal
// ============================================================================

/// Configuration for [`SmartGlocalClient`] (`SmartGlocal::PaymentConfiguration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartGlocalConfig {
    /// Public API token sent in `X-PUBLIC-TOKEN`.
    pub public_token: String,
    /// Optional custom tokenization URL from Telegram `native_params`.
    pub tokenize_url: String,
    /// Whether this invoice is a test invoice (selects playground endpoint when `tokenize_url` is absent/invalid).
    pub is_test: bool,
}

/// Validates `config.tokenize_url` or falls back to the built-in SmartGlocal endpoint
/// (exact port of `ComputeApiUrl` in `smartglocal_api_client.cpp`).
pub fn compute_smartglocal_api_url(config: &SmartGlocalConfig) -> String {
    if !config.tokenize_url.is_empty()
        && let Ok(parsed) = Url::parse(&config.tokenize_url)
    {
        let host = parsed.host_str().unwrap_or("");
        let has_userinfo = !parsed.username().is_empty() || parsed.password().is_some();
        let has_port = parsed.port().is_some();
        if parsed.scheme() == "https"
            && host.len() > SMARTGLOCAL_HOST_SUFFIX.len()
            && host.ends_with(SMARTGLOCAL_HOST_SUFFIX)
            && !has_userinfo
            && !has_port
            && parsed.path() == SMARTGLOCAL_REQUIRED_PATH
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.as_str() == config.tokenize_url
        {
            return config.tokenize_url.clone();
        }
    }
    let base = if config.is_test {
        SMARTGLOCAL_TEST_API_URL_BASE
    } else {
        SMARTGLOCAL_LIVE_API_URL_BASE
    };
    format!("https://{base}/{SMARTGLOCAL_TOKEN_ENDPOINT}")
}

/// Decoded card metadata inside a SmartGlocal token (`SmartGlocal::Card`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmartGlocalCard {
    /// Card type string (e.g., `"VISA"`).
    pub card_type: String,
    /// Card network string.
    pub network: String,
    /// Masked card number (e.g., `"424242******4242"`).
    pub masked_number: String,
}

impl SmartGlocalCard {
    /// Decodes a [`SmartGlocalCard`] from SmartGlocal's `data.info` JSON object.
    pub fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        let get_str = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let card_type = get_str("card_type");
        let masked_number = get_str("masked_card_number");
        if card_type.is_empty() || masked_number.is_empty() {
            return None;
        }
        Some(Self {
            card_type,
            network: get_str("card_network"),
            masked_number,
        })
    }

    /// Extracts the last 4 digits from `masked_number` (`SmartGlocal::Last4`, matching `[^\d]\d*(\d{4})$`).
    pub fn last4(&self) -> &str {
        smartglocal_last4(&self.masked_number)
    }

    /// Formats the saved-credentials card title matching `CardTitle(const SmartGlocal::Card&)`.
    pub fn title(&self) -> String {
        format!("{} *{}", self.card_type.to_lowercase(), self.last4())
    }
}

/// Extracts the trailing 4 digits preceded by a non-digit (and optional digits), matching
/// `[^\d]\d*(\d{4})$` in `smartglocal_card.cpp`.
pub fn smartglocal_last4(masked: &str) -> &str {
    let bytes = masked.as_bytes();
    if bytes.len() < 5 {
        return "";
    }
    // Count trailing ASCII digits.
    let mut digit_count = 0usize;
    for &b in bytes.iter().rev() {
        if b.is_ascii_digit() {
            digit_count += 1;
        } else {
            break;
        }
    }
    // Must have at least 4 trailing digits AND at least one non-digit before them.
    if digit_count >= 4 && digit_count < bytes.len() {
        &masked[bytes.len() - 4..]
    } else {
        ""
    }
}

/// Decoded SmartGlocal tokenization response (`SmartGlocal::Token`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartGlocalToken {
    /// Card token string.
    pub token_id: String,
    /// Card info returned by SmartGlocal.
    pub card: Option<SmartGlocalCard>,
}

impl SmartGlocalToken {
    /// Decodes a [`SmartGlocalToken`] from SmartGlocal's `data` JSON object.
    pub fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        let token_id = obj.get("token")?.as_str()?.to_string();
        if token_id.is_empty() {
            return None;
        }
        let card = obj
            .get("info")
            .and_then(Value::as_object)
            .and_then(SmartGlocalCard::from_json);
        Some(Self { token_id, card })
    }

    /// Formats the compact Telegram payment credentials JSON (`{"token":"...","type":"card"}`).
    pub fn to_credentials_json(&self) -> Vec<u8> {
        let payload = json!({
            "token": self.token_id,
            "type": "card",
        });
        serde_json::to_vec(&payload).unwrap_or_default()
    }
}

/// SmartGlocal error category (`SmartGlocal::Error::Code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartGlocalErrorCode {
    /// Network/transport failure.
    Network,
    /// Invalid JSON syntax in response.
    JsonParse,
    /// Valid JSON, but unexpected schema/root type.
    JsonFormat,
    /// API error returned by SmartGlocal.
    Unknown,
}

/// SmartGlocal tokenization error (`SmartGlocal::Error`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("SmartGlocal error ({code:?}) {description}: {message}")]
pub struct SmartGlocalError {
    /// Error category.
    pub code: SmartGlocalErrorCode,
    /// Short error code/description.
    pub description: String,
    /// Detailed error message.
    pub message: String,
}

impl SmartGlocalError {
    /// Parses a SmartGlocal response for error status (`Error::DecodedObjectFromResponse`).
    pub fn from_json(root: &serde_json::Map<String, Value>) -> Option<Self> {
        if root.get("status").and_then(Value::as_str) == Some("ok") {
            return None;
        }
        let Some(error) = root.get("error").and_then(Value::as_object) else {
            return Some(Self {
                code: SmartGlocalErrorCode::Unknown,
                description: "GenericError".to_string(),
                message: "Could not read the error response that was returned from SmartGlocal."
                    .to_string(),
            });
        };
        let code = error.get("code").and_then(Value::as_str).unwrap_or("");
        let description = error
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        if code.is_empty() || description.is_empty() {
            return Some(Self {
                code: SmartGlocalErrorCode::Unknown,
                description: "GenericError".to_string(),
                message:
                    "Could not interpret the error response that was returned from SmartGlocal."
                        .to_string(),
            });
        }
        Some(Self {
            code: SmartGlocalErrorCode::Unknown,
            description: code.to_string(),
            message: description.to_string(),
        })
    }
}

/// Builds the HTTP request for `SmartGlocal::APIClient::createTokenWithCard`.
pub fn build_smartglocal_token_request(
    config: &SmartGlocalConfig,
    card: &CardParams,
) -> HttpRequest {
    let url = compute_smartglocal_api_url(config);
    let month = format!("{:02}", card.exp_month);
    let year = format!("{:02}", card.exp_year % 100);
    let payload = json!({
        "card": {
            "number": card.number,
            "expiration_month": month,
            "expiration_year": year,
            "security_code": card.cvc,
        }
    });
    let body = serde_json::to_vec(&payload).unwrap_or_default();
    HttpRequest {
        method: HttpMethod::Post,
        url,
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-PUBLIC-TOKEN".to_string(), config.public_token.clone()),
        ],
        body,
    }
}

/// Parses a raw HTTP response (or transport error) from SmartGlocal's tokenize endpoint.
pub fn parse_smartglocal_token_response(
    response: Result<HttpResponse, HttpTransportError>,
) -> Result<SmartGlocalToken, SmartGlocalError> {
    let response = response.map_err(|err| SmartGlocalError {
        code: SmartGlocalErrorCode::Network,
        description: format!("RequestError{}", err.code),
        message: err.message,
    })?;

    let mut parsed_obj: Option<serde_json::Map<String, Value>> = None;
    if !response.body.is_empty() {
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|err| SmartGlocalError {
                code: SmartGlocalErrorCode::JsonParse,
                description: "InvalidJson".to_string(),
                message: err.to_string(),
            })?;
        let obj = value.as_object().ok_or_else(|| SmartGlocalError {
            code: SmartGlocalErrorCode::JsonFormat,
            description: "InvalidJsonRoot".to_string(),
            message: "Not an object in JSON reply.".to_string(),
        })?;
        if let Some(api_err) = SmartGlocalError::from_json(obj) {
            return Err(api_err);
        }
        parsed_obj = Some(obj.clone());
    }

    if !(200..300).contains(&response.status_code) {
        return Err(SmartGlocalError {
            code: SmartGlocalErrorCode::Network,
            description: format!("RequestError{}", response.status_code),
            message: format!("HTTP status {}", response.status_code),
        });
    }

    let data_obj = parsed_obj
        .as_ref()
        .and_then(|o| o.get("data"))
        .and_then(Value::as_object)
        .ok_or_else(|| SmartGlocalError {
            code: SmartGlocalErrorCode::JsonFormat,
            description: "InvalidTokenJson".to_string(),
            message: "Could not parse token.".to_string(),
        })?;

    SmartGlocalToken::from_json(data_obj).ok_or_else(|| SmartGlocalError {
        code: SmartGlocalErrorCode::JsonFormat,
        description: "InvalidTokenJson".to_string(),
        message: "Could not parse token.".to_string(),
    })
}

/// SmartGlocal tokenization client backed by an abstract [`HttpClient`].
pub struct SmartGlocalClient<H> {
    config: SmartGlocalConfig,
    http: H,
}

impl<H: HttpClient> SmartGlocalClient<H> {
    /// Creates a new [`SmartGlocalClient`].
    pub fn new(config: SmartGlocalConfig, http: H) -> Self {
        Self { config, http }
    }

    /// Tokenizes a payment card through the injected [`HttpClient`].
    pub fn create_token_with_card(
        &self,
        card: &CardParams,
    ) -> Result<SmartGlocalToken, SmartGlocalError> {
        let request = build_smartglocal_token_request(&self.config, card);
        parse_smartglocal_token_response(self.http.send(&request))
    }
}
