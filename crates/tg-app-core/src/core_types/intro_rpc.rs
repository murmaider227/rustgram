//! Object-safe [`DynIntroRpc`] bridge over [`tg_api::RpcClient`] for Intro login & passkey RPCs.

use std::future::Future;
use std::pin::Pin;

use tg_api::invoker::{InvokeResult, Invoker};
use tg_api::rpc::{DcId, RpcClient};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::broadcast;

/// Heap-allocated future returned by [`DynIntroRpc`].
pub type IntroBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Object-safe bridge over [`tg_api::RpcClient`] for Intro login RPCs.
///
/// Blanket-implemented for every `C: tg_api::RpcClient` (including `Arc<C>`), allowing
/// [`IntroWidget`] to accept any real or fake MTProto RPC client without becoming generic.
pub trait DynIntroRpc: Send + Sync {
    /// Returns the currently selected main data center ID.
    fn main_dc_id(&self) -> DcId;

    /// Switches the main data center ID.
    fn set_main_dc_id(&self, dc_id: DcId);

    /// Subscribes to live `Updates` pushes (used for QR `Update::LoginToken`), if supported.
    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>>;

    /// Calls `help.getNearestDc`.
    fn help_get_nearest_dc(&self) -> IntroBoxFuture<'_, InvokeResult<tl_types::NearestDc>>;

    /// Calls `auth.sendCode`.
    fn auth_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::SentCode>>;

    /// Calls `auth.resendCode`.
    fn auth_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::SentCode>>;

    /// Calls `auth.cancelCode`.
    fn auth_cancel_code(
        &self,
        req: tl_fns::auth::CancelCode,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>>;

    /// Calls `auth.signIn`.
    fn auth_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>>;

    /// Calls `auth.signUp`.
    fn auth_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>>;

    /// Calls `account.getPassword`.
    fn account_get_password(&self)
    -> IntroBoxFuture<'_, InvokeResult<tl_types::account::Password>>;

    /// Calls `auth.checkPassword`.
    fn auth_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>>;

    /// Calls `auth.requestPasswordRecovery`.
    fn auth_request_password_recovery(
        &self,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::auth::PasswordRecovery>>;

    /// Calls `auth.checkRecoveryPassword`.
    fn auth_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>>;

    /// Calls `auth.recoverPassword`.
    fn auth_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>>;

    /// Calls `account.sendVerifyEmailCode`.
    fn account_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::account::SentEmailCode>>;

    /// Calls `account.verifyEmail`.
    fn account_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::account::EmailVerified>>;

    /// Calls `account.deleteAccount`.
    fn account_delete_account(
        &self,
        req: tl_fns::account::DeleteAccount,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>>;

    /// Calls `auth.exportLoginToken`.
    fn auth_export_login_token(
        &self,
        req: tl_fns::auth::ExportLoginToken,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::LoginToken>>;

    /// Calls `auth.importLoginToken` on `dc_id`.
    fn auth_import_login_token(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::ImportLoginToken,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::LoginToken>>;

    /// Calls `auth.initPasskeyLogin`.
    fn auth_init_passkey_login(
        &self,
        req: tl_fns::auth::InitPasskeyLogin,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::PasskeyLoginOptions>>;

    /// Calls `auth.finishPasskeyLogin` on `dc_id`.
    fn auth_finish_passkey_login(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::FinishPasskeyLogin,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>>;

    /// Calls `auth.logOut`.
    fn auth_log_out(&self) -> IntroBoxFuture<'_, InvokeResult<tl_types::auth::LoggedOut>>;

    /// Calls `messages.getDialogFilters`.
    fn messages_get_dialog_filters(
        &self,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::messages::DialogFilters>>;
}

impl<C: RpcClient> DynIntroRpc for C {
    fn main_dc_id(&self) -> DcId {
        RpcClient::main_dc_id(self)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        RpcClient::set_main_dc_id(self, dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        RpcClient::subscribe_updates(self)
    }

    fn help_get_nearest_dc(&self) -> IntroBoxFuture<'_, InvokeResult<tl_types::NearestDc>> {
        Box::pin(Invoker::help_get_nearest_dc(self))
    }

    fn auth_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::SentCode>> {
        Box::pin(Invoker::auth_send_code(self, req))
    }

    fn auth_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::SentCode>> {
        Box::pin(Invoker::auth_resend_code(self, req))
    }

    fn auth_cancel_code(
        &self,
        req: tl_fns::auth::CancelCode,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>> {
        Box::pin(Invoker::auth_cancel_code(self, req))
    }

    fn auth_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>> {
        Box::pin(Invoker::auth_sign_in(self, req))
    }

    fn auth_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>> {
        Box::pin(Invoker::auth_sign_up(self, req))
    }

    fn account_get_password(
        &self,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::account::Password>> {
        Box::pin(Invoker::account_get_password(self))
    }

    fn auth_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>> {
        Box::pin(Invoker::auth_check_password(self, req))
    }

    fn auth_request_password_recovery(
        &self,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::auth::PasswordRecovery>> {
        Box::pin(Invoker::auth_request_password_recovery(self))
    }

    fn auth_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>> {
        Box::pin(Invoker::auth_check_recovery_password(self, req))
    }

    fn auth_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>> {
        Box::pin(Invoker::auth_recover_password(self, req))
    }

    fn account_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::account::SentEmailCode>> {
        Box::pin(Invoker::account_send_verify_email_code(self, req))
    }

    fn account_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::account::EmailVerified>> {
        Box::pin(Invoker::account_verify_email(self, req))
    }

    fn account_delete_account(
        &self,
        req: tl_fns::account::DeleteAccount,
    ) -> IntroBoxFuture<'_, InvokeResult<bool>> {
        Box::pin(Invoker::account_delete_account(self, req))
    }

    fn auth_export_login_token(
        &self,
        req: tl_fns::auth::ExportLoginToken,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::LoginToken>> {
        Box::pin(Invoker::auth_export_login_token(self, req))
    }

    fn auth_import_login_token(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::ImportLoginToken,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::LoginToken>> {
        Box::pin(Invoker::auth_import_login_token(self, dc_id, req))
    }

    fn auth_init_passkey_login(
        &self,
        req: tl_fns::auth::InitPasskeyLogin,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::PasskeyLoginOptions>> {
        Box::pin(async move { RpcClient::invoke(self, &req).await.map_err(Into::into) })
    }

    fn auth_finish_passkey_login(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::FinishPasskeyLogin,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_enums::auth::Authorization>> {
        Box::pin(async move {
            RpcClient::invoke_in_dc(self, dc_id, &req)
                .await
                .map_err(Into::into)
        })
    }

    fn auth_log_out(&self) -> IntroBoxFuture<'_, InvokeResult<tl_types::auth::LoggedOut>> {
        Box::pin(Invoker::auth_log_out(self))
    }

    fn messages_get_dialog_filters(
        &self,
    ) -> IntroBoxFuture<'_, InvokeResult<tl_types::messages::DialogFilters>> {
        Box::pin(Invoker::messages_get_dialog_filters(self))
    }
}
