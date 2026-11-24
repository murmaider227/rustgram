//! Live [`PaymentsBackend`] and [`PassportBackend`] implementations for [`LiveSession`]
//! wired to real Telegram TL RPCs via `tg-payments`, `tg-passport`, and `tg-crypto::srp`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/payments/payments_form.cpp`
//! - `Telegram/SourceFiles/payments/payments_checkout_process.cpp`
//! - `Telegram/SourceFiles/payments/stripe/stripe_api_client.cpp`
//! - `Telegram/SourceFiles/payments/smartglocal/smartglocal_api_client.cpp`
//! - `Telegram/SourceFiles/api/api_credits.cpp`
//! - `Telegram/SourceFiles/api/api_premium.cpp`
//! - `Telegram/SourceFiles/passport/passport_form_controller.cpp`
//!
//! Safety: card tokenization HTTP calls always go through `tg_payments::providers::HttpClient`;
//! tests inject a fake `HttpClient` and never contact real payment providers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_crypto::srp::{
    compute_check, compute_digest, compute_secure_secret_hash, count_secure_secret_id,
    encrypt_secure_secret, generate_secret_bytes, validate_new_algo,
    validate_new_secure_secret_algo,
};
use tg_data::{Peer, PeerId as DataPeerId, Session, UserId as DataUserId};
use tg_passport::controller::{
    CallbackOutcome, PassportController, UnlockOutcome, parse_password_kdf_algo,
    parse_secure_secret_algo, secure_secret_algo_to_tl, srp_check_to_tl,
};
use tg_passport::types::{
    FileType, FormRequest, PassportConfig, PassportFile, PassportForm, PassportValue, ValueMap,
    ValueType,
};
use tg_payments::card::{UncheckedCardDetails, YearMonth, remove_non_digits};
use tg_payments::credits::{
    CreditTopupOption, CreditsAmount, CreditsHistoryFilter, CreditsHistoryPeerType, PaidMediaCost,
    PaidReactionConfig, build_change_stars_subscription, build_get_stars_gift_options,
    build_get_stars_status, build_get_stars_transactions, stars_gift_options_from_tl,
    stars_status_from_tl, stars_topup_options_from_tl,
};
use tg_payments::currency::fill_amount_and_currency;
use tg_payments::form::{
    FormLoadEvent, InvoiceTarget, NativePaymentMethod, NewCredentials, PaymentForm,
    PaymentResultOutcome, RequestedInformation, SendPaymentAction, TMP_PASSWORD_PERIOD_SECS,
};
use tg_payments::premium::{
    LevelLimits, PremiumLimits, PremiumSubscriptionOption, boosts_list_from_tl,
    boosts_status_from_tl, build_apply_boost, giveaway_info_from_tl, my_boosts_from_tl,
    premium_promo_from_tl,
};
use tg_payments::providers::{
    CardParams, HttpClient, HttpMethod, HttpRequest, HttpResponse, HttpTransportError,
    SmartGlocalClient, SmartGlocalConfig, StripeClient, StripeConfig,
};
use tg_payments::star_gifts::{
    CanSendGiftResult, SavedStarGift, SavedStarGiftId, build_check_can_send_gift,
    build_convert_star_gift, build_get_star_gifts, build_save_star_gift, build_transfer_star_gift,
    build_upgrade_star_gift, check_can_send_gift_result_from_tl, is_tmp_password_refused_error,
    saved_star_gifts_from_tl, star_gifts_from_tl,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, known_peer_ids, now_unix_i32, peer_display_title, to_data_msg,
    to_data_peer, unix_to_local_datetime,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::payments::backend::{
    BoostsGiveawayBundle, CatalogGiftItem, CheckoutBundle, CreditsOverviewBundle, GiveawayInfoItem,
    PaidMediaPreview, PaidReactionContext, PassportBackend, PassportSessionBundle,
    PassportUnlockResult, PassportVerificationChallenge, PaymentSubmitOutcome, PaymentsBackend,
    PremiumFeatureItem, PremiumInvoicePreview, PremiumPromoBundle, ReceivedGiftAction,
    ReceivedGiftItem, StarGiftsBundle, ValidatedInfoResult,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default timeout for synchronous payments & passport RPC invocations.
const PAYMENTS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Default timeout for HTTPS card tokenization requests (`Stripe` / `SmartGlocal`).
const PAYMENT_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Blocking `reqwest` (rustls) adapter implementing [`tg_payments::providers::HttpClient`].
///
/// Installed lazily as the default tokenization client on [`LiveSession`]; tests inject a
/// fake [`HttpClient`] via [`LiveSession::set_payment_http_client`] (or point a local fake
/// server at [`ReqwestPaymentHttpClient`]) and never contact real payment providers.
pub struct ReqwestPaymentHttpClient {
    client: reqwest::blocking::Client,
}

impl ReqwestPaymentHttpClient {
    /// Creates a new blocking `reqwest`-backed payment HTTP client with bounded timeout.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { client })
    }

    /// Creates a new blocking `reqwest`-backed client using [`PAYMENT_HTTP_TIMEOUT`].
    pub fn with_default_timeout() -> Result<Self, String> {
        Self::new(PAYMENT_HTTP_TIMEOUT)
    }
}

impl HttpClient for ReqwestPaymentHttpClient {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        let mut req = match request.method {
            HttpMethod::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req
            .body(request.body.clone())
            .send()
            .map_err(|e| HttpTransportError {
                code: -1,
                message: e.to_string(),
            })?;
        let status_code = resp.status().as_u16();
        let body = resp
            .bytes()
            .map_err(|e| HttpTransportError {
                code: -2,
                message: e.to_string(),
            })?
            .to_vec();
        Ok(HttpResponse { status_code, body })
    }
}

/// Cached checkout form state for an invoice.
#[derive(Clone)]
struct ActiveCheckoutState {
    invoice_id: String,
    input_invoice: tl_enums::InputInvoice,
    form: PaymentForm,
    is_stars_form: bool,
}

/// Per-session state for Payments & Passport screens.
#[derive(Default)]
struct PaymentsLiveState {
    active_checkout: Option<ActiveCheckoutState>,
    /// Cached temporary 2FA password `(bytes, valid_until)` (`main/main_session.cpp`).
    tmp_password: Option<(Vec<u8>, i32)>,
    http_client: Option<Arc<dyn HttpClient + Send + Sync>>,
    received_gifts: HashMap<u64, SavedStarGift>,
    pending_transfer_recipient: Option<String>,
    boosts_channel_peer: Option<PeerId>,
    giveaway_target: Option<(PeerId, MsgId)>,
    passport_request: Option<FormRequest>,
    passport_controller: Option<PassportController>,
    /// Cached `help.getPassportConfig` country-to-language map (`passport_form_controller.cpp`).
    passport_config: PassportConfig,
    passport_bot_name: String,
    passport_bot_username: String,
    cached_stars_balance: Option<i64>,
    cached_ton_nanos: Option<i64>,
    random_seq: u64,
}

impl PaymentsLiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.random_seq = self.random_seq.wrapping_add(1);
        ((now << 24) ^ self.random_seq ^ 0x5041_5953_9911) as i64
    }
}

struct PaymentsStateEntry {
    owner: Weak<LiveBridgeState>,
    state: PaymentsLiveState,
}

fn payments_state_registry() -> &'static Mutex<HashMap<usize, PaymentsStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, PaymentsStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_payments_state<R>(live: &LiveSession, f: impl FnOnce(&mut PaymentsLiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = payments_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| PaymentsStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: PaymentsLiveState::default(),
    });
    f(&mut entry.state)
}

fn credits_amount_to_nanos(amount: CreditsAmount) -> i64 {
    amount
        .whole()
        .saturating_mul(1_000_000_000)
        .saturating_add(amount.nano())
        .max(0)
}

/// Records the signed-in account's latest Telegram Stars balance into the shared payments cache.
pub(crate) fn record_self_stars_balance(live: &LiveSession, amount: CreditsAmount) {
    with_payments_state(live, |st| {
        st.cached_stars_balance = Some(amount.whole().max(0));
    });
}

/// Records the signed-in account's latest TON balance into the shared payments cache.
pub(crate) fn record_self_ton_balance(live: &LiveSession, amount: CreditsAmount) {
    with_payments_state(live, |st| {
        st.cached_ton_nanos = Some(credits_amount_to_nanos(amount));
    });
}

/// Returns the cached self profile Stars/TON balance if the payments layer has already loaded it.
pub(crate) fn cached_self_profile_balance(
    live: &LiveSession,
) -> Option<tg_app_core::info::backend::ProfileBalanceInfo> {
    with_payments_state(live, |st| {
        if st.cached_stars_balance.is_none() && st.cached_ton_nanos.is_none() {
            return None;
        }
        Some(tg_app_core::info::backend::ProfileBalanceInfo {
            ton_nanos: st.cached_ton_nanos.unwrap_or(0),
            stars_balance: st.cached_stars_balance.unwrap_or(0),
        })
    })
}

// -----------------------------------------------------------------------------
// Internal RPC & Session helpers
// -----------------------------------------------------------------------------

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: tg_tl::RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(PAYMENTS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

/// Parses an invoice string into a TL [`tl_enums::InputInvoice`].
///
/// Supported formats:
/// - `"msg:<peer_id>:<msg_id>"` -> `InputInvoice::Message`
/// - `"https://t.me/$<slug>"` / `"t.me/$<slug>"` / `"$<slug>"` / `"<slug>"` -> `InputInvoice::Slug`
fn parse_input_invoice(
    live: &LiveSession,
    invoice_id: &str,
) -> Result<tl_enums::InputInvoice, String> {
    let trimmed = invoice_id.trim();
    if let Some(rest) = trimmed.strip_prefix("msg:") {
        let mut parts = rest.splitn(2, ':');
        let peer_raw: i64 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("Invalid invoice peer ID in '{invoice_id}'"))?;
        let msg_raw: i32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("Invalid invoice message ID in '{invoice_id}'"))?;
        let peer = read_session(live, |s| input_peer_for(s, DataPeerId(peer_raw)));
        if matches!(peer, tl_enums::InputPeer::Empty) {
            return Err("Invoice peer not found in session.".to_owned());
        }
        return Ok(tl_enums::InputInvoice::Message(
            tl_types::InputInvoiceMessage {
                peer,
                msg_id: msg_raw,
            },
        ));
    }
    let slug = trimmed
        .strip_prefix("https://t.me/$")
        .or_else(|| trimmed.strip_prefix("http://t.me/$"))
        .or_else(|| trimmed.strip_prefix("t.me/$"))
        .or_else(|| trimmed.strip_prefix('$'))
        .unwrap_or(trimmed);
    if slug.is_empty() {
        return Err("Invoice identifier cannot be empty.".to_owned());
    }
    Ok(tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug {
        slug: slug.to_owned(),
    }))
}

fn current_year_month() -> YearMonth {
    let dt = unix_to_local_datetime(now_unix_i32());
    YearMonth::new(
        dt.date.year.max(2025) as u32,
        u32::from(dt.date.month).clamp(1, 12),
    )
}

