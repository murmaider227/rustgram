//! Intro / login wizard driven by `tg-api`'s [`LoginStateMachine`].
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_widget.h`,
//! `Telegram/SourceFiles/intro/intro_widget.cpp`,
//! `Telegram/SourceFiles/intro/intro_step.cpp`,
//! `Telegram/SourceFiles/intro/intro_start.cpp`,
//! `Telegram/SourceFiles/intro/intro_phone.cpp`,
//! `Telegram/SourceFiles/intro/intro_code.cpp`,
//! `Telegram/SourceFiles/intro/intro_code_input.cpp`,
//! `Telegram/SourceFiles/intro/intro_password_check.cpp`,
//! `Telegram/SourceFiles/intro/intro_signup.cpp`,
//! `Telegram/SourceFiles/intro/intro_qr.cpp`,
//! `Telegram/SourceFiles/intro/intro_email.cpp`, and
//! `Telegram/SourceFiles/intro/intro.style`.

pub mod auth_boxes;
pub mod code;
pub mod email;
pub mod fake_invoker;
mod fake_langpack;
pub mod language_link;
pub mod passkey_rpc;
pub mod password;
pub mod phone;
pub mod qr;
pub mod signup;
pub mod start;
pub mod style;
#[cfg(test)]
mod tests_language_link;

#[cfg(test)]
mod screenshots;

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{Align, Frame, Key, Layout, Margin, Pos2, RichText, Ui, Vec2};
use tg_api::error::{ApiError, LoginErrorKind};
use tg_api::invoker::{InvokeResult, Invoker};
use tg_api::login::{CallStatus, LoginStateMachine, LoginStep};
use tg_api::rpc::{DcId, InvocationError, RpcClient, RpcError};
use tg_api::types::ApiCredentials;
use tg_data::TimeId;
use tg_lang::{hardcoded, tr};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::broadcast;

use crate::intro::code::{CodeDeliveryKind, CodeStepAction, CodeStepState};
use crate::intro::email::{EmailStepAction, EmailStepState};
use crate::intro::fake_invoker::FakeRpcClient;
use crate::intro::password::{
    PasswordModal, PasswordStepAction, PasswordStepState, format_reset_confirm_wait,
};
use crate::intro::phone::{PhoneStepAction, PhoneStepState};
use crate::intro::qr::{QrStepAction, QrStepState};
use crate::intro::signup::{SignupStepAction, SignupStepState};
use crate::intro::start::{StartStepAction, StartStepState};
use crate::intro::style::{
    CoverTransition, INTRO_COVER_HEIGHT, INTRO_STEP_WIDTH, TgforkIconCache, paint_intro_cover,
};
use tg_app_core::core_types::window_types::{SlideAnimation, SlideDirection};
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{AuthUser, PeerId};

pub use tg_app_core::core_types::intro_rpc::{DynIntroRpc, IntroBoxFuture};

struct IntroInvokerHandle(Arc<dyn DynIntroRpc>);

fn unused_in_intro_rpc() -> InvocationError {
    InvocationError::Rpc(RpcError::new(501, "NOT_USED_BY_INTRO"))
}

impl Invoker for IntroInvokerHandle {
    fn main_dc_id(&self) -> DcId {
        self.0.main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.0.set_main_dc_id(dc_id);
    }

    async fn help_get_nearest_dc(&self) -> InvokeResult<tl_types::NearestDc> {
        self.0.help_get_nearest_dc().await
    }

    async fn help_get_countries_list(
        &self,
        _: tl_fns::help::GetCountriesList,
    ) -> InvokeResult<tl_enums::help::CountriesList> {
        Err(unused_in_intro_rpc())
    }

    async fn auth_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> InvokeResult<tl_enums::auth::SentCode> {
        self.0.auth_send_code(req).await
    }

    async fn auth_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> InvokeResult<tl_enums::auth::SentCode> {
        self.0.auth_resend_code(req).await
    }

    async fn auth_cancel_code(&self, req: tl_fns::auth::CancelCode) -> InvokeResult<bool> {
        self.0.auth_cancel_code(req).await
    }

    async fn auth_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.0.auth_sign_in(req).await
    }

    async fn auth_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.0.auth_sign_up(req).await
    }

    async fn account_get_password(&self) -> InvokeResult<tl_types::account::Password> {
        self.0.account_get_password().await
    }

    async fn auth_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.0.auth_check_password(req).await
    }

    async fn auth_request_password_recovery(
        &self,
    ) -> InvokeResult<tl_types::auth::PasswordRecovery> {
        self.0.auth_request_password_recovery().await
    }

    async fn auth_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> InvokeResult<bool> {
        self.0.auth_check_recovery_password(req).await
    }

    async fn auth_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.0.auth_recover_password(req).await
    }

    async fn account_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> InvokeResult<tl_types::account::SentEmailCode> {
        self.0.account_send_verify_email_code(req).await
    }

    async fn account_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> InvokeResult<tl_enums::account::EmailVerified> {
        self.0.account_verify_email(req).await
    }

    async fn account_delete_account(
        &self,
        req: tl_fns::account::DeleteAccount,
    ) -> InvokeResult<bool> {
        self.0.account_delete_account(req).await
    }

    async fn auth_export_login_token(
        &self,
        req: tl_fns::auth::ExportLoginToken,
    ) -> InvokeResult<tl_enums::auth::LoginToken> {
        self.0.auth_export_login_token(req).await
    }

    async fn auth_import_login_token(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::ImportLoginToken,
    ) -> InvokeResult<tl_enums::auth::LoginToken> {
        self.0.auth_import_login_token(dc_id, req).await
    }

    async fn auth_log_out(&self) -> InvokeResult<tl_types::auth::LoggedOut> {
        self.0.auth_log_out().await
    }

    async fn messages_get_dialog_filters(&self) -> InvokeResult<tl_types::messages::DialogFilters> {
        self.0.messages_get_dialog_filters().await
    }

    async fn messages_get_dialogs(
        &self,
        _: tl_fns::messages::GetDialogs,
    ) -> InvokeResult<tl_enums::messages::Dialogs> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_get_pinned_dialogs(
        &self,
        _: tl_fns::messages::GetPinnedDialogs,
    ) -> InvokeResult<tl_types::messages::PeerDialogs> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_reorder_pinned_dialogs(
        &self,
        _: tl_fns::messages::ReorderPinnedDialogs,
    ) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_get_peer_dialogs(
        &self,
        _: tl_fns::messages::GetPeerDialogs,
    ) -> InvokeResult<tl_types::messages::PeerDialogs> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_mark_dialog_unread(
        &self,
        _: tl_fns::messages::MarkDialogUnread,
    ) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn folders_edit_peer_folders(
        &self,
        _: tl_fns::folders::EditPeerFolders,
    ) -> InvokeResult<tl_enums::Updates> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_get_history(
        &self,
        _: tl_fns::messages::GetHistory,
    ) -> InvokeResult<tl_enums::messages::Messages> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_get_replies(
        &self,
        _: tl_fns::messages::GetReplies,
    ) -> InvokeResult<tl_enums::messages::Messages> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_send_message(
        &self,
        _: tl_fns::messages::SendMessage,
    ) -> InvokeResult<tl_enums::Updates> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_edit_message(
        &self,
        _: tl_fns::messages::EditMessage,
    ) -> InvokeResult<tl_enums::Updates> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_delete_messages(
        &self,
        _: tl_fns::messages::DeleteMessages,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        Err(unused_in_intro_rpc())
    }

    async fn channels_delete_messages(
        &self,
        _: tl_fns::channels::DeleteMessages,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_delete_history(
        &self,
        _: tl_fns::messages::DeleteHistory,
    ) -> InvokeResult<tl_types::messages::AffectedHistory> {
        Err(unused_in_intro_rpc())
    }

    async fn channels_delete_history(
        &self,
        _: tl_fns::channels::DeleteHistory,
    ) -> InvokeResult<tl_enums::Updates> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_forward_messages(
        &self,
        _: tl_fns::messages::ForwardMessages,
    ) -> InvokeResult<tl_enums::Updates> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_read_history(
        &self,
        _: tl_fns::messages::ReadHistory,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        Err(unused_in_intro_rpc())
    }

    async fn channels_read_history(&self, _: tl_fns::channels::ReadHistory) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_set_typing(&self, _: tl_fns::messages::SetTyping) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn messages_save_draft(&self, _: tl_fns::messages::SaveDraft) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn updates_get_state(&self) -> InvokeResult<tl_types::updates::State> {
        Err(unused_in_intro_rpc())
    }

    async fn updates_get_difference(
        &self,
        _: tl_fns::updates::GetDifference,
    ) -> InvokeResult<tl_enums::updates::Difference> {
        Err(unused_in_intro_rpc())
    }

    async fn updates_get_channel_difference(
        &self,
        _: tl_fns::updates::GetChannelDifference,
    ) -> InvokeResult<tl_enums::updates::ChannelDifference> {
        Err(unused_in_intro_rpc())
    }

    async fn account_update_status(&self, _: tl_fns::account::UpdateStatus) -> InvokeResult<bool> {
        Err(unused_in_intro_rpc())
    }

    async fn langpack_get_languages(
        &self,
        _: tl_fns::langpack::GetLanguages,
    ) -> InvokeResult<Vec<tl_enums::LangPackLanguage>> {
        Err(unused_in_intro_rpc())
    }

    async fn langpack_get_lang_pack(
        &self,
        _: tl_fns::langpack::GetLangPack,
    ) -> InvokeResult<tl_types::LangPackDifference> {
        Err(unused_in_intro_rpc())
    }

    async fn langpack_get_difference(
        &self,
        _: tl_fns::langpack::GetDifference,
    ) -> InvokeResult<tl_types::LangPackDifference> {
        Err(unused_in_intro_rpc())
    }

    async fn langpack_get_language(
        &self,
        _: tl_fns::langpack::GetLanguage,
    ) -> InvokeResult<tl_types::LangPackLanguage> {
        Err(unused_in_intro_rpc())
    }

    async fn langpack_get_strings(
        &self,
        _: tl_fns::langpack::GetStrings,
    ) -> InvokeResult<Vec<tl_enums::LangPackString>> {
        Err(unused_in_intro_rpc())
    }
}

/// Slide animation direction (`Intro::details::Animate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Animate {
    /// Slide incoming step from the left.
    Back,
    /// Slide incoming step from the right.
    Forward,
}

/// Logical intro wizard steps mirroring `tg_api::LoginStep` and tdesktop `Intro::details::Step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntroStep {
    /// Welcome cover step (`Intro::StartWidget`).
    Start,
    /// QR code login (`Intro::QrWidget`).
    Qr,
    /// Phone number entry (`Intro::PhoneWidget`).
    Phone,
    /// Mandatory login email setup (`Intro::EmailWidget`).
    EmailSetup,
    /// Confirming newly configured login email (`Intro::CodeWidget` in email-setup mode).
    EmailVerify,
    /// Verification code entry (`Intro::CodeWidget`).
    Code,
    /// Cloud 2FA password & recovery (`Intro::PasswordCheckWidget`).
    Password,
    /// New account sign-up (`Intro::SignupWidget`).
    SignUp,
}

impl IntroStep {
    /// Maps a `tg_api::LoginStep` to its visual [`IntroStep`].
    #[must_use]
    pub const fn from_login_step(step: &LoginStep) -> Self {
        match step {
            LoginStep::Start { .. } => Self::Start,
            LoginStep::QrLogin(_) => Self::Qr,
            LoginStep::PhoneInput => Self::Phone,
            LoginStep::EmailSetup { .. } => Self::EmailSetup,
            LoginStep::EmailVerify { .. } => Self::EmailVerify,
            LoginStep::CodeInput(_) => Self::Code,
            LoginStep::PasswordCheck { .. } => Self::Password,
            LoginStep::SignUp { .. } => Self::SignUp,
            LoginStep::Authorized { .. } => Self::Start,
        }
    }