struct ArcHttpClient(Arc<dyn HttpClient + Send + Sync>);

impl HttpClient for ArcHttpClient {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        self.0.send(request)
    }
}

fn saved_gift_to_input(
    live: &LiveSession,
    saved: &SavedStarGift,
) -> Option<tl_enums::InputSavedStarGift> {
    if let Some(ref unique) = saved.info.unique
        && !unique.slug.is_empty()
        && saved.manage_id.is_none()
    {
        return Some(tl_enums::InputSavedStarGift::Slug(
            tl_types::InputSavedStarGiftSlug {
                slug: unique.slug.clone(),
            },
        ));
    }
    match saved.manage_id? {
        SavedStarGiftId::User(msg_id) => Some(tl_enums::InputSavedStarGift::User(
            tl_types::InputSavedStarGiftUser {
                msg_id: msg_id.as_i32().unwrap_or(0),
            },
        )),
        SavedStarGiftId::Chat { peer_id, saved_id } => {
            let peer = read_session(live, |s| input_peer_for(s, peer_id));
            Some(tl_enums::InputSavedStarGift::Chat(
                tl_types::InputSavedStarGiftChat {
                    peer,
                    saved_id: saved_id as i64,
                },
            ))
        }
        SavedStarGiftId::Slug => saved.info.unique.as_ref().map(|u| {
            tl_enums::InputSavedStarGift::Slug(tl_types::InputSavedStarGiftSlug {
                slug: u.slug.clone(),
            })
        }),
    }
}

fn resolve_peer_by_name(
    live: &LiveSession,
    recipient: &str,
) -> Result<tl_enums::InputPeer, String> {
    let cleaned = recipient.trim().trim_start_matches('@');
    if cleaned.is_empty() {
        return Ok(tl_enums::InputPeer::Self_);
    }
    let local = read_session(live, |s| {
        for pid in known_peer_ids(s) {
            if let Some(peer) = s.peer(pid) {
                let matches_title = peer.name().eq_ignore_ascii_case(cleaned);
                let matches_uname = peer
                    .as_user()
                    .is_some_and(|u| u.usernames.username().eq_ignore_ascii_case(cleaned))
                    || peer
                        .as_channel()
                        .is_some_and(|c| c.usernames.username().eq_ignore_ascii_case(cleaned));
                if matches_title || matches_uname {
                    return Some(input_peer_for(s, pid));
                }
            }
        }
        None
    });
    if let Some(ip) = local
        && !matches!(ip, tl_enums::InputPeer::Empty)
    {
        return Ok(ip);
    }
    let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) = invoke_rpc(
        live,
        tl_fns::contacts::ResolveUsername {
            username: cleaned.to_owned(),
            referer: None,
        },
    )?;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &resolved.users, &resolved.chats);
    });
    let pid = tg_api::convert::peer_id_from_tl(&resolved.peer);
    let ip = read_session(live, |s| input_peer_for(s, pid));
    if matches!(ip, tl_enums::InputPeer::Empty) {
        Err(format!("Could not resolve recipient '{recipient}'."))
    } else {
        Ok(ip)
    }
}

fn artwork_kind_for_gift(index: usize) -> String {
    const KINDS: [&str; 8] = [
        "star", "cake", "gem", "trophy", "rose", "crown", "rocket", "heart",
    ];
    KINDS[index % KINDS.len()].to_owned()
}

// -----------------------------------------------------------------------------
// Additional live helpers exposed on LiveSession for Payments & Passport
// -----------------------------------------------------------------------------

impl LiveSession {
    /// Installs an [`HttpClient`] used for native Stripe / SmartGlocal card tokenization.
    ///
    /// Tests inject a fake [`HttpClient`] so card tokenization request builders and response
    /// parsers are exercised end-to-end without any real network calls.
    pub fn set_payment_http_client(&self, client: Arc<dyn HttpClient + Send + Sync>) {
        with_payments_state(self, |st| {
            st.http_client = Some(client);
        });
    }

    /// Ensures the default `reqwest` (rustls) [`HttpClient`] is installed on this session
    /// when no custom/fake client has been installed yet, and returns a handle to it.
    pub fn ensure_payment_http_client(&self) -> Result<Arc<dyn HttpClient + Send + Sync>, String> {
        if let Some(existing) = with_payments_state(self, |st| st.http_client.clone()) {
            return Ok(existing);
        }
        let created: Arc<dyn HttpClient + Send + Sync> =
            Arc::new(ReqwestPaymentHttpClient::with_default_timeout()?);
        Ok(with_payments_state(self, |st| {
            let slot = st.http_client.get_or_insert_with(|| Arc::clone(&created));
            Arc::clone(slot)
        }))
    }

    /// Sets a pending recipient used when [`ReceivedGiftAction::Transfer`] is triggered via
    /// [`PaymentsBackend::apply_received_gift_action`].
    pub fn set_gift_transfer_recipient(&self, recipient: impl Into<String>) {
        let trimmed = recipient.into().trim().to_owned();
        with_payments_state(self, |st| {
            st.pending_transfer_recipient = (!trimmed.is_empty()).then_some(trimmed);
        });
    }

    /// Requests a Stars or TON revenue withdrawal URL via `payments.getStarsRevenueWithdrawalUrl`
    /// authenticated with the user's 2FA cloud password (SRP).
    ///
    /// - When `ton == true`, withdraws channel TON ad revenue (`amount` is omitted per TL schema).
    /// - When `ton == false`, withdraws Stars revenue (`amount` must be `Some(n)` with `n > 0`).
    pub fn get_stars_revenue_withdrawal_url(
        &self,
        peer_id: PeerId,
        ton: bool,
        amount: Option<i64>,
        password: &str,
    ) -> Result<String, String> {
        if password.is_empty() {
            return Err("Cloud password is required to withdraw revenue.".to_owned());
        }
        if !ton && amount.is_none_or(|a| a <= 0) {
            return Err("A positive Stars withdrawal amount is required.".to_owned());
        }
        let peer = if peer_id.is_zero() {
            tl_enums::InputPeer::Self_
        } else {
            let resolved = read_session(self, |s| input_peer_for(s, to_data_peer(peer_id)));
            if matches!(resolved, tl_enums::InputPeer::Empty) {
                return Err("Revenue peer not found in session.".to_owned());
            }
            resolved
        };

        let tl_enums::account::Password::Password(pwd) =
            invoke_rpc(self, tl_fns::account::GetPassword {})?;
        let current_algo = pwd
            .current_algo
            .as_ref()
            .and_then(parse_password_kdf_algo)
            .ok_or_else(|| "Account has no active 2FA password algorithm.".to_owned())?;
        let srp_b = pwd
            .srp_b
            .as_deref()
            .ok_or_else(|| "Missing SRP challenge B from server.".to_owned())?;
        let srp_id = pwd
            .srp_id
            .ok_or_else(|| "Missing SRP challenge ID from server.".to_owned())?;
        let check = compute_check(&current_algo, srp_b, srp_id, password.as_bytes())
            .map_err(|e| format!("SRP computation failed: {e}"))?;

        let tl_enums::payments::StarsRevenueWithdrawalUrl::StarsRevenueWithdrawalUrl(resp) =
            invoke_rpc(
                self,
                tl_fns::payments::GetStarsRevenueWithdrawalUrl {
                    ton,
                    peer,
                    amount: if ton { None } else { amount },
                    password: srp_check_to_tl(&check),
                },
            )?;
        Ok(resp.url)
    }

    /// Fetches a paid invoice receipt via `payments.getPaymentReceipt` and applies it to a
    /// [`PaymentForm`].
    pub fn load_payment_receipt(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<PaymentForm, String> {
        let input_peer = read_session(self, |s| input_peer_for(s, to_data_peer(peer_id)));
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Receipt peer not found in session.".to_owned());
        }
        let req = PaymentForm::build_get_payment_receipt_request(input_peer, to_data_msg(msg_id));
        let receipt = invoke_rpc(self, req)?;
        let (default_phone, default_country) = read_session(self, |s| {
            let phone = s
                .peer(s.self_peer_id())
                .and_then(|p| p.as_user())
                .map(|u| u.phone.clone())
                .unwrap_or_default();
            (phone, String::new())
        });
        let mut form = PaymentForm::new(default_phone, default_country);
        match form.apply_payment_receipt_tl(&receipt) {
            FormLoadEvent::FormReady => Ok(form),
            FormLoadEvent::CreditsReceiptReady(cr) => {
                form.invoice.cover.title = cr.title;
                form.invoice.cover.description = cr.description;
                form.invoice.currency = "XTR".to_owned();
                form.invoice.receipt.date = cr.date;
                form.invoice.receipt.total_amount = cr.credits.whole();
                form.invoice.receipt.currency = "XTR".to_owned();
                form.invoice.receipt.paid = true;
                Ok(form)
            }
            FormLoadEvent::Error(err) => Err(err.id),
            FormLoadEvent::CreditsPaymentStarted(_) => Ok(form),
        }
    }

    /// Sets the target channel peer inspected by [`PaymentsBackend::load_boosts_giveaway`].
    pub fn set_boosts_channel_peer(&self, peer_id: PeerId) {
        with_payments_state(self, |st| {
            st.boosts_channel_peer = Some(peer_id);
        });
    }

    /// Sets a giveaway message `(peer_id, msg_id)` queried via `payments.getGiveawayInfo`
    /// inside [`PaymentsBackend::load_boosts_giveaway`].
    pub fn set_giveaway_target(&self, peer_id: PeerId, msg_id: MsgId) {
        with_payments_state(self, |st| {
            st.giveaway_target = Some((peer_id, msg_id));
        });
    }

    /// Configures an explicit [`FormRequest`] (bot ID, scope, public key, nonce, callback URL)
    /// for the next [`PassportBackend::load_authorization_form`] call.
    pub fn configure_passport_request(&self, request: FormRequest) {
        with_payments_state(self, |st| {
            st.passport_request = Some(request);
        });
    }

    /// Loads all stored Telegram Passport values via `account.getAllSecureValues`.
    pub fn load_all_secure_values(&self) -> Result<Vec<PassportValue>, String> {
        let raw = invoke_rpc(self, tl_fns::account::GetAllSecureValues {})?;
        Ok(raw.iter().map(PassportValue::from_tl).collect())
    }

    /// Returns the currently cached `help.getPassportConfig` country-to-language config.
    pub fn passport_config(&self) -> PassportConfig {
        with_payments_state(self, |st| st.passport_config.clone())
    }
}

/// Returns the cached temporary 2FA password if it has not yet expired
/// (`Main::Session::validTmpPassword` in `main/main_session.cpp`).
pub(crate) fn cached_valid_tmp_password(live: &LiveSession) -> Option<Vec<u8>> {
    let now = now_unix_i32();
    with_payments_state(live, |st| {
        if let Some((ref bytes, valid_until)) = st.tmp_password {
            if !bytes.is_empty() && (valid_until == 0 || valid_until > now) {
                return Some(bytes.clone());
            }
            st.tmp_password = None;
        }
        None
    })
}

/// Drops any cached temporary 2FA password (`Main::Session::dropTmpPassword` /
/// `boxes/transfer_gift_box.cpp:444-448`).
pub(crate) fn clear_tmp_password(live: &LiveSession) {
    with_payments_state(live, |st| {
        st.tmp_password = None;
    });
}

/// Computes an SRP 2FA check for `password`, calls `account.getTmpPassword(period)`, validates
/// non-empty bytes (`boxes/transfer_gift_box.cpp:389-396`), and caches `(bytes, valid_until)`.
pub(crate) fn obtain_tmp_password(
    live: &LiveSession,
    password: &str,
    period: i32,
) -> Result<Vec<u8>, String> {
    let tl_enums::account::Password::Password(pwd) =
        invoke_rpc(live, tl_fns::account::GetPassword {})?;
    let current_algo = pwd
        .current_algo
        .as_ref()
        .and_then(parse_password_kdf_algo)
        .ok_or_else(|| "Account has no active 2FA password algorithm.".to_owned())?;
    let srp_b = pwd
        .srp_b
        .as_deref()
        .ok_or_else(|| "Missing SRP challenge B from server.".to_owned())?;
    let srp_id = pwd
        .srp_id
        .ok_or_else(|| "Missing SRP challenge ID from server.".to_owned())?;
    let check = compute_check(&current_algo, srp_b, srp_id, password.as_bytes())
        .map_err(|e| format!("SRP computation failed: {e}"))?;
    let tl_enums::account::TmpPassword::TmpPassword(tmp) = invoke_rpc(
        live,
        tl_fns::account::GetTmpPassword {
            password: srp_check_to_tl(&check),
            period,
        },
    )?;
    if tmp.tmp_password.is_empty() {
        return Err("TMP_PASSWORD_EMPTY: account.getTmpPassword returned no bytes".to_owned());
    }
    with_payments_state(live, |st| {
        st.tmp_password = Some((tmp.tmp_password.clone(), tmp.valid_until));
    });
    Ok(tmp.tmp_password)
}

/// Looks up a received Star Gift cached by [`PaymentsBackend::load_star_gifts`].
pub(crate) fn cached_received_gift(live: &LiveSession, gift_id: u64) -> Option<SavedStarGift> {
    with_payments_state(live, |st| st.received_gifts.get(&gift_id).cloned())
}

/// Removes a received Star Gift from the [`PaymentsBackend`] cache (e.g. after delete/withdraw).
pub(crate) fn remove_cached_received_gift(live: &LiveSession, gift_id: u64) {
    with_payments_state(live, |st| {
        st.received_gifts.remove(&gift_id);
    });
}

fn resolve_user_for_stars_gift(
    live: &LiveSession,
    recipient: &str,
) -> Result<(DataUserId, tl_enums::InputUser), String> {
    let cleaned = recipient.trim().trim_start_matches('@');
    let local = read_session(live, |s| {
        for pid in known_peer_ids(s) {
            if let Some(peer) = s.peer(pid)
                && let Some(u) = peer.as_user()
                && (u.usernames.username().eq_ignore_ascii_case(cleaned)
                    || peer.name().eq_ignore_ascii_case(cleaned))
                && let Some(uid) = pid.to_user()
            {
                let iu = input_user_for(s, uid);
                if !matches!(iu, tl_enums::InputUser::Empty) {
                    return Some((uid, iu));
                }
            }
        }
        None
    });
    if let Some(found) = local {
        return Ok(found);
    }
    let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) = invoke_rpc(
        live,
        tl_fns::contacts::ResolveUsername {
            username: cleaned.to_owned(),
            referer: None,
        },
    )?;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &resolved.users, &resolved.chats);
    });
    let pid = tg_api::convert::peer_id_from_tl(&resolved.peer);
    let uid = pid
        .to_user()
        .ok_or_else(|| format!("Recipient '{recipient}' is not a user."))?;
    let iu = read_session(live, |s| input_user_for(s, uid));
    if matches!(iu, tl_enums::InputUser::Empty) {
        Err(format!("Missing access hash for user '{recipient}'."))
    } else {
        Ok((uid, iu))
    }
}

// -----------------------------------------------------------------------------
// PaymentsBackend implementation for LiveSession
// -----------------------------------------------------------------------------