    /// Returns `true` if this step renders the top welcome cover (`Step::hasCover`).
    #[must_use]
    pub const fn has_cover(self) -> bool {
        matches!(self, Self::Start)
    }

    /// Returns `true` if the top-left Back button should be visible (`Step::hasBack`).
    #[must_use]
    pub const fn has_back(self) -> bool {
        !matches!(self, Self::Start)
    }
}

/// Events emitted by [`IntroWidget`] to [`crate::window::controller::WindowController`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntroAction {
    /// Login completed successfully; transition to main session window.
    SignedIn(AuthUser),
    /// User clicked the top-right Settings button (opens proxy/connection settings).
    OpenSettings,
}

/// Maps `tg_api::LoginErrorKind` to the localized `tg-lang` phrase matching tdesktop `intro/`.
#[must_use]
pub fn localized_login_error(kind: &LoginErrorKind) -> String {
    match kind {
        LoginErrorKind::PhoneNumberInvalid => tr::lng_bad_phone(),
        LoginErrorKind::PhoneNumberBanned => tr::lng_signin_banned_text(),
        LoginErrorKind::PhoneNumberFlood => tr::lng_error_phone_flood(),
        LoginErrorKind::PhoneNumberOccupied => tr::lng_bad_phone(),
        LoginErrorKind::PhoneCodeInvalid | LoginErrorKind::PhoneCodeExpired => tr::lng_bad_code(),
        LoginErrorKind::SendCodeUnavailable => String::new(),
        LoginErrorKind::SessionPasswordNeeded => String::new(),
        LoginErrorKind::PasswordInvalid => tr::lng_signin_bad_password(),
        LoginErrorKind::PasswordEmptyOrUnregistered => String::new(),
        LoginErrorKind::SrpIdInvalid => hardcoded::server_error().to_owned(),
        LoginErrorKind::PasswordRecoveryUnavailable => tr::lng_signin_no_email_forgot(),
        LoginErrorKind::PasswordRecoveryExpired => tr::lng_signin_wrong_code(),
        LoginErrorKind::RecoveryCodeInvalid => tr::lng_signin_wrong_code(),
        LoginErrorKind::FirstNameInvalid | LoginErrorKind::LastNameInvalid => tr::lng_bad_name(),
        LoginErrorKind::EmailInvalid => tr::lng_cloud_password_bad_email(),
        LoginErrorKind::EmailNotAllowed => tr::lng_settings_error_email_not_alowed(),
        LoginErrorKind::EmailHashExpired => hardcoded::email_confirmation_expired().to_owned(),
        LoginErrorKind::TwoFaConfirmWait { seconds } => format_reset_confirm_wait("", *seconds),
        LoginErrorKind::TwoFaRecentConfirm => tr::lng_signin_reset_cancelled(),
        LoginErrorKind::FloodWait { .. } => tr::lng_flood_error(),
        LoginErrorKind::MigrateDc { .. } => String::new(),
        LoginErrorKind::TermsNotAccepted => tr::lng_terms_signup_link(),
        LoginErrorKind::OutdatedClient => tr::lng_passport_app_out_of_date(),
        LoginErrorKind::Ignored { .. } => String::new(),
        LoginErrorKind::Other(raw) => raw.clone(),
    }
}

/// Passkey login outcome tracked alongside `LoginStateMachine`.
enum PasskeyAuthState {
    Authorized(Box<tl_types::User>),
    PasswordCheck(Box<tg_api::login::CloudPasswordState>),
}

/// Stateful intro flow widget (`Intro::Widget`) backed by `tg_api::LoginStateMachine`.
pub struct IntroWidget {
    runtime: tokio::runtime::Runtime,
    credentials: ApiCredentials,
    login: LoginStateMachine,
    invoker: IntroInvokerHandle,
    fake_rpc: Option<Arc<FakeRpcClient>>,
    updates_rx: Option<broadcast::Receiver<tl_enums::Updates>>,
    passkey_provider: Arc<dyn tg_platform::webauthn::WebAuthnProvider>,
    cable_scanner: Arc<dyn tg_platform::cable_scanner::CableBleScanner>,
    cable_tunnel: Arc<dyn tg_platform::cable_tunnel::CableTunnelConnector>,
    active_passkey_cable: Arc<std::sync::Mutex<Option<tg_platform::webauthn::CableCeremonyHandle>>>,
    passkey_cable_status:
        Arc<std::sync::Mutex<tg_app_core::settings::backend::PasskeyHybridQrStatus>>,
    passkey_matched_eid: Arc<std::sync::Mutex<Option<tg_platform::cable_scanner::MatchedCableEid>>>,
    passkey_hybrid_login_result:
        Arc<std::sync::Mutex<Option<Result<tg_platform::webauthn::LoginResult, String>>>>,
    passkey_cable_generation: Arc<std::sync::atomic::AtomicU64>,
    passkey_window_hwnd: usize,
    passkey_login_cache: Option<(tg_platform::webauthn::LoginData, Instant, DcId)>,
    passkey_auth: Option<PasskeyAuthState>,
    /// Mirror of `LoginStateMachine`'s previous step kinds so QR <-> Phone toggling
    /// pops instead of growing the stack unboundedly.
    stack_kinds: Vec<IntroStep>,
    slide_animation: Option<SlideAnimation>,
    cover_transition: Option<CoverTransition>,
    /// Start step view state.
    pub start: StartStepState,
    /// Phone step view state (country picker + phone formatting).
    pub phone: PhoneStepState,
    /// Code step view state (segmented digit input + call timer).
    pub code: CodeStepState,
    /// 2FA password & recovery step view state.
    pub password: PasswordStepState,
    /// New account sign-up step view state.
    pub signup: SignupStepState,
    /// QR login step view state.
    pub qr: QrStepState,
    /// Login email setup step view state.
    pub email: EmailStepState,
    /// Optional unconfirmed-auth-denied security modal (`UnconfirmedAuthDeniedBox`).
    pub unconfirmed_auth_denied: Option<auth_boxes::UnconfirmedAuthDeniedBoxState>,
    /// Current localized error text shown on the active step.
    pub error: String,
    icon_cache: TgforkIconCache,
    nearest_dc_fetched: bool,
    nearest_dc_rx: Option<std::sync::mpsc::Receiver<InvokeResult<tl_types::NearestDc>>>,
    language_link: language_link::LanguageLinkTasks,
}

impl std::fmt::Debug for IntroWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntroWidget")
            .field("login_step", self.login.step())
            .field("step_stack_len", &self.login.step_stack_len())
            .field("error", &self.error)
            .finish()
    }
}

impl Default for IntroWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl IntroWidget {
    /// Creates a new `IntroWidget` starting at [`IntroStep::Start`] with an offline [`FakeRpcClient`].
    ///
    /// Uses placeholder local-only credentials (`api_id = 0`, empty hash) when running offline;
    /// never embeds real Telegram API credentials.
    #[must_use]
    pub fn new() -> Self {
        Self::with_invoker(
            ApiCredentials {
                api_id: 0,
                api_hash: String::new(),
            },
            FakeRpcClient::default(),
        )
    }

    /// Creates a new `IntroWidget` backed by an explicit offline [`FakeRpcClient`].
    #[must_use]
    pub fn with_invoker(credentials: ApiCredentials, invoker: FakeRpcClient) -> Self {
        let fake = Arc::new(invoker);
        let dyn_rpc: Arc<dyn DynIntroRpc> = fake.clone();
        Self::build(credentials, dyn_rpc, Some(fake))
    }