impl PaymentsBackend for LiveSession {
    fn load_checkout_form(&self, invoice_id: &str) -> Result<CheckoutBundle, String> {
        let trimmed = invoice_id.trim();
        // Support `receipt:<peer_id>:<msg_id>` directly for viewing paid receipts.
        if let Some(rest) = trimmed.strip_prefix("receipt:") {
            let mut parts = rest.splitn(2, ':');
            let peer_raw: i64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("Invalid receipt peer ID in '{invoice_id}'"))?;
            let msg_raw: i32 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("Invalid receipt message ID in '{invoice_id}'"))?;
            let form = self.load_payment_receipt(PeerId(peer_raw), MsgId(msg_raw))?;
            let badge = if form.invoice.is_test {
                "TEST".to_owned()
            } else {
                "RECEIPT".to_owned()
            };
            return Ok(CheckoutBundle {
                invoice_id: invoice_id.to_owned(),
                form,
                cover_badge: badge,
                requires_3ds_demo: false,
                verification_url: String::new(),
            });
        }

        let input_invoice = parse_input_invoice(self, invoice_id)?;
        let req = PaymentForm::build_get_payment_form_request(input_invoice.clone(), None);
        let tl_form = invoke_rpc(self, req)?;

        let (default_phone, default_country) = read_session(self, |s| {
            let phone = s
                .peer(s.self_peer_id())
                .and_then(|p| p.as_user())
                .map(|u| u.phone.clone())
                .unwrap_or_default();
            (phone, String::new())
        });
        let mut form = PaymentForm::new(default_phone, default_country);
        let is_stars_form = match form.apply_payment_form_tl(&tl_form) {
            FormLoadEvent::FormReady => false,
            FormLoadEvent::CreditsPaymentStarted(credits_form) => {
                form.details.form_id = credits_form.form_id;
                form.details.bot_id = credits_form.bot_id;
                form.invoice.cover.title = credits_form.title;
                form.invoice.cover.description = credits_form.description;
                form.invoice.currency = credits_form.currency;
                form.invoice.prices = vec![tg_payments::form::LabeledPrice {
                    label: "Telegram Stars".to_owned(),
                    price: credits_form.credits as i64,
                }];
                form.invoice.subscription_period = (credits_form.subscription_period > 0)
                    .then_some(credits_form.subscription_period);
                true
            }
            FormLoadEvent::CreditsReceiptReady(_) => false,
            FormLoadEvent::Error(err) => return Err(err.id),
        };

        let cover_badge = if is_stars_form {
            "STARS".to_owned()
        } else if form.invoice.is_test {
            "TEST".to_owned()
        } else {
            String::new()
        };
        let url = form.details.url.clone();

        with_payments_state(self, |st| {
            st.active_checkout = Some(ActiveCheckoutState {
                invoice_id: invoice_id.to_owned(),
                input_invoice,
                form: form.clone(),
                is_stars_form,
            });
        });

        Ok(CheckoutBundle {
            invoice_id: invoice_id.to_owned(),
            form,
            cover_badge,
            requires_3ds_demo: false,
            verification_url: url,
        })
    }

    fn validate_requested_info(
        &mut self,
        invoice_id: &str,
        info: &RequestedInformation,
    ) -> Result<ValidatedInfoResult, String> {
        let (mut form, input_invoice, is_stars_form) = with_payments_state(self, |st| {
            if let Some(ref active) = st.active_checkout
                && active.invoice_id == invoice_id
            {
                return Some((
                    active.form.clone(),
                    active.input_invoice.clone(),
                    active.is_stars_form,
                ));
            }
            None
        })
        .map(Ok)
        .unwrap_or_else(|| {
            parse_input_invoice(self, invoice_id).map(|inv| (PaymentForm::default(), inv, false))
        })?;

        let req = form
            .build_validate_requested_info_request(info.clone(), input_invoice.clone())
            .map_err(|e| e.id)?;
        let resp = invoke_rpc(self, req)?;
        form.apply_validated_requested_info_tl(&resp);

        let result = ValidatedInfoResult {
            requested_info_id: form.requested_information_id.clone(),
            shipping_options: form.shipping_options.list.clone(),
        };

        with_payments_state(self, |st| {
            st.active_checkout = Some(ActiveCheckoutState {
                invoice_id: invoice_id.to_owned(),
                input_invoice,
                form,
                is_stars_form,
            });
        });
        Ok(result)
    }

    fn tokenize_test_card(
        &self,
        details: &UncheckedCardDetails,
        save_on_server: bool,
    ) -> Result<NewCredentials, String> {
        let form = with_payments_state(self, |st| {
            st.active_checkout.as_ref().map(|a| a.form.clone())
        });
        let form = form.ok_or_else(|| {
            "No active payment form loaded; call load_checkout_form first.".to_owned()
        })?;
        form.validate_card_local(details, current_year_month())
            .map_err(|e| e.id)?;
        let http = self.ensure_payment_http_client()?;

        let card_params = CardParams {
            number: remove_non_digits(&details.number),
            exp_month: details.expire_month,
            exp_year: details.expire_year,
            cvc: details.cvc.trim().to_owned(),
            name: details.cardholder_name.trim().to_owned(),
            address_country: details.address_country.trim().to_owned(),
            address_zip: details.address_zip.trim().to_owned(),
            currency: form.invoice.currency.clone(),
            ..CardParams::default()
        };

        let creds = match &form.payment_method.native {
            NativePaymentMethod::Stripe { publishable_key } => {
                let client = StripeClient::new(
                    StripeConfig {
                        publishable_key: publishable_key.clone(),
                        company_name: "Telegram".to_owned(),
                    },
                    ArcHttpClient(http),
                );
                let token = client
                    .create_token_with_card(&card_params)
                    .map_err(|e| format!("{}: {}", e.description, e.message))?;
                let title = token
                    .card
                    .as_ref()
                    .map(|c| c.title())
                    .unwrap_or_else(|| "card".to_owned());
                let data = String::from_utf8(token.to_credentials_json())
                    .map_err(|e| format!("invalid credentials UTF-8: {e}"))?;
                NewCredentials {
                    title,
                    data,
                    save_on_server,
                }
            }
            NativePaymentMethod::SmartGlocal {
                public_token,
                tokenize_url,
            } => {
                let client = SmartGlocalClient::new(
                    SmartGlocalConfig {
                        public_token: public_token.clone(),
                        tokenize_url: tokenize_url.clone(),
                        is_test: form.invoice.is_test,
                    },
                    ArcHttpClient(http),
                );
                let token = client
                    .create_token_with_card(&card_params)
                    .map_err(|e| format!("{}: {}", e.description, e.message))?;
                let title = token
                    .card
                    .as_ref()
                    .map(|c| c.title())
                    .unwrap_or_else(|| "card".to_owned());
                let data = String::from_utf8(token.to_credentials_json())
                    .map_err(|e| format!("invalid credentials UTF-8: {e}"))?;
                NewCredentials {
                    title,
                    data,
                    save_on_server,
                }
            }
            NativePaymentMethod::None => {
                return Err(
                    "Active payment form does not advertise a native card provider.".to_owned(),
                );
            }
        };

        with_payments_state(self, |st| {
            if let Some(ref mut active) = st.active_checkout {
                let _ = active.form.set_payment_credentials(creds.clone());
            }
        });
        Ok(creds)
    }

    fn verify_tmp_password(&self, password: &str) -> Result<Vec<u8>, String> {
        obtain_tmp_password(self, password, TMP_PASSWORD_PERIOD_SECS)
    }

    fn submit_payment_form(
        &mut self,
        invoice_id: &str,
        form_id: i64,
        shipping_option_id: Option<&str>,
        tip_amount: i64,
        credentials_title: &str,
        _require_3ds: bool,
    ) -> Result<PaymentSubmitOutcome, String> {
        let tmp_password = cached_valid_tmp_password(self);
        let (mut form, input_invoice, is_stars_form, tmp_password) =
            with_payments_state(self, |st| (st.active_checkout.clone(), tmp_password)).pipe(
                |(active, tmp)| -> Result<_, String> {
                    if let Some(a) = active
                        && a.invoice_id == invoice_id
                    {
                        return Ok((a.form, a.input_invoice, a.is_stars_form, tmp));
                    }
                    let inv = parse_input_invoice(self, invoice_id)?;
                    let mut f = PaymentForm::default();
                    f.details.form_id = form_id;
                    Ok((f, inv, false, tmp))
                },
            )?;

        if form_id != 0 {
            form.details.form_id = form_id;
        }
        if let Some(ship_id) = shipping_option_id {
            form.set_shipping_option(ship_id);
        }
        if tip_amount > 0 {
            if form.invoice.tips_max < tip_amount {
                form.invoice.tips_max = tip_amount;
            }
            form.set_tips(tip_amount);
        }

        let payment_result = if is_stars_form || form.invoice.currency == "XTR" {
            invoke_rpc(
                self,
                tl_fns::payments::SendStarsForm {
                    form_id: form.details.form_id,
                    invoice: input_invoice,
                },
            )?
        } else {
            // Select saved credentials if `credentials_title` matches one of them and no new
            // credentials are active.
            if form.payment_method.new_credentials.is_empty()
                && !credentials_title.is_empty()
                && let Some(idx) = form
                    .payment_method
                    .saved_credentials
                    .iter()
                    .position(|c| c.title == credentials_title || c.id == credentials_title)
            {
                form.payment_method.saved_credentials_index = idx;
            }
            form.trust_bot();
            match form
                .build_send_payment_form_request(input_invoice, tmp_password.as_deref())
                .map_err(|e| e.id)?
            {
                SendPaymentAction::Send(req) => match invoke_rpc(self, *req) {
                    Ok(res) => res,
                    Err(err) => {
                        if is_tmp_password_refused_error(&err) {
                            clear_tmp_password(self);
                        }
                        return Err(err);
                    }
                },
                SendPaymentAction::TmpPasswordRequired => {
                    return Err("Temporary 2FA password required for saved card.".to_owned());
                }
                SendPaymentAction::BotTrustRequired { .. } => {
                    return Err("Merchant bot trust confirmation required.".to_owned());
                }
            }
        };

        let provider = form.invoice.provider.clone();
        let total_amount = form.invoice.total_amount(form.shipping_options.selected());
        let currency = form.invoice.currency.clone();

        match PaymentForm::apply_payment_result_tl(payment_result) {
            PaymentResultOutcome::Finished(updates) => {
                let _ = self.apply_updates_to_session(&updates);
                Ok(PaymentSubmitOutcome::Paid {
                    date: now_unix_i32(),
                    total_amount,
                    currency,
                    receipt_id: format!("receipt_{form_id}"),
                })
            }
            PaymentResultOutcome::VerificationNeeded { url } => {
                Ok(PaymentSubmitOutcome::VerificationNeeded { url, provider })
            }
        }
    }

    fn load_credits_overview(&self) -> Result<CreditsOverviewBundle, String> {
        let now = now_unix_i32();
        let self_pid = read_session(self, Session::self_peer_id);

        // 1. Stars status (`payments.getStarsStatus`).
        let status_tl = invoke_rpc(
            self,
            build_get_stars_status(tl_enums::InputPeer::Self_, false),
        )?;
        let tl_enums::payments::StarsStatus::StarsStatus(ref s_inner) = status_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &s_inner.users, &s_inner.chats);
        });
        let status_slice = stars_status_from_tl(&status_tl, self_pid, now);
        record_self_stars_balance(self, status_slice.balance);

        // 2. Transactions page (`payments.getStarsTransactions`) — merges with any inline history.
        let mut history = status_slice.list;
        if let Ok(tx_tl) = invoke_rpc(
            self,
            build_get_stars_transactions(
                tl_enums::InputPeer::Self_,
                CreditsHistoryFilter::default(),
                String::new(),
                50,
                None,
            ),
        ) {
            let tl_enums::payments::StarsStatus::StarsStatus(ref tx_inner) = tx_tl;
            write_session(self, |s| {
                apply_tl_users_and_chats(s, &tx_inner.users, &tx_inner.chats);
            });
            let tx_slice = stars_status_from_tl(&tx_tl, self_pid, now);
            if !tx_slice.list.is_empty() {
                history = tx_slice.list;
            }
        }

        // 3. Optional TON balance (`payments.getStarsStatus { ton: true }`).
        let ton_balance = match invoke_rpc(
            self,
            build_get_stars_status(tl_enums::InputPeer::Self_, true),
        ) {
            Ok(ton_tl) => {
                let bal = stars_status_from_tl(&ton_tl, self_pid, now).balance;
                record_self_ton_balance(self, bal);
                bal
            }
            Err(_) => CreditsAmount::ZERO_TON,
        };

        // 4. Stars top-up options (`payments.getStarsTopupOptions`).
        let topup_options = match invoke_rpc(self, tl_fns::payments::GetStarsTopupOptions {}) {
            Ok(opts) => stars_topup_options_from_tl(&opts),
            Err(err) => {
                debug!(error = %err, "payments.getStarsTopupOptions failed");
                Vec::new()
            }
        };

        let history_peer_names: Vec<(String, String)> = read_session(self, |s| {
            history
                .iter()
                .map(|entry| {
                    let label = match entry.peer_type {
                        CreditsHistoryPeerType::Peer => {
                            let target = if entry.actor_id != DataPeerId::ZERO {
                                entry.actor_id
                            } else {
                                entry.peer_id
                            };
                            if target != DataPeerId::ZERO {
                                peer_display_title(s, target)
                            } else {
                                "Telegram".to_owned()
                            }
                        }
                        CreditsHistoryPeerType::AppStore => "App Store".to_owned(),
                        CreditsHistoryPeerType::PlayMarket => "Google Play".to_owned(),
                        CreditsHistoryPeerType::Fragment => "Fragment".to_owned(),
                        CreditsHistoryPeerType::PremiumBot => "PremiumBot".to_owned(),
                        CreditsHistoryPeerType::Ads => "Telegram Ads".to_owned(),
                        CreditsHistoryPeerType::Api => "Telegram API".to_owned(),
                        CreditsHistoryPeerType::Unsupported => "Unknown".to_owned(),
                    };
                    (entry.id.clone(), label)
                })
                .collect()
        });

        Ok(CreditsOverviewBundle {
            balance: status_slice.balance,
            ton_balance,
            topup_options,
            history,
            history_peer_names,
            subscriptions: status_slice.subscriptions,
            sample_paid_media: PaidMediaPreview {
                media_id: 0,
                author_title: String::new(),
                caption: String::new(),
                photo_count: 0,
                video_count: 0,
                cost: PaidMediaCost::default(),
            },
            sample_paid_reaction: PaidReactionContext {
                post_id: 0,
                channel_title: String::new(),
                post_excerpt: String::new(),
                config: PaidReactionConfig::default(),
                top_reactors: Vec::new(),
            },
        })
    }

    fn topup_stars(&mut self, option: &CreditTopupOption) -> Result<String, String> {
        // Port of `payments/payments_form.cpp:374-383`: when `gift_bare_peer_id != 0`, build
        // `inputStorePaymentStarsGift` with the recipient `InputUser` instead of a self top-up.
        let gift_user = if option.gift_bare_peer_id != 0 {
            let pid = DataPeerId(option.gift_bare_peer_id as i64);
            let uid = pid
                .to_user()
                .ok_or_else(|| "Stars gift recipient must be a user.".to_owned())?;
            let iu = read_session(self, |s| input_user_for(s, uid));
            if matches!(iu, tl_enums::InputUser::Empty) {
                return Err("Stars gift recipient user is not loaded in session.".to_owned());
            }
            Some(iu)
        } else {
            None
        };
        let target = InvoiceTarget::StarsTopup {
            stars: option.credits as i64,
            currency: option.currency.clone(),
            amount: option.amount as i64,
            gift_user,
            spend_purpose_peer: None,
        };
        let invoice = target
            .to_input_invoice()
            .ok_or_else(|| "Failed to build Stars top-up invoice.".to_owned())?;
        let tl_form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(invoice, None),
        )?;
        match tl_form {
            tl_enums::payments::PaymentForm::PaymentForm(f) => {
                Ok(format!("Top-up form #{} ready ({})", f.form_id, f.url))
            }
            tl_enums::payments::PaymentForm::Stars(f) => {
                Ok(format!("Stars form #{} ready", f.form_id))
            }
            tl_enums::payments::PaymentForm::StarGift(f) => {
                Ok(format!("Star gift form #{} ready", f.form_id))
            }
        }
    }

    fn change_stars_subscription(
        &mut self,
        subscription_id: &str,
        canceled: bool,
    ) -> Result<String, String> {
        let cleaned = subscription_id.trim();
        if cleaned.is_empty() {
            return Err("Subscription ID cannot be empty.".to_owned());
        }
        let _ok = invoke_rpc(self, build_change_stars_subscription(cleaned, canceled))?;
        self.bridge_state().notify_ui();
        if canceled {
            Ok("Subscription canceled.".to_owned())
        } else {
            Ok("Subscription renewed.".to_owned())
        }
    }

    fn confirm_paid_media(&mut self, media_id: u64, _stars: i64) -> Result<CreditsAmount, String> {
        let invoice = parse_input_invoice(self, &format!("paid_media_{media_id}"))?;
        let tl_form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(invoice.clone(), None),
        )?;
        let form_id = match &tl_form {
            tl_enums::payments::PaymentForm::Stars(f) => f.form_id,
            tl_enums::payments::PaymentForm::PaymentForm(f) => f.form_id,
            tl_enums::payments::PaymentForm::StarGift(f) => f.form_id,
        };
        let _res = invoke_rpc(self, tl_fns::payments::SendStarsForm { form_id, invoice })?;
        let self_pid = read_session(self, Session::self_peer_id);
        let status = invoke_rpc(
            self,
            build_get_stars_status(tl_enums::InputPeer::Self_, false),
        )?;
        let balance = stars_status_from_tl(&status, self_pid, now_unix_i32()).balance;
        record_self_stars_balance(self, balance);
        self.bridge_state().notify_ui();
        Ok(balance)
    }

    fn confirm_paid_reaction(
        &mut self,
        post_id: i32,
        stars: i32,
        anonymous: bool,
    ) -> Result<CreditsAmount, String> {
        let target_peer = with_payments_state(self, |st| st.boosts_channel_peer)
            .map(|p| read_session(self, |s| input_peer_for(s, to_data_peer(p))))
            .filter(|ip| !matches!(ip, tl_enums::InputPeer::Empty))
            .unwrap_or_else(|| {
                read_session(self, |s| {
                    known_peer_ids(s)
                        .into_iter()
                        .find(|&pid| s.peer(pid).is_some_and(|p| p.as_channel().is_some()))
                        .map(|pid| input_peer_for(s, pid))
                        .unwrap_or(tl_enums::InputPeer::Self_)
                })
            });
        let random_id = with_payments_state(self, PaymentsLiveState::next_random_id);
        let privacy = if anonymous {
            Some(tl_enums::PaidReactionPrivacy::Anonymous)
        } else {
            Some(tl_enums::PaidReactionPrivacy::Default)
        };
        let updates = invoke_rpc(
            self,
            tl_fns::messages::SendPaidReaction {
                peer: target_peer,
                msg_id: post_id,
                count: stars,
                random_id,
                private: privacy,
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        let self_pid = read_session(self, Session::self_peer_id);
        let status = invoke_rpc(
            self,
            build_get_stars_status(tl_enums::InputPeer::Self_, false),
        )?;
        let balance = stars_status_from_tl(&status, self_pid, now_unix_i32()).balance;
        record_self_stars_balance(self, balance);
        self.bridge_state().notify_ui();
        Ok(balance)
    }

    fn load_star_gifts(&self) -> Result<StarGiftsBundle, String> {
        let now = now_unix_i32();
        let self_pid = read_session(self, Session::self_peer_id);

        // 1. Balance via `payments.getStarsStatus`.
        let balance = match invoke_rpc(
            self,
            build_get_stars_status(tl_enums::InputPeer::Self_, false),
        ) {
            Ok(st) => {
                let bal = stars_status_from_tl(&st, self_pid, now).balance;
                record_self_stars_balance(self, bal);
                bal
            }
            Err(_) => CreditsAmount::ZERO_STARS,
        };

        // 2. Catalog via `payments.getStarGifts`.
        let catalog_tl = invoke_rpc(self, build_get_star_gifts(0))?;
        let catalog: Vec<CatalogGiftItem> = star_gifts_from_tl(&catalog_tl)
            .map(|(_, list)| {
                list.into_iter()
                    .enumerate()
                    .map(|(idx, gift)| {
                        let title = if !gift.resell_title.is_empty() {
                            gift.resell_title.clone()
                        } else if let Some(ref u) = gift.unique {
                            u.display_name()
                        } else {
                            format!("Star Gift #{}", gift.id)
                        };
                        CatalogGiftItem {
                            gift,
                            title,
                            artwork_kind: artwork_kind_for_gift(idx),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 3. Received gifts via `payments.getSavedStarGifts`.
        let saved_tl = invoke_rpc(
            self,
            tl_fns::payments::GetSavedStarGifts {
                exclude_unsaved: false,
                exclude_saved: false,
                exclude_unlimited: false,
                exclude_unique: false,
                sort_by_value: false,
                exclude_upgradable: false,
                exclude_unupgradable: false,
                peer_color_available: false,
                exclude_hosted: false,
                peer: tl_enums::InputPeer::Self_,
                collection_id: None,
                offset: String::new(),
                limit: 50,
            },
        )?;
        let tl_enums::payments::SavedStarGifts::SavedStarGifts(ref saved_inner) = saved_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &saved_inner.users, &saved_inner.chats);
        });
        let saved_slice = saved_star_gifts_from_tl(&saved_tl, self_pid, true);

        with_payments_state(self, |st| {
            st.received_gifts.clear();
            for g in &saved_slice.list {
                st.received_gifts.insert(g.info.id, g.clone());
            }
        });

        let received: Vec<ReceivedGiftItem> = read_session(self, |s| {
            saved_slice
                .list
                .into_iter()
                .enumerate()
                .map(|(idx, saved)| {
                    let title = if let Some(ref u) = saved.info.unique {
                        u.display_name()
                    } else if !saved.info.resell_title.is_empty() {
                        saved.info.resell_title.clone()
                    } else {
                        format!("Star Gift #{}", saved.info.id)
                    };
                    let sender_name = if saved.anonymous || saved.from_id == DataPeerId::ZERO {
                        "Anonymous".to_owned()
                    } else {
                        peer_display_title(s, saved.from_id)
                    };
                    ReceivedGiftItem {
                        saved,
                        title,
                        sender_name,
                        artwork_kind: artwork_kind_for_gift(idx),
                    }
                })
                .collect()
        });

        let default_recipient = read_session(self, |s| peer_display_title(s, s.self_peer_id()));
        Ok(StarGiftsBundle {
            balance,
            default_recipient,
            catalog,
            received,
        })
    }

    fn send_star_gift(
        &mut self,
        gift_id: u64,
        recipient: &str,
        message: &str,
        hide_name: bool,
        include_upgrade: bool,
    ) -> Result<String, String> {
        let recipient_peer = resolve_peer_by_name(self, recipient)?;
        let msg_tl = (!message.trim().is_empty()).then(|| {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: message.trim().to_owned(),
                entities: Vec::new(),
            })
        });
        let target = InvoiceTarget::StarGift {
            recipient: recipient_peer,
            gift_id: gift_id as i64,
            message: msg_tl,
            hide_name,
            include_upgrade,
        };
        let invoice = target
            .to_input_invoice()
            .ok_or_else(|| "Failed to build Star Gift invoice.".to_owned())?;
        let tl_form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(invoice.clone(), None),
        )?;
        let form_id = match &tl_form {
            tl_enums::payments::PaymentForm::StarGift(f) => f.form_id,
            tl_enums::payments::PaymentForm::Stars(f) => f.form_id,
            tl_enums::payments::PaymentForm::PaymentForm(f) => f.form_id,
        };
        let _res = invoke_rpc(self, tl_fns::payments::SendStarsForm { form_id, invoice })?;
        self.bridge_state().notify_ui();
        Ok(format!("Sent Star Gift #{gift_id} (form #{form_id})."))
    }

    fn apply_received_gift_action(
        &mut self,
        gift_id: u64,
        action: ReceivedGiftAction,
    ) -> Result<String, String> {
        let saved = with_payments_state(self, |st| st.received_gifts.get(&gift_id).cloned())
            .ok_or_else(|| format!("Received Star Gift #{gift_id} not found."))?;
        let stargift = saved_gift_to_input(self, &saved)
            .ok_or_else(|| format!("Gift #{gift_id} has no manageable ID."))?;

        match action {
            ReceivedGiftAction::ConvertToStars => {
                let _ = invoke_rpc(self, build_convert_star_gift(stargift))?;
                with_payments_state(self, |st| {
                    st.received_gifts.remove(&gift_id);
                });
                Ok(format!("Converted Star Gift #{gift_id} to Stars."))
            }
            ReceivedGiftAction::ToggleVisibility => {
                let unsave = !saved.hidden;
                let _ = invoke_rpc(self, build_save_star_gift(stargift, unsave))?;
                with_payments_state(self, |st| {
                    if let Some(entry) = st.received_gifts.get_mut(&gift_id) {
                        entry.hidden = unsave;
                    }
                });
                Ok(if unsave {
                    format!("Hid Star Gift #{gift_id} from profile.")
                } else {
                    format!("Displayed Star Gift #{gift_id} on profile.")
                })
            }
            ReceivedGiftAction::TogglePin => {
                let _ = invoke_rpc(
                    self,
                    tl_fns::payments::ToggleStarGiftsPinnedToTop {
                        peer: tl_enums::InputPeer::Self_,
                        stargift: vec![stargift],
                    },
                )?;
                with_payments_state(self, |st| {
                    if let Some(entry) = st.received_gifts.get_mut(&gift_id) {
                        entry.pinned = !entry.pinned;
                    }
                });
                Ok(format!("Toggled pin state for Star Gift #{gift_id}."))
            }
            ReceivedGiftAction::Upgrade => {
                let updates = invoke_rpc(self, build_upgrade_star_gift(stargift, true))?;
                let _ = self.apply_updates_to_session(&updates);
                Ok(format!("Upgraded Star Gift #{gift_id}."))
            }
            ReceivedGiftAction::Transfer => {
                let recipient =
                    with_payments_state(self, |st| st.pending_transfer_recipient.take())
                        .ok_or_else(|| {
                            "Select a recipient before transferring a Star Gift.".to_owned()
                        })?;
                self.transfer_received_gift(gift_id, &recipient)
            }
        }
    }

    fn transfer_received_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String> {
        let cleaned = recipient.trim();
        if cleaned.is_empty() {
            return Err("Recipient is required to transfer a Star Gift.".to_owned());
        }
        let saved = with_payments_state(self, |st| st.received_gifts.get(&gift_id).cloned())
            .ok_or_else(|| format!("Received Star Gift #{gift_id} not found."))?;
        let stargift = saved_gift_to_input(self, &saved)
            .ok_or_else(|| format!("Gift #{gift_id} has no manageable ID."))?;
        let to_peer = resolve_peer_by_name(self, cleaned)?;
        if matches!(to_peer, tl_enums::InputPeer::Self_) {
            return Err(
                "Cannot transfer a Star Gift to yourself; select another recipient.".to_owned(),
            );
        }
        let updates = invoke_rpc(self, build_transfer_star_gift(stargift, to_peer))?;
        let _ = self.apply_updates_to_session(&updates);
        with_payments_state(self, |st| {
            st.received_gifts.remove(&gift_id);
        });
        self.bridge_state().notify_ui();
        Ok(format!("Transferred Star Gift #{gift_id} to {cleaned}."))
    }

    fn transfer_recipient_candidates(&self) -> Vec<String> {
        read_session(self, |s| {
            let self_pid = s.self_peer_id();
            let mut out = Vec::new();
            for pid in known_peer_ids(s) {
                if pid == self_pid {
                    continue;
                }
                if let Some(Peer::User(u)) = s.peer(pid)
                    && !u.is_bot()
                    && !u.is_inaccessible()
                {
                    let uname = u.usernames.username();
                    let label = if !uname.is_empty() {
                        format!("@{uname}")
                    } else {
                        u.common.name.clone()
                    };
                    if !label.is_empty() && !out.contains(&label) {
                        out.push(label);
                    }
                }
            }
            out
        })
    }

    fn load_premium_promo(&self) -> Result<PremiumPromoBundle, String> {
        let tl_promo = invoke_rpc(self, tl_fns::help::GetPremiumPromo {})?;
        let tl_enums::help::PremiumPromo::PremiumPromo(ref inner) = tl_promo;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &[]);
        });
        let promo = premium_promo_from_tl(&tl_promo);
        let features: Vec<PremiumFeatureItem> = promo
            .videos
            .keys()
            .enumerate()
            .map(|(i, slug)| {
                let title = slug
                    .split('_')
                    .filter(|p| !p.is_empty())
                    .map(|word| {
                        let mut chars = word.chars();
                        match chars.next() {
                            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                PremiumFeatureItem {
                    slug: slug.clone(),
                    title,
                    description: String::new(),
                    detail: String::new(),
                    icon_rgb: avatar_rgb_for_color_index((i % 7) as u8),
                    icon_symbol: "★".to_owned(),
                    is_new: false,
                }
            })
            .collect();

        Ok(PremiumPromoBundle {
            promo,
            default_limits: PremiumLimits::new(false),
            premium_limits: PremiumLimits::new(true),
            features,
        })
    }

    fn preview_premium_invoice(
        &self,
        option: &PremiumSubscriptionOption,
    ) -> Result<PremiumInvoicePreview, String> {
        let slug = extract_premium_invoice_slug(&option.bot_url, option.months);
        let input_invoice = tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug { slug });
        // Read-only invoice preview: only calls `payments.getPaymentForm`, never submits payment.
        let tl_form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(input_invoice, None),
        )?;
        match tl_form {
            tl_enums::payments::PaymentForm::PaymentForm(f) => {
                let tl_enums::Invoice::Invoice(ref inv) = f.invoice;
                let total_minor: i64 = inv
                    .prices
                    .iter()
                    .map(|tl_enums::LabeledPrice::LabeledPrice(p)| p.amount)
                    .sum();
                let total_label = if total_minor > 0 && !inv.currency.is_empty() {
                    fill_amount_and_currency(total_minor, &inv.currency, false)
                } else {
                    option.total.clone()
                };
                let provider = if !f.native_provider.as_deref().unwrap_or("").is_empty() {
                    f.native_provider.unwrap_or_default()
                } else {
                    "Telegram Payments".to_owned()
                };
                Ok(PremiumInvoicePreview {
                    form_id: f.form_id,
                    title: format!("Telegram Premium ({} months)", option.months),
                    description: format!(
                        "{} total ({}/month). Review details before confirming in checkout.",
                        total_label, option.cost_per_month
                    ),
                    total_label,
                    months: option.months,
                    provider,
                    checkout_url: if f.url.is_empty() {
                        option.bot_url.clone()
                    } else {
                        f.url
                    },
                    is_test: inv.test,
                })
            }
            tl_enums::payments::PaymentForm::Stars(f) => {
                let tl_enums::Invoice::Invoice(ref inv) = f.invoice;
                Ok(PremiumInvoicePreview {
                    form_id: f.form_id,
                    title: if f.title.is_empty() {
                        format!("Telegram Premium ({} months)", option.months)
                    } else {
                        f.title
                    },
                    description: if f.description.is_empty() {
                        format!("{} total ({}/month).", option.total, option.cost_per_month)
                    } else {
                        f.description
                    },
                    total_label: option.total.clone(),
                    months: option.months,
                    provider: "Telegram Stars".to_owned(),
                    checkout_url: option.bot_url.clone(),
                    is_test: inv.test,
                })
            }
            tl_enums::payments::PaymentForm::StarGift(f) => Ok(PremiumInvoicePreview {
                form_id: f.form_id,
                title: format!("Telegram Premium ({} months)", option.months),
                description: format!("{} total ({}/month).", option.total, option.cost_per_month),
                total_label: option.total.clone(),
                months: option.months,
                provider: "Telegram Payments".to_owned(),
                checkout_url: option.bot_url.clone(),
                is_test: false,
            }),
        }
    }

    fn load_boosts_giveaway(&self) -> Result<BoostsGiveawayBundle, String> {
        let configured_channel = with_payments_state(self, |st| st.boosts_channel_peer);
        let (target_pid, channel_title, is_group, input_peer) = read_session(self, |s| {
            let pid = configured_channel
                .map(to_data_peer)
                .filter(|&p| s.peer(p).is_some())
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .find(|&p| s.peer(p).is_some_and(|peer| peer.as_channel().is_some()))
                })
                .unwrap_or_else(|| s.self_peer_id());
            let title = peer_display_title(s, pid);
            let is_grp = s
                .peer(pid)
                .and_then(|p| p.as_channel())
                .is_some_and(|ch| ch.is_megagroup());
            let ip = input_peer_for(s, pid);
            let ip = if matches!(ip, tl_enums::InputPeer::Empty) {
                tl_enums::InputPeer::Self_
            } else {
                ip
            };
            (pid, title, is_grp, ip)
        });

        let status_tl = invoke_rpc(
            self,
            tl_fns::premium::GetBoostsStatus {
                peer: input_peer.clone(),
            },
        )?;
        let boost_status = boosts_status_from_tl(&status_tl, is_group);

        let recent_boosts = match invoke_rpc(
            self,
            tl_fns::premium::GetBoostsList {
                gifts: false,
                peer: input_peer,
                offset: String::new(),
                limit: 50,
            },
        ) {
            Ok(list_tl) => {
                let tl_enums::premium::BoostsList::BoostsList(ref inner) = list_tl;
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &inner.users, &[]);
                });
                let slice = boosts_list_from_tl(&list_tl, target_pid, false);
                read_session(self, |s| {
                    slice
                        .list
                        .into_iter()
                        .map(|b| {
                            let name = if b.user_id != DataUserId::ZERO {
                                peer_display_title(s, DataPeerId::from_user(b.user_id))
                            } else {
                                "Unclaimed".to_owned()
                            };
                            (b, name)
                        })
                        .collect()
                })
            }
            Err(_) => Vec::new(),
        };

        let my_slots = match invoke_rpc(self, tl_fns::premium::GetMyBoosts {}) {
            Ok(my_tl) => {
                let tl_enums::premium::MyBoosts::MyBoosts(ref inner) = my_tl;
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &inner.users, &inner.chats);
                });
                my_boosts_from_tl(&my_tl)
            }
            Err(_) => Vec::new(),
        };

        let mut giveaways = Vec::new();
        let giveaway_target = with_payments_state(self, |st| st.giveaway_target);
        if let Some((gw_peer, gw_msg_id)) = giveaway_target {
            let gw_input = read_session(self, |s| input_peer_for(s, to_data_peer(gw_peer)));
            if !matches!(gw_input, tl_enums::InputPeer::Empty)
                && let Ok(gw_tl) = invoke_rpc(
                    self,
                    tl_fns::payments::GetGiveawayInfo {
                        peer: gw_input,
                        msg_id: gw_msg_id.0,
                    },
                )
            {
                let info = giveaway_info_from_tl(&gw_tl);
                let host_title =
                    read_session(self, |s| peer_display_title(s, to_data_peer(gw_peer)));
                let prize_summary = if info.credits > 0 {
                    format!("{} Telegram Stars", info.credits)
                } else if info.winners_count > 0 {
                    format!("{} Telegram Premium prizes", info.winners_count)
                } else {
                    "Telegram Giveaway".to_owned()
                };
                giveaways.push(GiveawayInfoItem {
                    title: format!("Giveaway #{}", gw_msg_id.0),
                    channel_title: host_title,
                    additional_channels: Vec::new(),
                    prize_summary,
                    prize_description: String::new(),
                    only_new_subscribers: info.too_early_date > 0,
                    countries_summary: if info.disallowed_country.is_empty() {
                        "Worldwide".to_owned()
                    } else {
                        format!("Restricted ({})", info.disallowed_country)
                    },
                    info,
                });
            }
        }

        Ok(BoostsGiveawayBundle {
            channel_title,
            boost_status,
            level_limits: LevelLimits::default(),
            my_slots,
            recent_boosts,
            giveaways,
        })
    }

    fn apply_channel_boost(
        &mut self,
        channel_title: &str,
        slots: &[i32],
    ) -> Result<BoostsGiveawayBundle, String> {
        if slots.is_empty() {
            return Err("Select at least one boost slot to apply.".to_owned());
        }
        let configured_channel = with_payments_state(self, |st| st.boosts_channel_peer);
        let input_peer = read_session(self, |s| {
            let pid = configured_channel
                .map(to_data_peer)
                .filter(|&p| s.peer(p).is_some())
                .or_else(|| {
                    let trimmed = channel_title.trim();
                    known_peer_ids(s).into_iter().find(|&p| {
                        s.peer(p).is_some_and(|peer| {
                            peer.as_channel().is_some()
                                && (trimmed.is_empty()
                                    || peer_display_title(s, p).eq_ignore_ascii_case(trimmed))
                        })
                    })
                })
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .find(|&p| s.peer(p).is_some_and(|peer| peer.as_channel().is_some()))
                })
                .unwrap_or_else(|| s.self_peer_id());
            let ip = input_peer_for(s, pid);
            if matches!(ip, tl_enums::InputPeer::Empty) {
                tl_enums::InputPeer::Self_
            } else {
                ip
            }
        });
        let my_tl = invoke_rpc(self, build_apply_boost(input_peer, Some(slots.to_vec())))?;
        let tl_enums::premium::MyBoosts::MyBoosts(ref inner) = my_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let updated_slots = my_boosts_from_tl(&my_tl);
        self.bridge_state().notify_ui();
        match self.load_boosts_giveaway() {
            Ok(mut bundle) => {
                if !updated_slots.is_empty() {
                    bundle.my_slots = updated_slots;
                }
                Ok(bundle)
            }
            Err(_) => Ok(BoostsGiveawayBundle {
                channel_title: channel_title.to_owned(),
                boost_status: tg_payments::premium::BoostStatus::default(),
                level_limits: LevelLimits::default(),
                my_slots: updated_slots,
                recent_boosts: Vec::new(),
                giveaways: Vec::new(),
            }),
        }
    }

    fn check_can_send_gift(&self, gift_id: u64) -> Result<CanSendGiftResult, String> {
        let tl_res = invoke_rpc(self, build_check_can_send_gift(gift_id))?;
        Ok(check_can_send_gift_result_from_tl(&tl_res))
    }

    fn load_stars_gift_options(&self, recipient: &str) -> Result<Vec<CreditTopupOption>, String> {
        let cleaned = recipient.trim();
        let (gift_peer_id, input_user) = if cleaned.is_empty() {
            (DataPeerId::ZERO, None)
        } else {
            let (uid, iu) = resolve_user_for_stars_gift(self, cleaned)?;
            (DataPeerId::from_user(uid), Some(iu))
        };
        let opts_tl = invoke_rpc(self, build_get_stars_gift_options(input_user))?;
        Ok(stars_gift_options_from_tl(&opts_tl, gift_peer_id))
    }

    fn as_marketplace_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::payments::marketplace_backend::StarGiftsMarketplaceBackend>
    {
        Some(self)
    }
}

// -----------------------------------------------------------------------------
// PassportBackend implementation for LiveSession
// -----------------------------------------------------------------------------

impl PassportBackend for LiveSession {
    fn configure_form_request(&mut self, request: FormRequest) {
        self.configure_passport_request(request);
    }

    fn load_authorization_form(&self, bot_username: &str) -> Result<PassportSessionBundle, String> {
        let configured_req = with_payments_state(self, |st| st.passport_request.clone());

        let (request, bot_name, resolved_username) = if let Some(req) = configured_req {
            let (name, uname) = read_session(self, |s| {
                let pid = DataPeerId::from_user(req.bot_id);
                match s.peer(pid) {
                    Some(Peer::User(u)) => (
                        u.common.name.clone(),
                        if u.usernames.username().is_empty() {
                            bot_username.trim_start_matches('@').to_owned()
                        } else {
                            u.usernames.username().to_owned()
                        },
                    ),
                    _ => (
                        bot_username.trim_start_matches('@').to_owned(),
                        bot_username.trim_start_matches('@').to_owned(),
                    ),
                }
            });
            (req, name, uname)
        } else {
            let cleaned = bot_username.trim().trim_start_matches('@');
            if cleaned.is_empty() {
                return Err("Bot username cannot be empty.".to_owned());
            }
            let (bot_id, bot_name) = resolve_bot_user(self, cleaned)?;
            let req = FormRequest {
                bot_id,
                scope: "id".to_owned(),
                callback_url: String::new(),
                public_key: String::new(),
                nonce: "live_session_nonce".to_owned(),
            };
            (req, bot_name, cleaned.to_owned())
        };

        let mut controller = PassportController::new(request);

        // Port of `passport/passport_form_controller.cpp:1248-1270`: populate country-to-language
        // mapping from cache or `help.getPassportConfig` (non-fatal on failure so form still loads).
        let cached_cfg = with_payments_state(self, |st| st.passport_config.clone());
        if cached_cfg.hash != 0 {
            controller.set_passport_config(cached_cfg);
        } else if let Ok(cfg_tl) = invoke_rpc(self, controller.build_get_passport_config_request())
            && controller.apply_passport_config_tl(&cfg_tl).is_ok()
        {
            let loaded = controller.passport_config().clone();
            with_payments_state(self, |st| {
                st.passport_config = loaded;
            });
        }

        let auth_req = controller
            .build_get_authorization_form_request()
            .map_err(|e| e.to_string())?;
        let auth_form_tl = invoke_rpc(self, auth_req)?;
        let tl_enums::account::AuthorizationForm::AuthorizationForm(ref af_inner) = auth_form_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &af_inner.users, &[]);
        });
        controller
            .apply_authorization_form_tl(&auth_form_tl)
            .map_err(|e| e.to_string())?;

        let pwd_tl = invoke_rpc(self, tl_fns::account::GetPassword {})?;
        controller.apply_password_tl(&pwd_tl);

        let pwd_settings = controller.password_settings().clone();
        let form = controller.form().clone();

        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
            st.passport_bot_name = bot_name.clone();
            st.passport_bot_username = resolved_username.clone();
        });

        Ok(PassportSessionBundle {
            bot_name,
            bot_username: resolved_username,
            has_cloud_password: pwd_settings.challenge.is_some(),
            password_hint: pwd_settings.hint,
            has_recovery: pwd_settings.has_recovery,
            has_secure_values: pwd_settings.not_empty_passport,
            form,
        })
    }

    fn verify_cloud_password(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        let mut controller = with_payments_state(self, |st| st.passport_controller.clone())
            .ok_or_else(|| "No active Passport authorization form.".to_owned())?;

        let (hash_for_auth, req) = controller
            .prepare_password_check(password.as_bytes())
            .map_err(|e| e.to_string())?;
        let pwd_settings_tl = invoke_rpc(self, req)?;
        let outcome = controller
            .apply_password_settings_tl(&pwd_settings_tl, password.as_bytes(), hash_for_auth)
            .map_err(|e| e.to_string())?;

        let notice = match outcome {
            UnlockOutcome::Unlocked { .. } => "Telegram Passport unlocked.".to_owned(),
            UnlockOutcome::NeedGenerateSecret => {
                // Re-fetch fresh SRP challenge for `account.updatePasswordSettings`.
                let fresh_pwd = invoke_rpc(self, tl_fns::account::GetPassword {})?;
                controller.apply_password_tl(&fresh_pwd);
                let challenge = controller
                    .password_settings()
                    .challenge
                    .as_ref()
                    .ok_or_else(|| "Missing SRP challenge while saving new secret.".to_owned())?;
                let srp_check = compute_check(
                    &challenge.algo,
                    &challenge.srp_b,
                    challenge.srp_id,
                    password.as_bytes(),
                )
                .map_err(|e| e.to_string())?;
                let prepared = controller
                    .prepare_new_secret(password.as_bytes(), hash_for_auth, &srp_check)
                    .map_err(|e| e.to_string())?;
                let _ = invoke_rpc(self, prepared.request)?;
                controller.apply_new_secret_saved(prepared.secret, prepared.secret_id);
                "Generated and saved new Telegram Passport secret.".to_owned()
            }
            UnlockOutcome::NeedResetSecret => {
                return Err(
                    "Stored Telegram Passport secret could not be decrypted; reset required."
                        .to_owned(),
                );
            }
            UnlockOutcome::UpdateAppRequired => {
                return Err(
                    "App update required to support this Passport KDF algorithm.".to_owned(),
                );
            }
        };

        let form = controller.form().clone();
        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
        });
        self.bridge_state().notify_ui();
        Ok(PassportUnlockResult { form, notice })
    }

    fn setup_cloud_password(
        &mut self,
        password: &str,
        hint: &str,
    ) -> Result<PassportUnlockResult, String> {
        if password.is_empty() {
            return Err("Password cannot be empty.".to_owned());
        }
        let mut controller = with_payments_state(self, |st| st.passport_controller.clone())
            .ok_or_else(|| "No active Passport authorization form.".to_owned())?;

        let pwd_tl = invoke_rpc(self, tl_fns::account::GetPassword {})?;
        controller.apply_password_tl(&pwd_tl);
        let tl_enums::account::Password::Password(ref pwd) = pwd_tl;

        let new_algo = parse_password_kdf_algo(&pwd.new_algo)
            .map(validate_new_algo)
            .ok_or_else(|| "Unsupported new password KDF algorithm.".to_owned())?;
        let new_password_hash = compute_digest(&new_algo, password.as_bytes())
            .map_err(|e| format!("SRP verifier computation failed: {e}"))?;

        let new_secure_algo = parse_secure_secret_algo(&pwd.new_secure_algo)
            .and_then(|a| validate_new_secure_secret_algo(a).ok())
            .ok_or_else(|| "Unsupported Passport secret KDF algorithm.".to_owned())?;
        let secret = generate_secret_bytes();
        let secret_id = count_secure_secret_id(&secret);
        let hash_for_secret = compute_secure_secret_hash(&new_secure_algo, password.as_bytes());
        let encrypted_secret = encrypt_secure_secret(&secret, &hash_for_secret)
            .map_err(|e| format!("Passport secret encryption failed: {e}"))?;

        let req = tl_fns::account::UpdatePasswordSettings {
            password: tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty,
            new_settings: tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                tl_types::account::PasswordInputSettings {
                    new_algo: Some(
                        tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                            tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                                salt1: new_algo.salt1,
                                salt2: new_algo.salt2,
                                g: new_algo.g,
                                p: new_algo.p,
                            },
                        ),
                    ),
                    new_password_hash: Some(new_password_hash.to_vec()),
                    hint: Some(hint.to_owned()),
                    email: None,
                    new_secure_settings: Some(
                        tl_enums::SecureSecretSettings::SecureSecretSettings(
                            tl_types::SecureSecretSettings {
                                secure_algo: secure_secret_algo_to_tl(&new_secure_algo),
                                secure_secret: encrypted_secret.to_vec(),
                                secure_secret_id: secret_id as i64,
                            },
                        ),
                    ),
                },
            ),
        };
        let _ = invoke_rpc(self, req)?;
        controller.apply_new_secret_saved(secret, secret_id);
        let form = controller.form().clone();
        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
        });
        self.bridge_state().notify_ui();
        Ok(PassportUnlockResult {
            form,
            notice: "Cloud password and Telegram Passport secret configured.".to_owned(),
        })
    }

    fn upload_passport_file(
        &mut self,
        value_type: ValueType,
        file_type: FileType,
        raw_bytes: &[u8],
    ) -> Result<PassportFile, String> {
        let (master_secret, file_id) = with_payments_state(self, |st| {
            let secret = st
                .passport_controller
                .as_ref()
                .and_then(|c| c.secret().copied());
            let fid = st.next_random_id();
            (secret, fid)
        });
        let master_secret = master_secret.ok_or_else(|| {
            "Unlock Telegram Passport with your cloud password before uploading scans.".to_owned()
        })?;

        let edit_file = crate::passport_upload::encrypt_and_upload_passport_file(
            self,
            &master_secret,
            file_type,
            raw_bytes,
            file_id,
        )?;
        let uploaded = edit_file.fields.clone();

        with_payments_state(self, |st| {
            if let Some(ref mut controller) = st.passport_controller {
                crate::passport_upload::stage_uploaded_file_in_controller(
                    controller, value_type, edit_file,
                );
            }
        });
        self.bridge_state().notify_ui();
        Ok(uploaded)
    }

    fn save_passport_value(
        &mut self,
        value_type: ValueType,
        fields: ValueMap,
        scan_labels: Vec<String>,
    ) -> Result<PassportForm, String> {
        for label in &scan_labels {
            if let Some((file_type, path)) =
                crate::passport_upload::parse_scan_upload_spec(label, value_type)
            {
                let raw_bytes = std::fs::read(&path).map_err(|e| {
                    format!("Failed to read Passport scan '{}': {e}", path.display())
                })?;
                let _ = self.upload_passport_file(value_type, file_type, &raw_bytes)?;
            }
        }
        let mut controller = with_payments_state(self, |st| st.passport_controller.clone())
            .ok_or_else(|| "No active Passport authorization form.".to_owned())?;
        let req = controller
            .prepare_save_value(value_type, fields)
            .map_err(|e| e.to_string())?;
        let saved_tl = invoke_rpc(self, req)?;
        controller.apply_saved_value_tl(&saved_tl);
        let form = controller.form().clone();
        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
        });
        self.bridge_state().notify_ui();
        Ok(form)
    }

    fn send_contact_verification_code(
        &mut self,
        value_type: ValueType,
        value: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("Contact value cannot be empty.".to_owned());
        }
        match value_type {
            ValueType::Phone => {
                let sent = invoke_rpc(
                    self,
                    tl_fns::account::SendVerifyPhoneCode {
                        phone_number: trimmed.to_owned(),
                        settings: tl_enums::CodeSettings::CodeSettings(tl_types::CodeSettings {
                            allow_flashcall: false,
                            current_number: false,
                            allow_app_hash: false,
                            allow_missed_call: false,
                            allow_firebase: false,
                            unknown_number: false,
                            logout_tokens: None,
                            token: None,
                            app_sandbox: None,
                        }),
                    },
                )?;
                parse_passport_sent_phone_code(trimmed, sent)
            }
            ValueType::Email => {
                let tl_enums::account::SentEmailCode::SentEmailCode(sent) = invoke_rpc(
                    self,
                    tl_fns::account::SendVerifyEmailCode {
                        purpose: tl_enums::EmailVerifyPurpose::Passport,
                        email: trimmed.to_owned(),
                    },
                )?;
                Ok(PassportVerificationChallenge {
                    value_type: ValueType::Email,
                    target: trimmed.to_owned(),
                    code_length: if sent.length > 0 { sent.length } else { -1 },
                    phone_code_hash: None,
                    email_pattern: Some(sent.email_pattern),
                    fragment_url: None,
                    next_call_timeout_secs: None,
                })
            }
            _ => Err("Verification codes are only supported for phone and email.".to_owned()),
        }
    }

    fn resend_phone_verification_code(
        &mut self,
        phone: &str,
        phone_code_hash: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        let trimmed = phone.trim();
        if trimmed.is_empty() || phone_code_hash.trim().is_empty() {
            return Err("Phone number and phone_code_hash are required to resend code.".to_owned());
        }
        let sent = invoke_rpc(
            self,
            tl_fns::auth::ResendCode {
                phone_number: trimmed.to_owned(),
                phone_code_hash: phone_code_hash.to_owned(),
                reason: None,
            },
        )?;
        parse_passport_sent_phone_code(trimmed, sent)
    }

    fn confirm_contact_verification_code(
        &mut self,
        value_type: ValueType,
        value: &str,
        phone_code_hash: Option<&str>,
        code: &str,
    ) -> Result<PassportForm, String> {
        let trimmed_val = value.trim();
        let trimmed_code = code.trim();
        if trimmed_code.is_empty() {
            return Err("Please enter the verification code.".to_owned());
        }
        match value_type {
            ValueType::Phone => {
                let hash = phone_code_hash
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| "Missing phone verification hash.".to_owned())?;
                let verified = invoke_rpc(
                    self,
                    tl_fns::account::VerifyPhone {
                        phone_number: trimmed_val.to_owned(),
                        phone_code_hash: hash.to_owned(),
                        phone_code: trimmed_code.to_owned(),
                    },
                )?;
                if !verified {
                    return Err("Phone verification code was rejected.".to_owned());
                }
            }
            ValueType::Email => {
                let _verified = invoke_rpc(
                    self,
                    tl_fns::account::VerifyEmail {
                        purpose: tl_enums::EmailVerifyPurpose::Passport,
                        verification: tl_enums::EmailVerification::Code(
                            tl_types::EmailVerificationCode {
                                code: trimmed_code.to_owned(),
                            },
                        ),
                    },
                )?;
            }
            _ => return Err("Unsupported contact verification type.".to_owned()),
        }
        let mut fields = ValueMap::default();
        fields.set_text("value", trimmed_val.to_owned());
        self.save_passport_value(value_type, fields, Vec::new())
    }

    fn delete_passport_values(&mut self, types: &[ValueType]) -> Result<PassportForm, String> {
        if types.is_empty() {
            return Err("No Passport value types specified for deletion.".to_owned());
        }
        let tl_types_vec: Vec<tl_enums::SecureValueType> =
            types.iter().map(|t| t.to_tl()).collect();
        let _ = invoke_rpc(
            self,
            tl_fns::account::DeleteSecureValue {
                types: tl_types_vec,
            },
        )?;
        let form = with_payments_state(self, |st| {
            if let Some(ref mut controller) = st.passport_controller {
                for &vt in types {
                    controller
                        .form_mut()
                        .values
                        .insert(vt, PassportValue::new(vt));
                }
                Some(controller.form().clone())
            } else {
                None
            }
        })
        .ok_or_else(|| "No active Passport authorization form.".to_owned())?;
        self.bridge_state().notify_ui();
        Ok(form)
    }

    fn reset_passport(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        let mut controller = with_payments_state(self, |st| st.passport_controller.clone())
            .ok_or_else(|| "No active Passport authorization form.".to_owned())?;

        // 1. Fetch fresh SRP challenge and reset secure secret via `account.updatePasswordSettings`.
        let fresh_pwd = invoke_rpc(self, tl_fns::account::GetPassword {})?;
        controller.apply_password_tl(&fresh_pwd);
        if let Some(challenge) = controller.password_settings().challenge.as_ref() {
            let srp_check = compute_check(
                &challenge.algo,
                &challenge.srp_b,
                challenge.srp_id,
                password.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            let reset_req = controller.prepare_reset_secret(&srp_check);
            let _ = invoke_rpc(self, reset_req)?;
        }

        // 2. Delete all stored Passport secure values via `account.deleteSecureValue`.
        let all_types = [
            ValueType::PersonalDetails,
            ValueType::Passport,
            ValueType::DriverLicense,
            ValueType::IdentityCard,
            ValueType::InternalPassport,
            ValueType::Address,
            ValueType::UtilityBill,
            ValueType::BankStatement,
            ValueType::RentalAgreement,
            ValueType::PassportRegistration,
            ValueType::TemporaryRegistration,
            ValueType::Phone,
            ValueType::Email,
        ];
        let _ = invoke_rpc(
            self,
            tl_fns::account::DeleteSecureValue {
                types: all_types.iter().map(|t| t.to_tl()).collect(),
            },
        )?;
        for vt in all_types {
            controller
                .form_mut()
                .values
                .insert(vt, PassportValue::new(vt));
        }

        let form = controller.form().clone();
        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
        });
        self.bridge_state().notify_ui();
        Ok(PassportUnlockResult {
            form,
            notice: "Telegram Passport secret and stored values were reset.".to_owned(),
        })
    }

    fn load_passport_config(&mut self) -> Result<PassportConfig, String> {
        let current_hash = with_payments_state(self, |st| st.passport_config.hash);
        let tl_res = invoke_rpc(self, tl_fns::help::GetPassportConfig { hash: current_hash })?;
        let config = match tl_res {
            tl_enums::help::PassportConfig::NotModified => {
                with_payments_state(self, |st| st.passport_config.clone())
            }
            tl_enums::help::PassportConfig::PassportConfig(data) => {
                let tl_enums::DataJSON::DataJSON(ref json) = data.countries_langs;
                let parsed = PassportConfig::parse_json(data.hash, json.data.as_bytes())
                    .map_err(|e| e.to_string())?;
                with_payments_state(self, |st| {
                    st.passport_config = parsed.clone();
                });
                parsed
            }
        };
        with_payments_state(self, |st| {
            if let Some(ref mut controller) = st.passport_controller {
                controller.set_passport_config(config.clone());
                if controller.secret().is_some() {
                    let _ = controller.fill_native_from_fallback();
                }
            }
        });
        Ok(config)
    }

    fn accept_authorization(&mut self) -> Result<String, String> {
        let mut controller = with_payments_state(self, |st| st.passport_controller.clone())
            .ok_or_else(|| "No active Passport authorization form.".to_owned())?;
        let prepared = controller
            .prepare_accept_authorization()
            .map_err(|e| e.to_string())?;
        let _ = invoke_rpc(self, prepared.request)?;
        controller.mark_submit_success();
        let msg = controller
            .callback_url_for_outcome(&CallbackOutcome::Success)
            .unwrap_or_else(|| "Telegram Passport authorization accepted.".to_owned());
        with_payments_state(self, |st| {
            st.passport_controller = Some(controller);
        });
        self.bridge_state().notify_ui();
        Ok(msg)
    }

    fn decline_authorization(&mut self) -> Result<String, String> {
        let msg = with_payments_state(self, |st| {
            let url = st
                .passport_controller
                .as_ref()
                .and_then(|c| c.callback_url_for_outcome(&CallbackOutcome::Cancel));
            st.passport_controller = None;
            url.unwrap_or_else(|| "Telegram Passport authorization declined.".to_owned())
        });
        Ok(msg)
    }
}