    /// Creates a new `IntroWidget` wired to any [`tg_api::RpcClient`] (including `Arc<C>`),
    /// allowing the real MTProto backend to be plugged in directly.
    #[must_use]
    pub fn with_rpc_client<C: RpcClient + 'static>(credentials: ApiCredentials, client: C) -> Self {
        let dyn_rpc: Arc<dyn DynIntroRpc> = Arc::new(client);
        Self::build(credentials, dyn_rpc, None)
    }

    /// Creates a new `IntroWidget` wired to a pre-erased `Arc<dyn DynIntroRpc>`.
    #[must_use]
    pub fn with_dyn_rpc_client(credentials: ApiCredentials, client: Arc<dyn DynIntroRpc>) -> Self {
        Self::build(credentials, client, None)
    }

    fn build(
        credentials: ApiCredentials,
        client: Arc<dyn DynIntroRpc>,
        fake_rpc: Option<Arc<FakeRpcClient>>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("failed to construct local intro tokio runtime");
        let updates_rx = client.subscribe_updates();
        let login = LoginStateMachine::new(credentials.clone(), "US");
        let passkey_provider: Arc<dyn tg_platform::webauthn::WebAuthnProvider> =
            Arc::new(tg_platform::webauthn::WindowsWebAuthnProvider::new());
        let cable_scanner: Arc<dyn tg_platform::cable_scanner::CableBleScanner> =
            if fake_rpc.is_some() {
                Arc::new(tg_platform::cable_scanner::FakeCableBleScanner::new(true))
            } else {
                Arc::new(tg_platform::cable_scanner::WinRtCableBleScanner::new())
            };
        let mut qr = QrStepState::new();
        if fake_rpc.is_none() {
            qr.passkey_supported = passkey_provider.is_supported();
        }
        Self {
            runtime,
            credentials,
            login,
            invoker: IntroInvokerHandle(client),
            fake_rpc,
            updates_rx,
            passkey_provider,
            cable_scanner,
            cable_tunnel: Arc::new(tg_platform::cable_tunnel::TlsCableTunnelConnector::new()),
            active_passkey_cable: Arc::new(std::sync::Mutex::new(None)),
            passkey_cable_status: Arc::new(std::sync::Mutex::new(
                tg_app_core::settings::backend::PasskeyHybridQrStatus::Idle,
            )),
            passkey_matched_eid: Arc::new(std::sync::Mutex::new(None)),
            passkey_hybrid_login_result: Arc::new(std::sync::Mutex::new(None)),
            passkey_cable_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            passkey_window_hwnd: 0,
            passkey_login_cache: None,
            passkey_auth: None,
            stack_kinds: Vec::new(),
            slide_animation: None,
            cover_transition: None,
            start: StartStepState::new(),
            phone: PhoneStepState::new("US"),
            code: CodeStepState::new(),
            password: PasswordStepState::new(),
            signup: SignupStepState::new(),
            qr,
            email: EmailStepState::new(),
            unconfirmed_auth_denied: None,
            error: String::new(),
            icon_cache: TgforkIconCache::default(),
            nearest_dc_fetched: false,
            nearest_dc_rx: None,
            language_link: language_link::LanguageLinkTasks::default(),
        }
    }

    /// Returns a reference to the underlying `tg_api::LoginStateMachine`.
    #[must_use]
    pub const fn login_state_machine(&self) -> &LoginStateMachine {
        &self.login
    }

    /// Opens the `UnconfirmedAuthDeniedBox` warning modal on top of the intro flow.
    pub fn show_unconfirmed_auth_denied(
        &mut self,
        count: u32,
        message_text: impl Into<String>,
        bot: bool,
        mixed: bool,
    ) {
        self.unconfirmed_auth_denied = Some(auth_boxes::UnconfirmedAuthDeniedBoxState::new(
            count,
            message_text,
            bot,
            mixed,
        ));
    }

    /// Returns the current `tg_api::LoginStep`.
    #[must_use]
    pub fn login_step(&self) -> &LoginStep {
        self.login.step()
    }

    /// Returns the currently selected main data center ID reported by the active RPC client.
    #[must_use]
    pub fn main_dc_id(&self) -> DcId {
        self.invoker.main_dc_id()
    }

    /// Returns a reference to the offline [`FakeRpcClient`] when running in offline demo mode.
    #[must_use]
    pub fn fake_rpc(&self) -> Option<&FakeRpcClient> {
        self.fake_rpc.as_deref()
    }

    /// Replaces the platform WebAuthn provider used for `"Log in using passkey"` (for tests).
    pub fn set_passkey_provider(
        &mut self,
        provider: Arc<dyn tg_platform::webauthn::WebAuthnProvider>,
    ) {
        self.qr.passkey_supported = provider.is_supported();
        self.passkey_provider = provider;
    }

    /// Replaces the platform caBLE BLE scanner used for hybrid QR passkey sign-in (for tests).
    pub fn set_cable_scanner(
        &mut self,
        scanner: Arc<dyn tg_platform::cable_scanner::CableBleScanner>,
    ) {
        self.cable_scanner.stop();
        self.cable_scanner = scanner;
    }

    /// Returns the live caBLEv2 BLE proximity status for an active passkey hybrid sign-in.
    #[must_use]
    pub fn passkey_hybrid_login_status(
        &self,
    ) -> tg_app_core::settings::backend::PasskeyHybridQrStatus {
        self.passkey_cable_status
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Returns the matched caBLEv2 BLE EID if a nearby phone responded to the hybrid login QR.
    #[must_use]
    pub fn matched_passkey_cable_eid(&self) -> Option<tg_platform::cable_scanner::MatchedCableEid> {
        self.passkey_matched_eid.lock().ok().and_then(|g| g.clone())
    }

    /// Starts cross-device caBLE / hybrid QR passkey sign-in (`auth.initPasskeyLogin` ->
    /// `WebAuthnProvider::start_cable_login` -> `start_cable_proximity_match`).
    pub fn start_passkey_hybrid_login(&mut self, now: Instant) -> Result<String, ApiError> {
        self.error.clear();
        self.cancel_passkey_hybrid_login();
        let (login_data, _initial_dc) = if let Some((ref cached, cached_at, dc)) =
            self.passkey_login_cache
            && now.duration_since(cached_at)
                < std::time::Duration::from_millis(u64::from(cached.timeout_ms))
        {
            (cached.clone(), dc)
        } else {
            self.passkey_login_cache = None;
            let fetched = self.runtime.block_on(passkey_rpc::init_passkey_login_async(
                &*self.invoker.0,
                &self.credentials,
            ));
            match fetched {
                Ok((data, dc)) => {
                    self.passkey_login_cache = Some((data.clone(), now, dc));
                    (data, dc)
                }
                Err(err) => {
                    self.handle_api_error(&err, now);
                    return Err(err);
                }
            }
        };

        let ceremony = match self.passkey_provider.start_cable_login(&login_data) {
            Ok(c) => c,
            Err(err) => {
                self.error = err.to_string();
                return Err(ApiError::ServerError(self.error.clone()));
            }
        };
        let uri = ceremony.session.qr_uri().to_owned();
        let eid_key = *ceremony.session.eid_key();
        let generation = self
            .passkey_cable_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .wrapping_add(1);
        if let Ok(mut guard) = self.active_passkey_cable.lock() {
            *guard = Some(ceremony);
        }
        if let Ok(mut st) = self.passkey_cable_status.lock() {
            *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Scanning;
        }
        if let Ok(mut m) = self.passkey_matched_eid.lock() {
            *m = None;
        }
        if let Ok(mut r) = self.passkey_hybrid_login_result.lock() {
            *r = None;
        }

        let status_for_avail = Arc::clone(&self.passkey_cable_status);
        let gen_for_avail = Arc::clone(&self.passkey_cable_generation);
        let on_availability: tg_platform::cable_scanner::AvailabilityCallback =
            Arc::new(move |available| {
                if !available
                    && gen_for_avail.load(std::sync::atomic::Ordering::SeqCst) == generation
                    && let Ok(mut st) = status_for_avail.lock()
                    && matches!(
                        *st,
                        tg_app_core::settings::backend::PasskeyHybridQrStatus::Scanning
                    )
                {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::NoBluetooth;
                }
            });

        let status_for_match = Arc::clone(&self.passkey_cable_status);
        let eid_for_match = Arc::clone(&self.passkey_matched_eid);
        let cable_for_match = Arc::clone(&self.active_passkey_cable);
        let result_for_match = Arc::clone(&self.passkey_hybrid_login_result);
        let gen_for_match = Arc::clone(&self.passkey_cable_generation);
        let tunnel_for_match = Arc::clone(&self.cable_tunnel);
        let rt_handle = self.runtime.handle().clone();
        let login_data_for_match = login_data.clone();
        match tg_platform::cable_scanner::start_cable_proximity_match(
            &self.cable_scanner,
            eid_key,
            true,
            move |matched| {
                if gen_for_match.load(std::sync::atomic::Ordering::SeqCst) != generation {
                    return;
                }
                if let Ok(mut st) = status_for_match.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Connecting {
                        tunnel_domain: matched.tunnel_domain.clone(),
                    };
                }
                if let Ok(mut m) = eid_for_match.lock() {
                    *m = Some(matched.clone());
                }

                let accepted = cable_for_match.lock().ok().and_then(|mut guard| {
                    let ceremony = guard.as_mut()?;
                    let (domain, path, initiator) = ceremony
                        .session
                        .accept_ble_advert(&matched.raw_service_data)?;
                    Some((
                        tg_platform::cable_tunnel::CableTunnelEndpoint::new(domain, path),
                        initiator,
                        ceremony.client_data_json.clone(),
                        ceremony.client_data_hash,
                    ))
                });
                let Some((endpoint, initiator, client_data_json, client_data_hash)) = accepted
                else {
                    if gen_for_match.load(std::sync::atomic::Ordering::SeqCst) == generation
                        && let Ok(mut st) = status_for_match.lock()
                    {
                        *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                            "BLE advertisement did not match active caBLE QR session".to_owned(),
                        );
                    }
                    return;
                };

                let status_task = Arc::clone(&status_for_match);
                let result_task = Arc::clone(&result_for_match);
                let gen_task = Arc::clone(&gen_for_match);
                let tunnel_task = Arc::clone(&tunnel_for_match);
                let req_data = login_data_for_match.clone();
                let timeout =
                    std::time::Duration::from_millis(u64::from(req_data.timeout_ms.max(5_000)));
                rt_handle.spawn_blocking(move || {
                    let res = tg_platform::cable_tunnel::CableTunnelClient::new(tunnel_task)
                        .run_login(
                            &endpoint,
                            initiator,
                            client_data_json,
                            client_data_hash,
                            &req_data,
                            timeout,
                        );
                    if gen_task.load(std::sync::atomic::Ordering::SeqCst) != generation {
                        return;
                    }
                    match res {
                        Ok(outcome) => {
                            if let Ok(mut slot) = result_task.lock() {
                                *slot = Some(Ok(outcome.result));
                            }
                        }
                        Err(err) => {
                            let msg = err.to_string();
                            if let Ok(mut st) = status_task.lock() {
                                *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                                    msg.clone(),
                                );
                            }
                            if let Ok(mut slot) = result_task.lock() {
                                *slot = Some(Err(msg));
                            }
                        }
                    }
                });
            },
            on_availability,
        ) {
            Ok(true) => {}
            Ok(false) => {
                if let Ok(mut st) = self.passkey_cable_status.lock()
                    && matches!(
                        *st,
                        tg_app_core::settings::backend::PasskeyHybridQrStatus::Scanning
                    )
                {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::NoBluetooth;
                }
            }
            Err(err) => {
                if let Ok(mut st) = self.passkey_cable_status.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                        err.to_string(),
                    );
                }
            }
        }

        self.qr.hybrid_qr_uri = Some(uri.clone());
        self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
        Ok(uri)
    }

    /// Replaces the active caBLEv2 tunnel connector (primarily for tests using
    /// `FakeCableTunnelConnector`).
    pub fn set_cable_tunnel(
        &mut self,
        tunnel: Arc<dyn tg_platform::cable_tunnel::CableTunnelConnector>,
    ) {
        self.cable_tunnel = tunnel;
    }

    /// Returns the active hybrid login session's 64-byte EID key (for deterministic tests).
    #[must_use]
    pub fn active_passkey_cable_eid_key(
        &self,
    ) -> Option<[u8; tg_platform::webauthn::CABLE_EID_KEY_LEN]> {
        self.active_passkey_cable
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| *c.session.eid_key()))
    }

    /// Returns the active hybrid login session's 16-byte QR secret (for deterministic tests).
    #[must_use]
    pub fn active_passkey_cable_qr_secret(
        &self,
    ) -> Option<[u8; tg_platform::webauthn::CABLE_QR_SECRET_LEN]> {
        self.active_passkey_cable
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| *c.session.qr_secret()))
    }

    /// Returns the active hybrid login session's 65-byte uncompressed P-256 identity key.
    #[must_use]
    pub fn active_passkey_cable_identity_public_x962(
        &self,
    ) -> Option<[u8; tg_platform::webauthn::P256_X962_LEN]> {
        self.active_passkey_cable
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.session.identity_public_x962()))
    }

    /// Polls for completion of an active cross-device caBLEv2 hybrid passkey login tunnel and
    /// submits `auth.finishPasskeyLogin` when an assertion has arrived.
    ///
    /// Returns `Ok(true)` if the ceremony finished authorization (or entered 2FA), `Ok(false)` if
    /// still in progress or idle, or `Err` if the tunnel or RPC failed.
    pub fn poll_passkey_hybrid_login(&mut self, now: Instant) -> Result<bool, ApiError> {
        let ready = self
            .passkey_hybrid_login_result
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        let Some(outcome) = ready else {
            return Ok(false);
        };
        let assertion = match outcome {
            Ok(assertion) => assertion,
            Err(msg) => {
                self.error = msg.clone();
                if let Ok(mut st) = self.passkey_cable_status.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(msg);
                }
                self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                return Err(ApiError::ServerError(self.error.clone()));
            }
        };

        let initial_dc = self
            .passkey_login_cache
            .as_ref()
            .map_or(self.invoker.0.main_dc_id(), |&(_, _, dc)| dc);
        let finish = self
            .runtime
            .block_on(passkey_rpc::finish_passkey_login_async(
                &*self.invoker.0,
                initial_dc,
                None,
                &assertion,
            ));
        match finish {
            Ok(tl_enums::auth::Authorization::Authorization(auth)) => {
                let tl_enums::User::User(user) = auth.user else {
                    self.error = "PASSKEY_USER_EMPTY".to_owned();
                    if let Ok(mut st) = self.passkey_cable_status.lock() {
                        *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                            self.error.clone(),
                        );
                    }
                    self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                    return Err(ApiError::ServerError(self.error.clone()));
                };
                self.passkey_login_cache = None;
                self.passkey_auth = Some(PasskeyAuthState::Authorized(Box::new(user)));
                if let Ok(mut g) = self.active_passkey_cable.lock() {
                    *g = None;
                }
                if let Ok(mut st) = self.passkey_cable_status.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Completed;
                }
                self.qr.hybrid_qr_uri = None;
                self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                Ok(true)
            }
            Ok(tl_enums::auth::Authorization::SignUpRequired(_)) => {
                self.passkey_login_cache = None;
                self.error = "PASSKEY_SIGNUP_REQUIRED".to_owned();
                if let Ok(mut st) = self.passkey_cable_status.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                        self.error.clone(),
                    );
                }
                self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                Err(ApiError::ServerError(self.error.clone()))
            }
            Err(ApiError::Login(LoginErrorKind::SessionPasswordNeeded)) => {
                self.passkey_login_cache = None;
                let pw_res = self.runtime.block_on(self.invoker.0.account_get_password());
                match pw_res {
                    Ok(pw) => {
                        let state = tg_api::login::CloudPasswordState::from_account_password(&pw);
                        self.password.reset();
                        self.passkey_auth = Some(PasskeyAuthState::PasswordCheck(Box::new(state)));
                        if let Ok(mut st) = self.passkey_cable_status.lock() {
                            *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Completed;
                        }
                        self.qr.hybrid_qr_uri = None;
                        self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                        Ok(true)
                    }
                    Err(inv_err) => {
                        let api_err = ApiError::Invocation(inv_err);
                        self.handle_api_error(&api_err, now);
                        if let Ok(mut st) = self.passkey_cable_status.lock() {
                            *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                                self.error.clone(),
                            );
                        }
                        self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                        Err(api_err)
                    }
                }
            }
            Err(err) => {
                self.passkey_login_cache = None;
                self.handle_api_error(&err, now);
                if let Ok(mut st) = self.passkey_cable_status.lock() {
                    *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Error(
                        self.error.clone(),
                    );
                }
                self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                Err(err)
            }
        }
    }

    /// Cancels any active passkey hybrid QR login ceremony and stops the BLE scanner.
    pub fn cancel_passkey_hybrid_login(&mut self) {
        self.passkey_cable_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.cable_scanner.stop();
        if let Ok(mut guard) = self.active_passkey_cable.lock() {
            *guard = None;
        }
        if let Ok(mut st) = self.passkey_cable_status.lock() {
            *st = tg_app_core::settings::backend::PasskeyHybridQrStatus::Idle;
        }
        if let Ok(mut m) = self.passkey_matched_eid.lock() {
            *m = None;
        }
        if let Ok(mut r) = self.passkey_hybrid_login_result.lock() {
            *r = None;
        }
        self.qr.hybrid_qr_uri = None;
        self.qr.hybrid_qr_status = tg_app_core::settings::backend::PasskeyHybridQrStatus::Idle;
    }

    /// Sets the parent main-window Win32 `HWND` passed to native WebAuthn login dialogs.
    pub fn set_passkey_window_hwnd(&mut self, hwnd: usize) {
        self.passkey_window_hwnd = hwnd;
    }

    /// Returns the authorized user produced by passkey login (including post-passkey 2FA), if any.
    #[must_use]
    pub fn passkey_authorized_user(&self) -> Option<&tl_types::User> {
        match &self.passkey_auth {
            Some(PasskeyAuthState::Authorized(user)) => Some(user.as_ref()),
            _ => None,
        }
    }

    /// Returns `true` when passkey login completed with `SESSION_PASSWORD_NEEDED` and is awaiting 2FA.
    #[must_use]
    pub fn is_passkey_password_check(&self) -> bool {
        matches!(self.passkey_auth, Some(PasskeyAuthState::PasswordCheck(_)))
    }

    /// Executes passkey sign-in (`auth.initPasskeyLogin` -> WebAuthn assertion -> `auth.finishPasskeyLogin`).
    pub fn login_with_passkey(&mut self, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let (login_data, initial_dc) = if let Some((ref cached, cached_at, dc)) =
            self.passkey_login_cache
            && now.duration_since(cached_at)
                < std::time::Duration::from_millis(u64::from(cached.timeout_ms))
        {
            (cached.clone(), dc)
        } else {
            self.passkey_login_cache = None;
            let fetched = self.runtime.block_on(passkey_rpc::init_passkey_login_async(
                &*self.invoker.0,
                &self.credentials,
            ));
            match fetched {
                Ok((data, dc)) => {
                    self.passkey_login_cache = Some((data.clone(), now, dc));
                    (data, dc)
                }
                Err(err) => {
                    self.handle_api_error(&err, now);
                    return Err(err);
                }
            }
        };

        let assertion = match self
            .passkey_provider
            .get_assertion(self.passkey_window_hwnd, &login_data)
        {
            Ok(res) => res,
            Err(tg_platform::webauthn::WebAuthnErrorKind::Cancelled) => return Ok(()),
            Err(tg_platform::webauthn::WebAuthnErrorKind::UnsignedBuild) => {
                self.error = tr::lng_settings_passkeys_unsigned_error();
                return Err(ApiError::ServerError(self.error.clone()));
            }
            Err(err) => {
                self.error = err.to_string();
                return Err(ApiError::ServerError(self.error.clone()));
            }
        };

        let finish = self
            .runtime
            .block_on(passkey_rpc::finish_passkey_login_async(
                &*self.invoker.0,
                initial_dc,
                None,
                &assertion,
            ));
        match finish {
            Ok(tl_enums::auth::Authorization::Authorization(auth)) => {
                let tl_enums::User::User(user) = auth.user else {
                    self.error = "PASSKEY_USER_EMPTY".to_owned();
                    return Err(ApiError::ServerError(self.error.clone()));
                };
                self.passkey_login_cache = None;
                self.passkey_auth = Some(PasskeyAuthState::Authorized(Box::new(user)));
                Ok(())
            }
            Ok(tl_enums::auth::Authorization::SignUpRequired(_)) => {
                self.passkey_login_cache = None;
                self.error = "PASSKEY_SIGNUP_REQUIRED".to_owned();
                Err(ApiError::ServerError(self.error.clone()))
            }
            Err(ApiError::Login(LoginErrorKind::SessionPasswordNeeded)) => {
                self.passkey_login_cache = None;
                let pw_res = self.runtime.block_on(self.invoker.0.account_get_password());
                match pw_res {
                    Ok(pw) => {
                        let state = tg_api::login::CloudPasswordState::from_account_password(&pw);
                        self.password.reset();
                        self.passkey_auth = Some(PasskeyAuthState::PasswordCheck(Box::new(state)));
                        Ok(())
                    }
                    Err(inv_err) => {
                        let api_err = ApiError::Invocation(inv_err);
                        self.handle_api_error(&api_err, now);
                        Err(api_err)
                    }
                }
            }
            Err(err) => {
                self.passkey_login_cache = None;
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Returns the active visual step derived directly from `LoginStateMachine::step()`.
    #[must_use]
    pub fn current_step(&self) -> IntroStep {
        IntroStep::from_login_step(self.login.step())
    }

    /// Returns the current step stack depth (`step_stack_len + 1`).
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.login.step_stack_len() + 1
    }

    /// Returns `true` when `LoginStateMachine`'s `step_stack` has a previous step to pop.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.login.step_stack_len() > 0 && self.current_step().has_back()
    }

    /// Resets the intro wizard back to the welcome screen (e.g. after logout).
    pub fn reset(&mut self) {
        self.login = LoginStateMachine::new(self.credentials.clone(), "US");
        self.stack_kinds.clear();
        self.slide_animation = None;
        self.cover_transition = None;
        self.code = CodeStepState::new();
        self.password.reset();
        self.signup.reset();
        self.qr.reset();
        self.email.reset();
        self.error.clear();
        self.nearest_dc_fetched = false;
        self.cancel_passkey_hybrid_login();
        self.passkey_login_cache = None;
        self.passkey_auth = None;
    }

    /// Pops one step from `tg-api`'s [`LoginStateMachine`] navigation stack (`Intro::Widget::goBack`).
    pub fn go_back(&mut self) -> bool {
        if self.login.step_stack_len() == 0 {
            return false;
        }
        let leaving = self.current_step();
        let now = tg_app_core::clock::now();

        // Cancel in-flight phone code on the server when stepping back from Code or EmailSetup,
        // without calling `LoginStateMachine::cancel_code` (which would clear `step_stack`).
        match self.login.step() {
            LoginStep::CodeInput(state) => {
                let req = tl_fns::auth::CancelCode {
                    phone_number: state.phone.clone(),
                    phone_code_hash: state.phone_hash.clone(),
                };
                let _ = self.runtime.block_on(self.invoker.auth_cancel_code(req));
            }
            LoginStep::EmailSetup { phone, phone_hash } => {
                let req = tl_fns::auth::CancelCode {
                    phone_number: phone.clone(),
                    phone_code_hash: phone_hash.clone(),
                };
                let _ = self.runtime.block_on(self.invoker.auth_cancel_code(req));
            }
            _ => {}
        }

        if !self.login.go_back() {
            return false;
        }
        self.error.clear();
        self.sync_after_transition(leaving, Animate::Back, now);
        true
    }

    /// Returns `true` if a step slide or cover transition animation is currently running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        let now = tg_app_core::clock::now();
        self.slide_animation
            .as_ref()
            .is_some_and(|anim| !anim.is_finished(now))
            || self
                .cover_transition
                .as_ref()
                .is_some_and(|cov| !cov.is_finished(now))
    }

    /// Fetches `help.getNearestDc` once on startup (`Intro::Widget::getNearestDC`).
    pub fn fetch_nearest_dc(&mut self) {
        if self.nearest_dc_fetched {
            return;
        }
        self.nearest_dc_fetched = true;
        if let Ok(nearest) = self
            .runtime
            .block_on(self.login.fetch_nearest_dc(&self.invoker))
        {
            self.phone.apply_suggested_country(&nearest.country);
        }
    }

    /// Sends `help.getNearestDc` once without blocking the frame, like tdesktop's
    /// `Intro::Widget::getNearestDC` (the first request also opens the connection and creates the
    /// auth key, which takes seconds). The reply is applied by [`Self::poll_nearest_dc`].
    /// The offline fake client keeps the synchronous path so tests stay deterministic.
    fn request_nearest_dc(&mut self, ctx: &egui::Context) {
        if self.nearest_dc_fetched {
            return;
        }
        if self.fake_rpc.is_some() {
            self.fetch_nearest_dc();
            return;
        }
        self.nearest_dc_fetched = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let rpc = Arc::clone(&self.invoker.0);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("intro-nearest-dc".to_owned())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let res = rt.block_on(rpc.help_get_nearest_dc());
                let _ = tx.send(res);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.nearest_dc_rx = Some(rx);
        }
    }

    /// Applies a finished background `help.getNearestDc` reply: the suggested phone country, and
    /// the main DC while the user has not started a login request yet.
    fn poll_nearest_dc(&mut self) {
        let Some(rx) = self.nearest_dc_rx.as_ref() else {
            return;
        };
        let res = match rx.try_recv() {
            Ok(res) => res,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.nearest_dc_rx = None;
                return;
            }
        };
        self.nearest_dc_rx = None;
        if let Ok(nearest) = res {
            if self.login.apply_nearest_dc(&nearest) {
                self.invoker.set_main_dc_id(nearest.nearest_dc);
            }
            self.phone.apply_suggested_country(&nearest.country);
        }
    }

    /// Transitions from Start (or Phone) -> QR login (`StartWidget::submit` -> `goNext<QrWidget>`).
    pub fn start_qr_flow(&mut self, now: Instant) -> Result<(), ApiError> {
        let prev = self.current_step();
        self.qr.reset();
        let now_unix = current_unix_time();
        let res = self
            .runtime
            .block_on(
                self.login
                    .start_qr_login(&self.invoker, now_unix, Vec::new()),
            )
            .map(|_| ());
        self.sync_after_transition(prev, Animate::Forward, now);
        match res {
            Ok(()) => Ok(()),
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Navigates between QR and Phone steps (`QrWidget::submit` / `PhoneWidget::setupQrLogin`).
    pub fn switch_to_phone(&mut self) {
        if self.stack_kinds.last() == Some(&IntroStep::Phone) {
            self.go_back();
            return;
        }
        let prev = self.current_step();
        self.login.enter_phone_input();
        self.error.clear();
        self.sync_after_transition(prev, Animate::Forward, tg_app_core::clock::now());
    }

    /// Navigates from Phone to QR step (`goNextOrBack<QrWidget>`).
    pub fn switch_to_qr(&mut self, now: Instant) {
        if self.stack_kinds.last() == Some(&IntroStep::Qr) {
            self.go_back();
            if self.qr.is_refresh_due(now) {
                let _ = self.refresh_qr_token(now);
            }
        } else {
            let _ = self.start_qr_flow(now);
        }
    }

    /// Submits a phone number through `LoginStateMachine::submit_phone`.
    pub fn submit_phone(&mut self, phone: &str, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let res = self
            .runtime
            .block_on(self.login.submit_phone(&self.invoker, phone));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Submits a verification code in `CodeInput` or `EmailVerify`.
    pub fn submit_code(&mut self, code: &str, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let res = self
            .runtime
            .block_on(self.login.submit_code(&self.invoker, code));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                if self.current_step() != prev {
                    self.sync_after_transition(prev, Animate::Back, now);
                }
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Resends verification code via SMS (`CodeWidget::noTelegramCode`).
    pub fn resend_code(&mut self, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let res = self.runtime.block_on(self.login.resend_code(&self.invoker));
        match res {
            Ok(_) => {
                if let LoginStep::CodeInput(state) = self.login.step() {
                    self.code.prepare(state.code_length, now);
                }
                Ok(())
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Advances the 1-second phone-call countdown timer (`CodeWidget::sendCall`).
    pub fn tick_call_countdown(&mut self, now: Instant) -> Result<CallStatus, ApiError> {
        let res = self
            .runtime
            .block_on(self.login.tick_call_countdown(&self.invoker));
        if let Err(ref err) = res {
            self.handle_api_error(err, now);
        }
        res
    }

    /// Submits the 2FA cloud password via `LoginStateMachine::submit_password` (`tg-crypto::srp`).
    pub fn submit_password(&mut self, password: &str, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        if let Some(PasskeyAuthState::PasswordCheck(mut pwd_state)) = self.passkey_auth.take() {
            let Some(algo) = pwd_state.current_algo.clone() else {
                let err = ApiError::Login(LoginErrorKind::OutdatedClient);
                self.handle_api_error(&err, now);
                return Err(err);
            };
            let hash = tg_crypto::srp::compute_hash(&algo, password.as_bytes());
            let check = tg_crypto::srp::compute_check_with_hash(
                &algo,
                &pwd_state.srp_b,
                pwd_state.srp_id,
                &hash,
            )
            .map_err(ApiError::from)?;
            pwd_state.srp_id = 0;
            let tl_check = tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
                tl_types::InputCheckPasswordSRP {
                    srp_id: check.srp_id,
                    a: check.a.to_vec(),
                    m1: check.m1.to_vec(),
                },
            );
            let res = self.runtime.block_on(
                self.invoker
                    .0
                    .auth_check_password(tl_fns::auth::CheckPassword { password: tl_check }),
            );
            match res {
                Ok(tl_enums::auth::Authorization::Authorization(auth)) => {
                    if let tl_enums::User::User(user) = auth.user {
                        self.passkey_auth = Some(PasskeyAuthState::Authorized(Box::new(user)));
                        return Ok(());
                    }
                    self.passkey_auth = Some(PasskeyAuthState::PasswordCheck(pwd_state));
                    return Err(ApiError::ServerError("PASSKEY_USER_EMPTY".to_owned()));
                }
                Ok(tl_enums::auth::Authorization::SignUpRequired(_)) => {
                    return Err(ApiError::ServerError("PASSKEY_SIGNUP_REQUIRED".to_owned()));
                }
                Err(InvocationError::Rpc(rpc)) => {
                    self.passkey_auth = Some(PasskeyAuthState::PasswordCheck(pwd_state));
                    let err = ApiError::Login(tg_api::error::map_login_error(&rpc));
                    self.handle_api_error(&err, now);
                    return Err(err);
                }
                Err(other) => {
                    self.passkey_auth = Some(PasskeyAuthState::PasswordCheck(pwd_state));
                    let err = ApiError::Invocation(other);
                    self.handle_api_error(&err, now);
                    return Err(err);
                }
            }
        }
        let prev = self.current_step();
        let res = self
            .runtime
            .block_on(self.login.submit_password(&self.invoker, password, now));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                if self.current_step() != prev {
                    self.sync_after_transition(prev, Animate::Back, now);
                }
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Initiates 2FA password recovery email (`PasswordCheckWidget::toRecover`).
    pub fn request_password_recovery(&mut self, now: Instant) -> Result<String, ApiError> {
        self.error.clear();
        let res = self
            .runtime
            .block_on(self.login.request_password_recovery(&self.invoker));
        if let Err(ref err) = res {
            self.handle_api_error(err, now);
        } else {
            self.password.request_focus = true;
        }
        res
    }

    /// Verifies the 2FA email recovery code and immediately finalizes recovery when valid.
    pub fn submit_recovery_code(&mut self, code: &str, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let checked = self
            .runtime
            .block_on(self.login.check_recovery_code(&self.invoker, code));
        match checked {
            Ok(_) => self.complete_password_recovery(now),
            Err(err) => {
                if self.current_step() != prev {
                    self.sync_after_transition(prev, Animate::Back, now);
                }
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Completes 2FA password recovery via `auth.recoverPassword`.
    pub fn complete_password_recovery(&mut self, now: Instant) -> Result<(), ApiError> {
        let prev = self.current_step();
        let res = self
            .runtime
            .block_on(self.login.complete_password_recovery(&self.invoker));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Resets the account after forgetting the 2FA password (`Intro::Widget::resetAccount`).
    pub fn reset_account(&mut self, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let phone_for_msg = match self.login.step() {
            LoginStep::PasswordCheck { phone, .. } => self.phone.format_phone_display(phone),
            _ => String::new(),
        };
        let res = self
            .runtime
            .block_on(self.login.reset_account(&self.invoker));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(ApiError::Login(LoginErrorKind::TwoFaConfirmWait { seconds })) => {
                let msg = format_reset_confirm_wait(&phone_for_msg, seconds);
                self.password.modal = Some(PasswordModal::ResetNotice(msg));
                Err(ApiError::Login(LoginErrorKind::TwoFaConfirmWait {
                    seconds,
                }))
            }
            Err(ApiError::Login(LoginErrorKind::TwoFaRecentConfirm)) => {
                self.password.modal =
                    Some(PasswordModal::ResetNotice(tr::lng_signin_reset_cancelled()));
                Err(ApiError::Login(LoginErrorKind::TwoFaRecentConfirm))
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Submits a login email address in `LoginStep::EmailSetup`.
    pub fn submit_email_setup(&mut self, email: &str, now: Instant) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let res = self
            .runtime
            .block_on(self.login.submit_email_setup(&self.invoker, email));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Marks Terms of Service accepted and submits SignUp.
    pub fn accept_terms_and_submit_signup(&mut self, now: Instant) -> Result<(), ApiError> {
        let _ = self.login.accept_terms();
        let first = self.signup.first_name.clone();
        let last = self.signup.last_name.clone();
        self.submit_signup(&first, &last, now)
    }

    /// Submits `auth.signUp` with `(first_name, last_name)`.
    pub fn submit_signup(
        &mut self,
        first_name: &str,
        last_name: &str,
        now: Instant,
    ) -> Result<(), ApiError> {
        self.error.clear();
        let prev = self.current_step();
        let res = self.runtime.block_on(self.login.submit_sign_up(
            &self.invoker,
            first_name,
            last_name,
        ));
        match res {
            Ok(_) => {
                self.sync_after_transition(prev, Animate::Forward, now);
                Ok(())
            }
            Err(err) => {
                if self.current_step() != prev {
                    self.sync_after_transition(prev, Animate::Back, now);
                }
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Refreshes the QR login token (`LoginStateMachine::refresh_qr_token`).
    pub fn refresh_qr_token(&mut self, now: Instant) -> Result<(), ApiError> {
        let prev = self.current_step();
        let now_unix = current_unix_time();
        let res = self
            .runtime
            .block_on(self.login.refresh_qr_token(&self.invoker, now_unix));
        match res {
            Ok(_) => {
                if let LoginStep::QrLogin(qr) = self.login.step() {
                    self.qr.schedule_refresh(qr.refresh_after_secs, now);
                } else {
                    self.sync_after_transition(prev, Animate::Forward, now);
                }
                Ok(())
            }
            Err(err) => {
                self.handle_api_error(&err, now);
                Err(err)
            }
        }
    }

    /// Polls any queued live `Updates` (e.g. `Update::LoginToken`) while on `LoginStep::QrLogin`.
    pub fn poll_qr_updates(&mut self, now: Instant) {
        let mut batch = Vec::new();
        if let Some(rx) = &mut self.updates_rx {
            while let Ok(upd) = rx.try_recv() {
                batch.push(upd);
            }
        }
        if batch.is_empty() || !matches!(self.login.step(), LoginStep::QrLogin(_)) {
            return;
        }
        let now_unix = current_unix_time();
        for upd in batch {
            let prev = self.current_step();
            match self
                .runtime
                .block_on(self.login.on_qr_updates(&self.invoker, &upd, now_unix))
            {
                Ok(true) => {
                    if let LoginStep::QrLogin(qr) = self.login.step() {
                        self.qr.schedule_refresh(qr.refresh_after_secs, now);
                    } else {
                        self.sync_after_transition(prev, Animate::Forward, now);
                        break;
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    self.handle_api_error(&err, now);
                }
            }
        }
    }

    /// Renders the active intro step and processes user interactions.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn Backend,
        palette: &Palette,
        _strings: &dyn Strings,
    ) -> Option<IntroAction> {
        self.request_nearest_dc(ui.ctx());
        self.poll_nearest_dc();
        self.request_language_suggestion(Some(ui.ctx()));
        self.poll_language_link();

        let mut emitted = None;
        let now = tg_app_core::clock::now();
        self.poll_qr_updates(now);

        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, palette.window_bg);

        // Evaluate slide animation (`Intro::Step::_slideAnimation`)
        let slide_offset = if let Some(anim) = &self.slide_animation {
            if anim.is_finished(now) {
                self.slide_animation = None;
                0.0
            } else {
                ui.ctx().request_repaint();
                anim.incoming_offset(now, 160.0)
            }
        } else {
            0.0
        };

        // Evaluate cover animation (`Intro::Widget::_coverShownAnimation`)
        let step = self.current_step();
        let cover_vis = if let Some(cov) = &self.cover_transition {
            let v = cov.visibility(now);
            if cov.is_finished(now) {
                self.cover_transition = None;
                if step.has_cover() { 1.0 } else { 0.0 }
            } else {
                ui.ctx().request_repaint();
                v
            }
        } else if step.has_cover() {
            1.0
        } else {
            0.0
        };

        // Paint top welcome cover when visible (`Step::paintCover`).
        if cover_vis > 0.001 {
            paint_intro_cover(ui, rect, cover_vis, &mut self.icon_cache);
        }

        // Escape navigates back when no modal is open (`Intro::Widget::keyPressEvent`).
        let modal_open = self.phone.picker_open
            || self.phone.banned_modal.is_some()
            || self.phone.banned_box.is_some()
            || self.phone.phone_flood_modal.is_some()
            || self.password.modal.is_some()
            || self.signup.photo_picker_open
            || self.signup.terms_modal != crate::intro::signup::TermsModalState::Closed
            || self.unconfirmed_auth_denied.is_some();
        let can_back = self.can_go_back();
        if can_back && !modal_open && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.go_back();
            return None;
        }

        // Top controls bar: Back on the left, Settings on the right (`introSettingsSkip = 10px`).
        let controls_top = INTRO_COVER_HEIGHT * cover_vis;
        let top_bar_rect = egui::Rect::from_min_size(
            Pos2::new(rect.left(), rect.top() + controls_top),
            Vec2::new(rect.width(), 44.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(top_bar_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if can_back {
                            let back_btn = ui.add(
                                egui::Button::new(
                                    RichText::new(format!("\u{2190}  {}", tr::lng_go_back()))
                                        .size(13.5)
                                        .color(palette.window_active_text_fg),
                                )
                                .frame(false),
                            );
                            if back_btn.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if back_btn.clicked() {
                                self.go_back();
                            }
                        }

                        if !step.has_cover() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let settings_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new(tr::lng_menu_settings().to_uppercase())
                                            .size(12.5)
                                            .strong()
                                            .color(palette.window_active_text_fg),
                                    )
                                    .frame(false),
                                );
                                if settings_btn.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if settings_btn.clicked() {
                                    emitted = Some(IntroAction::OpenSettings);
                                }
                            });
                        }
                    });
                });
        });

        // Centered step card (`st::introStepWidth = 380px`).
        let card_width = INTRO_STEP_WIDTH.min(rect.width() - 40.0).max(260.0);
        let top_margin = if step.has_cover() {
            (INTRO_COVER_HEIGHT * cover_vis + 26.0).max(40.0)
        } else if step == IntroStep::Qr {
            ((rect.height() - 420.0) * 0.45).max(54.0)
        } else {
            ((rect.height() - 360.0) * 0.38).max(76.0)
        };
        let center_x = rect.center().x + slide_offset;
        let card_rect = egui::Rect::from_min_size(
            Pos2::new(center_x - card_width * 0.5, rect.top() + top_margin),
            Vec2::new(card_width, (rect.height() - top_margin - 20.0).max(260.0)),
        );

        let inner_width = if step.has_cover() {
            card_width
        } else {
            card_width.min(300.0)
        };
        let inner_rect = egui::Rect::from_min_size(
            Pos2::new(card_rect.center().x - inner_width * 0.5, card_rect.top()),
            Vec2::new(inner_width, card_rect.height()),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
            ui.set_width(inner_width);
            self.render_active_step(ui, inner_width, now, palette);
        });

        // Unconfirmed auth denied security modal (`UnconfirmedAuthDeniedBox`)
        if let Some(ref mut denied) = self.unconfirmed_auth_denied
            && denied.show_modal(ui.ctx(), palette)
        {
            self.unconfirmed_auth_denied = None;
        }

        // If `LoginStateMachine` (or passkey auth) reached `Authorized`, emit `IntroAction::SignedIn`.
        if let Some(PasskeyAuthState::Authorized(user)) = &self.passkey_auth {
            let auth_user = tl_user_to_auth_user(user, &self.phone);
            ensure_mock_backend_signed_in(backend);
            emitted = Some(IntroAction::SignedIn(auth_user));
        } else if let LoginStep::Authorized { user, .. } = self.login.step().clone() {
            let auth_user = tl_user_to_auth_user(&user, &self.phone);
            ensure_mock_backend_signed_in(backend);
            emitted = Some(IntroAction::SignedIn(auth_user));
        }

        emitted
    }

    fn render_active_step(&mut self, ui: &mut Ui, width: f32, now: Instant, palette: &Palette) {
        if let Some(PasskeyAuthState::PasswordCheck(password_state)) = &self.passkey_auth {
            let password_state = password_state.clone();
            let err = self.error.clone();
            if let Some(PasswordStepAction::SubmitPassword(pwd)) = self.password.show(
                ui,
                width,
                &password_state,
                &tg_api::login::PasswordCheckMode::Password,
                &err,
                now,
                palette,
            ) {
                let _ = self.submit_password(&pwd, now);
            }
            return;
        }
        match self.login.step().clone() {
            LoginStep::Start { .. } => {
                if let Some(act) = self.start.show(ui, width, palette) {
                    match act {
                        StartStepAction::StartMessaging => {
                            let _ = self.start_qr_flow(now);
                        }
                        StartStepAction::ToggleLanguage => {
                            self.click_language_link(Some(ui.ctx()));
                        }
                    }
                }
            }
            LoginStep::QrLogin(qr_state) => {
                let _ = self.poll_passkey_hybrid_login(now);
                self.qr.hybrid_qr_status = self.passkey_hybrid_login_status();
                let err = self.error.clone();
                if let Some(act) = self.qr.show(
                    ui,
                    width,
                    &qr_state,
                    &err,
                    now,
                    &mut self.icon_cache,
                    palette,
                ) {
                    match act {
                        QrStepAction::RefreshToken => {
                            let _ = self.refresh_qr_token(now);
                        }
                        QrStepAction::SwitchToPhone => {
                            self.cancel_passkey_hybrid_login();
                            self.switch_to_phone();
                        }
                        QrStepAction::LoginWithPasskey => {
                            let _ = self.login_with_passkey(now);
                        }
                        QrStepAction::LoginWithPasskeyHybridQr => {
                            let _ = self.start_passkey_hybrid_login(now);
                        }
                        QrStepAction::CancelPasskeyHybridQr => {
                            self.cancel_passkey_hybrid_login();
                        }
                    }
                }
            }
            LoginStep::PhoneInput => {
                let err = self.error.clone();
                if let Some(act) = self.phone.show(ui, width, &err, now, palette) {
                    match act {
                        PhoneStepAction::Submit(phone) => {
                            let _ = self.submit_phone(&phone, now);
                        }
                        PhoneStepAction::SwitchToQr => {
                            self.switch_to_qr(now);
                        }
                    }
                }
            }
            LoginStep::EmailSetup { .. } => {
                let err = self.error.clone();
                if let Some(EmailStepAction::Submit(email)) =
                    self.email.show(ui, width, &err, now, palette)
                {
                    let _ = self.submit_email_setup(&email, now);
                }
            }
            LoginStep::EmailVerify {
                phone,
                email_pattern,
                code_length,
                ..
            } => {
                let formatted = self.phone.format_phone_display(&phone);
                let delivery = CodeDeliveryKind::EmailSetup { email_pattern };
                let err = self.error.clone();
                if let Some(CodeStepAction::SubmitCode(code)) = self.code.show(
                    ui,
                    width,
                    &formatted,
                    &delivery,
                    code_length,
                    false,
                    CallStatus::Disabled,
                    0,
                    &err,
                    now,
                    palette,
                ) {
                    let _ = self.submit_code(&code, now);
                }
            }
            LoginStep::CodeInput(code_state) => {
                let formatted = self.phone.format_phone_display(&code_state.phone);
                let delivery = CodeDeliveryKind::from_code_state(&code_state);
                let err = self.error.clone();
                if let Some(act) = self.code.show(
                    ui,
                    width,
                    &formatted,
                    &delivery,
                    code_state.code_length,
                    code_state.code_by_telegram,
                    code_state.call_status,
                    code_state.call_timeout_secs,
                    &err,
                    now,
                    palette,
                ) {
                    match act {
                        CodeStepAction::SubmitCode(code) => {
                            let _ = self.submit_code(&code, now);
                        }
                        CodeStepAction::ResendViaSms => {
                            let _ = self.resend_code(now);
                        }
                        CodeStepAction::TickCallTimer => {
                            let _ = self.tick_call_countdown(now);
                        }
                        CodeStepAction::OpenFragment(url) => {
                            self.code.info_note = Some(url);
                        }
                    }
                }
            }
            LoginStep::PasswordCheck {
                password_state,
                mode,
                ..
            } => {
                let err = self.error.clone();
                if let Some(act) =
                    self.password
                        .show(ui, width, &password_state, &mode, &err, now, palette)
                {
                    match act {
                        PasswordStepAction::SubmitPassword(pwd) => {
                            let _ = self.submit_password(&pwd, now);
                        }
                        PasswordStepAction::RequestRecovery => {
                            let _ = self.request_password_recovery(now);
                        }
                        PasswordStepAction::SubmitRecoveryCode(code) => {
                            let _ = self.submit_recovery_code(&code, now);
                        }
                        PasswordStepAction::CompleteRecovery => {
                            let _ = self.complete_password_recovery(now);
                        }
                        PasswordStepAction::ConfirmAccountReset => {
                            let _ = self.reset_account(now);
                        }
                    }
                }
            }
            LoginStep::SignUp {
                terms_of_service,
                terms_accepted,
                ..
            } => {
                let err = self.error.clone();
                if let Some(act) = self.signup.show(
                    ui,
                    width,
                    terms_of_service.as_ref(),
                    terms_accepted,
                    &err,
                    now,
                    palette,
                ) {
                    match act {
                        SignupStepAction::AcceptTerms => {
                            let _ = self.accept_terms_and_submit_signup(now);
                        }
                        SignupStepAction::Submit {
                            first_name,
                            last_name,
                        } => {
                            let _ = self.submit_signup(&first_name, &last_name, now);
                        }
                    }
                }
            }
            LoginStep::Authorized { .. } => {}
        }
    }

    fn sync_after_transition(&mut self, prev: IntroStep, animate: Animate, now: Instant) {
        let target_stack_len = self.login.step_stack_len();
        if target_stack_len == 0 {
            self.stack_kinds.clear();
        } else if target_stack_len > self.stack_kinds.len() {
            while self.stack_kinds.len() + 1 < target_stack_len {
                self.stack_kinds.push(prev);
            }
            self.stack_kinds.push(prev);
        } else {
            self.stack_kinds.truncate(target_stack_len);
        }

        match self.login.step() {
            LoginStep::CodeInput(state) => {
                self.code.prepare(state.code_length, now);
            }
            LoginStep::EmailVerify { code_length, .. } => {
                self.code.prepare(*code_length, now);
            }
            LoginStep::PasswordCheck { .. } => {
                self.password.reset();
            }
            LoginStep::SignUp { .. } => {
                self.signup.reset();
            }
            LoginStep::EmailSetup { .. } => {
                self.email.reset();
            }
            LoginStep::QrLogin(qr) => {
                self.qr.schedule_refresh(qr.refresh_after_secs, now);
            }
            LoginStep::PhoneInput => {
                self.phone.focus_phone = true;
            }
            LoginStep::Start { .. } | LoginStep::Authorized { .. } => {}
        }

        let next = self.current_step();
        if !matches!(self.login.step(), LoginStep::Authorized { .. }) && prev != next {
            if prev.has_cover() != next.has_cover() {
                self.cover_transition = Some(CoverTransition::start(next.has_cover(), now));
                self.slide_animation = None;
            } else {
                let dir = match animate {
                    Animate::Back => SlideDirection::FromLeft,
                    Animate::Forward => SlideDirection::FromRight,
                };
                self.slide_animation = Some(SlideAnimation::start(dir));
            }
        }
    }

    fn handle_api_error(&mut self, err: &ApiError, now: Instant) {
        match err {
            ApiError::Login(LoginErrorKind::PhoneNumberBanned) => {
                let text = tr::lng_signin_banned_text();
                self.phone.banned_modal = Some(text.clone());
                let mut banned = auth_boxes::PhoneBannedBoxState::new(self.phone.full_number());
                banned.body_text = text;
                self.phone.banned_box = Some(banned);
                self.phone.trigger_error(now);
                return;
            }
            ApiError::Login(LoginErrorKind::PhoneNumberFlood) => {
                self.phone.phone_flood_modal = Some(tr::lng_error_phone_flood());
                self.phone.trigger_error(now);
                return;
            }
            ApiError::Login(kind) => {
                self.error = localized_login_error(kind);
            }
            other => {
                self.error = other.to_string();
            }
        }

        match self.current_step() {
            IntroStep::Phone => self.phone.trigger_error(now),
            IntroStep::Code | IntroStep::EmailVerify => self.code.trigger_error(now),
            IntroStep::Password => self.password.trigger_error(now),
            IntroStep::SignUp => self.signup.trigger_error(now),
            IntroStep::EmailSetup => self.email.trigger_error(now),
            IntroStep::Start | IntroStep::Qr => {}
        }
    }
}

fn current_unix_time() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1_700_000_000, |d| d.as_secs() as TimeId)
}

fn tl_user_to_auth_user(user: &tl_types::User, phone_state: &PhoneStepState) -> AuthUser {
    let first = user.first_name.as_deref().unwrap_or("");
    let name = match user.last_name.as_deref() {
        Some(last) if !last.is_empty() && !first.is_empty() => format!("{first} {last}"),
        Some(last) if !last.is_empty() => last.to_owned(),
        _ => first.to_owned(),
    };
    let mut initials = String::new();
    if let Some(c) = first.chars().next() {
        initials.extend(c.to_uppercase());
    }
    if let Some(last) = user.last_name.as_deref()
        && let Some(c) = last.chars().next()
    {
        initials.extend(c.to_uppercase());
    }
    if initials.is_empty() {
        initials = "TG".to_owned();
    }
    let phone_display = user
        .phone
        .as_deref()
        .map(|p| phone_state.format_phone_display(p))
        .unwrap_or_default();
    AuthUser {
        id: PeerId::from_user(u64::try_from(user.id).unwrap_or(0)),
        name,
        username: user
            .username
            .clone()
            .unwrap_or_else(|| "demo_account".to_owned()),
        phone: phone_display,
        bio: "Local offline Rustgram session.".to_owned(),
        initials,
        avatar_rgb: [0x40, 0xA7, 0xE3],
    }
}

fn ensure_mock_backend_signed_in(backend: &mut dyn Backend) {
    if backend.current_user().is_some() {
        return;
    }
    if backend.send_login_code("+999 661 0000").is_ok()
        && let Ok(tg_app_core::stand_in::types::CodeVerifyOutcome::PasswordRequired { .. }) =
            backend.verify_login_code("12345")
    {
        let _ = backend.verify_cloud_password("rust");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intro::code::{SegmentedCodeInput, format_call_status_text};
    use crate::intro::fake_invoker::{
        FakeQrBehavior, FakeResetBehavior, FakeRpcClient, FakeRpcConfig, TEST_2FA_HINT,
        TEST_2FA_PASSWORD, TEST_2FA_RECOVERY_CODE, TEST_2FA_RECOVERY_EMAIL, TEST_CODE_2FA,
        TEST_CODE_APP, TEST_CODE_EMAIL, TEST_CODE_EXPIRED, TEST_CODE_FLOOD, TEST_CODE_FRAGMENT,
        TEST_CODE_SIGNUP, TEST_PHONE_2FA, TEST_PHONE_APP, TEST_PHONE_BANNED,
        TEST_PHONE_EMAIL_SETUP, TEST_PHONE_FLOOD, TEST_PHONE_FRAGMENT, TEST_PHONE_INVALID,
        TEST_PHONE_SIGNUP,
    };
    use crate::intro::qr::{QrMatrix, qr_replace_elements, render_tdesktop_qr_rgba};
    use crate::intro::style::{render_tgfork_icon_rgba, shake_offset_at};
    use tg_api::login::PasswordCheckMode;
    use tg_tl::enums::auth::SentCodeType;

    #[test]
    fn step_stack_back_navigation_via_login_state_machine() {
        let now = Instant::now();
        let mut intro = IntroWidget::new();
        assert_eq!(intro.current_step(), IntroStep::Start);
        assert!(intro.current_step().has_cover());
        assert!(!intro.can_go_back());
        assert!(!intro.go_back());

        // Start -> Phone -> Code -> PasswordCheck
        intro.switch_to_phone();
        assert_eq!(intro.current_step(), IntroStep::Phone);
        assert!(!intro.current_step().has_cover());
        assert!(intro.can_go_back());
        assert_eq!(intro.history_len(), 2);

        intro
            .submit_phone(&format!("+{TEST_PHONE_2FA}"), now)
            .expect("send code");
        assert_eq!(intro.current_step(), IntroStep::Code);
        assert_eq!(intro.history_len(), 3);

        intro
            .submit_code(TEST_CODE_2FA, now)
            .expect("enter PasswordCheck");
        assert_eq!(intro.current_step(), IntroStep::Password);
        assert_eq!(intro.history_len(), 4);

        // Back pops Password -> Code -> Phone -> Start via tg-api's step_stack/go_back.
        assert!(intro.go_back());
        assert_eq!(intro.current_step(), IntroStep::Code);
        assert_eq!(intro.history_len(), 3);

        assert!(intro.go_back());
        assert_eq!(intro.current_step(), IntroStep::Phone);
        assert_eq!(intro.history_len(), 2);
        assert_eq!(intro.fake_rpc().expect("fake rpc").cancel_code_calls(), 1);

        assert!(intro.go_back());
        assert_eq!(intro.current_step(), IntroStep::Start);
        assert_eq!(intro.history_len(), 1);
        assert!(!intro.go_back());

        // QR <-> Phone toggling uses go_back instead of growing the stack unboundedly.
        intro.start_qr_flow(now).expect("start QR");
        assert_eq!(intro.current_step(), IntroStep::Qr);
        assert_eq!(intro.history_len(), 2);

        intro.switch_to_phone();
        assert_eq!(intro.current_step(), IntroStep::Phone);
        assert_eq!(intro.history_len(), 3);

        intro.switch_to_qr(now);
        assert_eq!(intro.current_step(), IntroStep::Qr);
        assert_eq!(intro.history_len(), 2);

        assert!(intro.go_back());
        assert_eq!(intro.current_step(), IntroStep::Start);
        assert_eq!(intro.history_len(), 1);
    }

    #[test]
    fn generic_arc_rpc_client_constructor_works() {
        let now = Instant::now();
        let rpc = Arc::new(FakeRpcClient::default());
        let mut intro = IntroWidget::with_rpc_client(
            ApiCredentials {
                api_id: 12345,
                api_hash: "test_hash".to_owned(),
            },
            Arc::clone(&rpc),
        );
        assert!(intro.fake_rpc().is_none());

        intro.switch_to_phone();
        intro
            .submit_phone(&format!("+{TEST_PHONE_APP}"), now)
            .expect("send code via Arc<FakeRpcClient>");
        intro
            .submit_code(TEST_CODE_APP, now)
            .expect("authorize via Arc<FakeRpcClient>");
        assert!(matches!(intro.login_step(), LoginStep::Authorized { .. }));

        // Also test QR -> 2FA -> PASSWORD_EMPTY restores QrLogin via go_back.
        rpc.set_qr_behavior(FakeQrBehavior::Pending { ttl_secs: 30 });
        intro.reset();
        intro.start_qr_flow(now).expect("start QR");
        rpc.set_qr_behavior(FakeQrBehavior::AcceptWith2Fa);
        rpc.push_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::LoginToken,
            date: 1_700_000_005,
        }));
        intro.poll_qr_updates(now);
        assert_eq!(intro.current_step(), IntroStep::Password);

        rpc.set_next_password_error(Some(RpcError::new(401, "AUTH_KEY_UNREGISTERED")));
        let err = intro.submit_password(TEST_2FA_PASSWORD, now);
        assert_eq!(
            err,
            Err(ApiError::Login(LoginErrorKind::PasswordEmptyOrUnregistered))
        );
        assert_eq!(intro.current_step(), IntroStep::Qr);
    }

    #[test]
    fn phone_country_auto_detection_search_and_formatting() {
        let mut phone = PhoneStepState::new("US");
        assert_eq!(phone.selected_iso2, "US");
        assert_eq!(phone.code_input, "+1");
        assert_eq!(phone.country_display, "USA");

        // Typing a country code auto-detects the country.
        phone.on_code_edited("+44");
        assert_eq!(phone.selected_iso2, "GB");
        assert_eq!(phone.country_display, "United Kingdom");

        // Overflowing past 4 digits splits valid code and spills remainder into phone field.
        phone.on_code_edited("+38050123");
        assert_eq!(phone.selected_iso2, "UA");
        assert_eq!(phone.code_input, "+380");
        assert!(phone.phone_input.starts_with("50"));

        // Unknown calling code shows localized invalid country code text.
        phone.on_code_edited("+9999");
        assert!(phone.selected_iso2.is_empty());
        assert_eq!(phone.country_display, tr::lng_bad_country_code());

        // Phone formatting uses tg-lang groups for US (+1 XXX XXX XXXX).
        phone.select_country_iso2("US");
        phone.on_phone_edited("5550101234");
        assert_eq!(phone.phone_input, "555 010 1234");
        assert_eq!(phone.full_number(), "+1 555 010 1234");

        // Country search matches name, alternative name, ISO2, and calling code.
        let by_alt = phone.filtered_countries("United States");
        assert!(by_alt.iter().any(|e| e.iso2 == "US"));
        let by_code = phone.filtered_countries("+49");
        assert!(by_code.iter().any(|e| e.iso2 == "DE"));
        let empty = phone.filtered_countries("zzznonexistentcountry");
        assert!(empty.is_empty());
    }

    #[test]
    fn app_code_sms_resend_call_countdown_and_errors() {
        let mut intro = IntroWidget::new();
        let now = Instant::now();
        intro.switch_to_phone();

        // Invalid phone triggers PHONE_NUMBER_INVALID + shake.
        let err = intro.submit_phone(&format!("+{TEST_PHONE_INVALID}"), now);
        assert_eq!(
            err,
            Err(ApiError::Login(LoginErrorKind::PhoneNumberInvalid))
        );
        assert_eq!(intro.error, tr::lng_bad_phone());
        assert!(intro.phone.shake.is_animating(now));

        // Flood wait error maps to localized flood text.
        let flood = intro.submit_phone(&format!("+{TEST_PHONE_FLOOD}"), now);
        assert!(matches!(
            flood,
            Err(ApiError::Login(LoginErrorKind::FloodWait { seconds: 30 }))
        ));
        assert_eq!(intro.error, tr::lng_flood_error());

        // Banned phone opens banned modal.
        let banned = intro.submit_phone(&format!("+{TEST_PHONE_BANNED}"), now);
        assert_eq!(
            banned,
            Err(ApiError::Login(LoginErrorKind::PhoneNumberBanned))
        );
        assert_eq!(intro.phone.banned_modal, Some(tr::lng_signin_banned_text()));
        intro.phone.banned_modal = None;

        // Valid synthetic phone moves to Code step (delivered via App).
        intro
            .submit_phone(&format!("+{TEST_PHONE_APP}"), now)
            .expect("valid synthetic phone");
        assert_eq!(intro.current_step(), IntroStep::Code);
        assert!(matches!(
            intro.login_step(),
            LoginStep::CodeInput(s) if s.code_by_telegram && s.code_length == 5
        ));

        // Wrong code -> PHONE_CODE_INVALID + digit box shake.
        let bad = intro.submit_code("11111", now);
        assert_eq!(bad, Err(ApiError::Login(LoginErrorKind::PhoneCodeInvalid)));
        assert_eq!(intro.error, tr::lng_bad_code());
        assert!(intro.code.input.error_border);
        assert!(intro.code.input.shake.is_animating(now));

        // Flood code -> FLOOD_WAIT_45.
        let code_flood = intro.submit_code(TEST_CODE_FLOOD, now);
        assert!(matches!(
            code_flood,
            Err(ApiError::Login(LoginErrorKind::FloodWait { seconds: 45 }))
        ));
        assert_eq!(intro.error, tr::lng_flood_error());

        // Resend code via SMS enables Call fallback timer (`Waiting`, 60s).
        intro.resend_code(now).expect("resend via SMS");
        assert!(matches!(
            intro.login_step(),
            LoginStep::CodeInput(s)
                if !s.code_by_telegram
                    && s.call_status == CallStatus::Waiting
                    && s.call_timeout_secs == 60
        ));

        // Tick countdown down to 0 -> triggers automated call resend (`Called`).
        for _ in 0..59 {
            assert_eq!(
                intro.tick_call_countdown(now).expect("tick waiting"),
                CallStatus::Waiting
            );
        }
        assert_eq!(
            intro.tick_call_countdown(now).expect("tick triggers call"),
            CallStatus::Called
        );
        assert_eq!(
            format_call_status_text(false, CallStatus::Called, 0),
            Some(tr::lng_code_called())
        );

        // Correct code finishes authorization!
        intro.submit_code(TEST_CODE_APP, now).expect("valid code");
        assert!(matches!(intro.login_step(), LoginStep::Authorized { .. }));
    }

    #[test]
    fn expired_code_and_back_cancel_code() {
        let mut intro = IntroWidget::new();
        let now = Instant::now();
        intro.switch_to_phone();
        intro
            .submit_phone(&format!("+{TEST_PHONE_APP}"), now)
            .expect("send code");
        assert_eq!(intro.current_step(), IntroStep::Code);

        // Going back cancels the active login code on the server.
        assert!(intro.go_back());
        assert_eq!(intro.current_step(), IntroStep::Phone);
        assert_eq!(intro.fake_rpc().expect("fake rpc").cancel_code_calls(), 1);

        // Submitting an expired code returns to Phone step.
        intro
            .submit_phone(&format!("+{TEST_PHONE_APP}"), now)
            .expect("send code again");
        let exp = intro.submit_code(TEST_CODE_EXPIRED, now);
        assert_eq!(exp, Err(ApiError::Login(LoginErrorKind::PhoneCodeExpired)));
        assert_eq!(intro.current_step(), IntroStep::Phone);
    }

    #[test]
    fn srp_2fa_password_check_recovery_and_reset_flows() {
        let mut intro = IntroWidget::new();
        let now = Instant::now();
        intro.switch_to_phone();
        intro
            .submit_phone(&format!("+{TEST_PHONE_2FA}"), now)
            .expect("send code");
        intro
            .submit_code(TEST_CODE_2FA, now)
            .expect("transitions to PasswordCheck");

        assert_eq!(intro.current_step(), IntroStep::Password);
        match intro.login_step() {
            LoginStep::PasswordCheck { password_state, .. } => {
                assert_eq!(password_state.hint, TEST_2FA_HINT);
                assert!(password_state.has_recovery);
            }
            other => panic!("expected PasswordCheck, got {other:?}"),
        }

        // Wrong password fails real SRP-6a verification via tg-crypto.
        let wrong = intro.submit_password("wrong-pass", now);
        assert_eq!(wrong, Err(ApiError::Login(LoginErrorKind::PasswordInvalid)));
        assert_eq!(intro.error, tr::lng_signin_bad_password());
        assert!(intro.password.shake.is_animating(now));

        // Correct password passes real SRP-6a verification via tg-crypto!
        intro
            .submit_password(TEST_2FA_PASSWORD, now)
            .expect("SRP verification succeeds");
        assert!(matches!(intro.login_step(), LoginStep::Authorized { .. }));

        // Recovery email code flow.
        let mut rec = IntroWidget::new();
        rec.switch_to_phone();
        rec.submit_phone(&format!("+{TEST_PHONE_2FA}"), now)
            .expect("send code");
        rec.submit_code(TEST_CODE_2FA, now).expect("enter 2FA");
        let pattern = rec
            .request_password_recovery(now)
            .expect("request recovery");
        assert_eq!(pattern, TEST_2FA_RECOVERY_EMAIL);
        assert!(matches!(
            rec.login_step(),
            LoginStep::PasswordCheck {
                mode: PasswordCheckMode::Recovery { .. },
                ..
            }
        ));
        assert_eq!(
            rec.submit_recovery_code("000001", now),
            Err(ApiError::Login(LoginErrorKind::RecoveryCodeInvalid))
        );
        rec.submit_recovery_code(TEST_2FA_RECOVERY_CODE, now)
            .expect("recover password");
        assert!(matches!(
            rec.login_step(),
            LoginStep::Authorized {
                password_recovered: true,
                ..
            }
        ));

        // Account reset flow states: 2FA_CONFIRM_WAIT -> RecentConfirm -> Immediate.
        let invoker = FakeRpcClient::new(FakeRpcConfig {
            reset_behavior: FakeResetBehavior::ConfirmWait(604_800),
            ..FakeRpcConfig::default()
        });
        let mut rst = IntroWidget::with_invoker(
            ApiCredentials {
                api_id: 0,
                api_hash: String::new(),
            },
            invoker,
        );
        rst.switch_to_phone();
        rst.submit_phone(&format!("+{TEST_PHONE_2FA}"), now)
            .expect("send code");
        rst.submit_code(TEST_CODE_2FA, now).expect("enter 2FA");

        let wait_err = rst.reset_account(now);
        assert!(matches!(
            wait_err,
            Err(ApiError::Login(LoginErrorKind::TwoFaConfirmWait {
                seconds: 604_800
            }))
        ));
        assert!(matches!(
            rst.password.modal,
            Some(PasswordModal::ResetNotice(_))
        ));

        rst.fake_rpc()
            .expect("fake rpc")
            .set_reset_behavior(FakeResetBehavior::RecentConfirm);
        let recent_err = rst.reset_account(now);
        assert_eq!(
            recent_err,
            Err(ApiError::Login(LoginErrorKind::TwoFaRecentConfirm))
        );

        rst.fake_rpc()
            .expect("fake rpc")
            .set_reset_behavior(FakeResetBehavior::Immediate);
        rst.reset_account(now).expect("reset transitions to SignUp");
        assert_eq!(rst.current_step(), IntroStep::SignUp);
    }

    #[test]
    fn signup_terms_and_email_setup_and_fragment_flows() {
        let now = Instant::now();

        // SignUp + Terms of Service enforcement
        let mut intro = IntroWidget::new();
        intro.switch_to_phone();
        intro
            .submit_phone(&format!("+{TEST_PHONE_SIGNUP}"), now)
            .expect("send code");
        intro
            .submit_code(TEST_CODE_SIGNUP, now)
            .expect("sign up required");
        assert_eq!(intro.current_step(), IntroStep::SignUp);

        // Submitting before accepting Terms fails with TermsNotAccepted.
        assert_eq!(
            intro.submit_signup("Demo", "Tester", now),
            Err(ApiError::Login(LoginErrorKind::TermsNotAccepted))
        );
        intro.signup.first_name = "Demo".to_owned();
        intro.signup.last_name = "Tester".to_owned();
        intro
            .accept_terms_and_submit_signup(now)
            .expect("sign up succeeds after accepting terms");
        assert!(matches!(intro.login_step(), LoginStep::Authorized { .. }));

        // Mandatory Login Email setup -> EmailVerify -> EmailCode login
        let mut em = IntroWidget::new();
        em.switch_to_phone();
        em.submit_phone(&format!("+{TEST_PHONE_EMAIL_SETUP}"), now)
            .expect("requires email setup");
        assert_eq!(em.current_step(), IntroStep::EmailSetup);
        assert_eq!(
            em.submit_email_setup("not-an-email", now),
            Err(ApiError::Login(LoginErrorKind::EmailInvalid))
        );
        assert_eq!(
            em.submit_email_setup("blocked@example.test", now),
            Err(ApiError::Login(LoginErrorKind::EmailNotAllowed))
        );
        em.submit_email_setup("demo@example.test", now)
            .expect("sends verify email code");
        assert_eq!(em.current_step(), IntroStep::EmailVerify);
        em.submit_code(TEST_CODE_EMAIL, now)
            .expect("verifies email and sends email login code");
        assert_eq!(em.current_step(), IntroStep::Code);
        em.submit_code(TEST_CODE_EMAIL, now)
            .expect("signs in with email code");
        assert!(matches!(em.login_step(), LoginStep::Authorized { .. }));

        // Fragment SMS flow
        let mut frag = IntroWidget::new();
        frag.switch_to_phone();
        frag.submit_phone(&format!("+{TEST_PHONE_FRAGMENT}"), now)
            .expect("fragment sent");
        assert!(matches!(
            frag.login_step(),
            LoginStep::CodeInput(s) if s.fragment_url.is_some()
        ));
        frag.submit_code(TEST_CODE_FRAGMENT, now)
            .expect("fragment login");
        assert!(matches!(frag.login_step(), LoginStep::Authorized { .. }));
    }

    #[test]
    fn qr_login_token_refresh_acceptance_and_rounded_rendering() {
        let now = Instant::now();
        let mut intro = IntroWidget::new();
        intro.start_qr_flow(now).expect("starts QR flow");
        assert_eq!(intro.current_step(), IntroStep::Qr);

        let url1 = match intro.login_step() {
            LoginStep::QrLogin(qr) => {
                assert!(qr.qr_url.starts_with("tg://login?token="));
                qr.qr_url.clone()
            }
            other => panic!("expected QrLogin, got {other:?}"),
        };

        // Verify QR encoding and tdesktop rounded renderer with Rustgram center icon.
        let matrix = QrMatrix::encode(&url1).expect("QR encodes");
        assert!(matrix.size >= 29);
        let replaced = qr_replace_elements(matrix.size);
        assert_eq!((matrix.size - replaced) % 2, 0);

        let icon = render_tgfork_icon_rgba(88);
        assert_eq!(icon.len(), 88 * 88 * 4);
        let (rgba, w, h) =
            render_tdesktop_qr_rgba(&url1, 180, 2, Some((&icon, 88))).expect("renders QR");
        assert_eq!(w, h);
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        // Refresh rotates the token.
        intro.refresh_qr_token(now).expect("refreshes QR token");
        let url2 = match intro.login_step() {
            LoginStep::QrLogin(qr) => qr.qr_url.clone(),
            other => panic!("expected QrLogin, got {other:?}"),
        };
        assert_ne!(url1, url2);

        // Simulated mobile acceptance via DC migration completes login.
        intro
            .fake_rpc()
            .expect("fake rpc")
            .set_qr_behavior(FakeQrBehavior::MigrateThenAccept { dc_id: 4 });
        intro.refresh_qr_token(now).expect("migrates & accepts QR");
        assert_eq!(intro.main_dc_id(), 4);
        assert!(matches!(intro.login_step(), LoginStep::Authorized { .. }));
    }

    #[test]
    fn segmented_code_input_shake_and_delivery_texts() {
        let mut input = SegmentedCodeInput::new(5);
        assert_eq!(input.input_digit(1), None);
        assert_eq!(input.input_digit(2), None);
        input.backspace();
        assert_eq!(input.collect_code(), "1");
        assert_eq!(input.set_code("12345"), Some("12345".to_owned()));
        assert!(input.is_complete());

        // Shake curve endpoints and peak match `shake_animation.cpp`.
        assert_eq!(shake_offset_at(0.0), 0.0);
        assert_eq!(shake_offset_at(1.0 / 6.0), 4.0);
        assert_eq!(shake_offset_at(2.0 / 6.0), -4.0);
        assert_eq!(shake_offset_at(1.0), 0.0);

        // All generated tg_tl SentCodeType variants produce non-empty localized texts.
        let types = [
            SentCodeType::App(tl_types::auth::SentCodeTypeApp { length: 5 }),
            SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
            SentCodeType::Call(tl_types::auth::SentCodeTypeCall { length: 5 }),
            SentCodeType::FlashCall(tl_types::auth::SentCodeTypeFlashCall {
                pattern: "+1***".to_owned(),
            }),
            SentCodeType::MissedCall(tl_types::auth::SentCodeTypeMissedCall {
                prefix: "+1555".to_owned(),
                length: 5,
            }),
            SentCodeType::FragmentSms(tl_types::auth::SentCodeTypeFragmentSms {
                url: "https://fragment.com".to_owned(),
                length: 5,
            }),
            SentCodeType::EmailCode(tl_types::auth::SentCodeTypeEmailCode {
                apple_signin_allowed: false,
                google_signin_allowed: false,
                email_pattern: "u***@example.test".to_owned(),
                length: 6,
                reset_available_period: None,
                reset_pending_date: None,
            }),
        ];
        for t in &types {
            let kind = CodeDeliveryKind::from_sent_code_type(t);
            assert!(!kind.description_text("+1 555 010 0000").is_empty());
        }
    }
}