fn parse_passport_sent_phone_code(
    phone: &str,
    sent: tl_enums::auth::SentCode,
) -> Result<PassportVerificationChallenge, String> {
    match sent {
        tl_enums::auth::SentCode::SentCode(data) => {
            let mut code_length = -1i32;
            let mut fragment_url = None;
            match &data.r#type {
                tl_enums::auth::SentCodeType::Sms(s) => {
                    code_length = if s.length > 0 { s.length } else { -1 };
                }
                tl_enums::auth::SentCodeType::Call(c) => {
                    code_length = if c.length > 0 { c.length } else { -1 };
                }
                tl_enums::auth::SentCodeType::FragmentSms(f) => {
                    code_length = f.length;
                    fragment_url = Some(f.url.clone());
                }
                tl_enums::auth::SentCodeType::App(a) => {
                    code_length = if a.length > 0 { a.length } else { -1 };
                }
                _ => {}
            }
            Ok(PassportVerificationChallenge {
                value_type: ValueType::Phone,
                target: phone.to_owned(),
                code_length,
                phone_code_hash: Some(data.phone_code_hash),
                email_pattern: None,
                fragment_url,
                next_call_timeout_secs: data.timeout,
            })
        }
        tl_enums::auth::SentCode::Success(_) => {
            Err("Unexpected auth.sentCodeSuccess for Passport phone verification.".to_owned())
        }
        tl_enums::auth::SentCode::PaymentRequired(_) => {
            Err("Phone verification requires SMS fee confirmation.".to_owned())
        }
    }
}

fn resolve_bot_user(live: &LiveSession, username: &str) -> Result<(DataUserId, String), String> {
    if let Some(found) = read_session(live, |s| {
        for pid in known_peer_ids(s) {
            if let Some(Peer::User(u)) = s.peer(pid)
                && u.usernames.username().eq_ignore_ascii_case(username)
            {
                return Some((u.user_id(), u.common.name.clone()));
            }
        }
        None
    }) {
        return Ok(found);
    }
    let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) = invoke_rpc(
        live,
        tl_fns::contacts::ResolveUsername {
            username: username.to_owned(),
            referer: None,
        },
    )?;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &resolved.users, &resolved.chats);
    });
    let pid = tg_api::convert::peer_id_from_tl(&resolved.peer);
    let uid = pid
        .to_user()
        .ok_or_else(|| format!("Resolved peer '{username}' is not a bot user."))?;
    let name = read_session(live, |s| peer_display_title(s, pid));
    Ok((uid, name))
}

fn extract_premium_invoice_slug(bot_url: &str, months: i32) -> String {
    let trimmed = bot_url.trim();
    if let Some(slug) = trimmed
        .strip_prefix("https://t.me/$")
        .or_else(|| trimmed.strip_prefix("http://t.me/$"))
        .or_else(|| trimmed.strip_prefix("t.me/$"))
        .or_else(|| trimmed.strip_prefix("tg://invoice?slug="))
        && !slug.is_empty()
    {
        return slug.split(['&', '#']).next().unwrap_or(slug).to_owned();
    }
    if let Some((_, start_param)) = trimmed.split_once("start=") {
        let cleaned = start_param.split(['&', '#']).next().unwrap_or("").trim();
        if !cleaned.is_empty() {
            return cleaned.to_owned();
        }
    }
    format!("premium_{months}m")
}

trait PipeExt: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}

impl<T> PipeExt for T {}
