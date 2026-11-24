//! Async [`Invoker`] trait over generated `tg_tl` functions and blanket [`RpcClient`] impl.
//!
//! Port origin: `mtproto/sender.h`, `mtproto/mtp_instance.h`, `crates/tg-api/docs/requests.md`.
//!
//! Every method maps 1:1 to a generated `tg_tl::functions::*` RPC call and returns
//! the generated `tg_tl` response type. Any type implementing [`RpcClient`] gets a
//! complete [`Invoker`] implementation automatically.

use std::future::Future;

use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::rpc::{DcId, InvocationError, RpcClient};

/// Result alias for `Invoker` RPC calls.
pub type InvokeResult<T> = Result<T, InvocationError>;

/// Async RPC invoker trait for Telegram API calls using generated `tg_tl` types.
///
/// One method per API call used by `tg-api`'s auth, dialogs, history, messaging,
/// updates, and localization workflows.
pub trait Invoker: Send + Sync {
    /// Returns the currently selected main data center ID.
    fn main_dc_id(&self) -> DcId;

    /// Switches the main data center ID (e.g. on `PHONE_MIGRATE_X` or `loginTokenMigrateTo`).
    fn set_main_dc_id(&self, dc_id: DcId);

    // --- Intro / Auth / Help ---

    /// Calls `help.getNearestDc`.
    fn help_get_nearest_dc(&self)
    -> impl Future<Output = InvokeResult<tl_types::NearestDc>> + Send;

    /// Calls `help.getCountriesList`.
    fn help_get_countries_list(
        &self,
        req: tl_fns::help::GetCountriesList,
    ) -> impl Future<Output = InvokeResult<tl_enums::help::CountriesList>> + Send;

    /// Calls `auth.sendCode`.
    fn auth_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::SentCode>> + Send;

    /// Calls `auth.resendCode`.
    fn auth_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::SentCode>> + Send;

    /// Calls `auth.cancelCode`.
    fn auth_cancel_code(
        &self,
        req: tl_fns::auth::CancelCode,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `auth.signIn`.
    fn auth_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::Authorization>> + Send;

    /// Calls `auth.signUp`.
    fn auth_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::Authorization>> + Send;

    /// Calls `account.getPassword`.
    fn account_get_password(
        &self,
    ) -> impl Future<Output = InvokeResult<tl_types::account::Password>> + Send;

    /// Calls `auth.checkPassword`.
    fn auth_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::Authorization>> + Send;

    /// Calls `auth.requestPasswordRecovery`.
    fn auth_request_password_recovery(
        &self,
    ) -> impl Future<Output = InvokeResult<tl_types::auth::PasswordRecovery>> + Send;

    /// Calls `auth.checkRecoveryPassword`.
    fn auth_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `auth.recoverPassword`.
    fn auth_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::Authorization>> + Send;

    /// Calls `account.sendVerifyEmailCode`.
    fn account_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> impl Future<Output = InvokeResult<tl_types::account::SentEmailCode>> + Send;

    /// Calls `account.verifyEmail`.
    fn account_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> impl Future<Output = InvokeResult<tl_enums::account::EmailVerified>> + Send;

    /// Calls `account.deleteAccount`.
    fn account_delete_account(
        &self,
        req: tl_fns::account::DeleteAccount,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `auth.exportLoginToken`.
    fn auth_export_login_token(
        &self,
        req: tl_fns::auth::ExportLoginToken,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::LoginToken>> + Send;

    /// Calls `auth.importLoginToken` explicitly on `dc_id` (`.toDC(dcId)`).
    fn auth_import_login_token(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::ImportLoginToken,
    ) -> impl Future<Output = InvokeResult<tl_enums::auth::LoginToken>> + Send;

    /// Calls `auth.logOut`.
    fn auth_log_out(&self) -> impl Future<Output = InvokeResult<tl_types::auth::LoggedOut>> + Send;

    // --- Dialogs & Folders ---

    /// Calls `messages.getDialogs`.
    fn messages_get_dialogs(
        &self,
        req: tl_fns::messages::GetDialogs,
    ) -> impl Future<Output = InvokeResult<tl_enums::messages::Dialogs>> + Send;

    /// Calls `messages.getPinnedDialogs`.
    fn messages_get_pinned_dialogs(
        &self,
        req: tl_fns::messages::GetPinnedDialogs,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::PeerDialogs>> + Send;

    /// Calls `messages.reorderPinnedDialogs`.
    fn messages_reorder_pinned_dialogs(
        &self,
        req: tl_fns::messages::ReorderPinnedDialogs,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `messages.getPeerDialogs`.
    fn messages_get_peer_dialogs(
        &self,
        req: tl_fns::messages::GetPeerDialogs,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::PeerDialogs>> + Send;

    /// Calls `messages.getDialogFilters`.
    fn messages_get_dialog_filters(
        &self,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::DialogFilters>> + Send;

    /// Calls `messages.markDialogUnread`.
    fn messages_mark_dialog_unread(
        &self,
        req: tl_fns::messages::MarkDialogUnread,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `folders.editPeerFolders`.
    fn folders_edit_peer_folders(
        &self,
        req: tl_fns::folders::EditPeerFolders,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send;

    // --- History & Messaging ---

    /// Calls `messages.getHistory`.
    fn messages_get_history(
        &self,
        req: tl_fns::messages::GetHistory,
    ) -> impl Future<Output = InvokeResult<tl_enums::messages::Messages>> + Send;

    /// Calls `messages.getReplies`.
    fn messages_get_replies(
        &self,
        req: tl_fns::messages::GetReplies,
    ) -> impl Future<Output = InvokeResult<tl_enums::messages::Messages>> + Send;

    /// Calls `messages.sendMessage`.
    fn messages_send_message(
        &self,
        req: tl_fns::messages::SendMessage,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send;

    /// Calls `messages.sendMedia`.
    fn messages_send_media(
        &self,
        _req: tl_fns::messages::SendMedia,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "messages.sendMedia not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `messages.sendMultiMedia`.
    fn messages_send_multi_media(
        &self,
        _req: tl_fns::messages::SendMultiMedia,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "messages.sendMultiMedia not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `messages.editMessage`.
    fn messages_edit_message(
        &self,
        req: tl_fns::messages::EditMessage,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send;

    /// Calls `messages.deleteMessages`.
    fn messages_delete_messages(
        &self,
        req: tl_fns::messages::DeleteMessages,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::AffectedMessages>> + Send;

    /// Calls `channels.deleteMessages`.
    fn channels_delete_messages(
        &self,
        req: tl_fns::channels::DeleteMessages,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::AffectedMessages>> + Send;

    /// Calls `messages.deleteHistory`.
    fn messages_delete_history(
        &self,
        req: tl_fns::messages::DeleteHistory,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::AffectedHistory>> + Send;

    /// Calls `channels.deleteHistory`.
    fn channels_delete_history(
        &self,
        req: tl_fns::channels::DeleteHistory,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send;

    /// Calls `messages.forwardMessages`.
    fn messages_forward_messages(
        &self,
        req: tl_fns::messages::ForwardMessages,
    ) -> impl Future<Output = InvokeResult<tl_enums::Updates>> + Send;

    /// Calls `messages.readHistory`.
    fn messages_read_history(
        &self,
        req: tl_fns::messages::ReadHistory,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::AffectedMessages>> + Send;

    /// Calls `channels.readHistory`.
    fn channels_read_history(
        &self,
        req: tl_fns::channels::ReadHistory,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `messages.setTyping`.
    fn messages_set_typing(
        &self,
        req: tl_fns::messages::SetTyping,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    /// Calls `messages.saveDraft`.
    fn messages_save_draft(
        &self,
        req: tl_fns::messages::SaveDraft,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    // --- Updates & Presence ---

    /// Calls `updates.getState`.
    fn updates_get_state(
        &self,
    ) -> impl Future<Output = InvokeResult<tl_types::updates::State>> + Send;

    /// Calls `updates.getDifference`.
    fn updates_get_difference(
        &self,
        req: tl_fns::updates::GetDifference,
    ) -> impl Future<Output = InvokeResult<tl_enums::updates::Difference>> + Send;

    /// Calls `updates.getChannelDifference`.
    fn updates_get_channel_difference(
        &self,
        req: tl_fns::updates::GetChannelDifference,
    ) -> impl Future<Output = InvokeResult<tl_enums::updates::ChannelDifference>> + Send;

    /// Calls `account.updateStatus`.
    fn account_update_status(
        &self,
        req: tl_fns::account::UpdateStatus,
    ) -> impl Future<Output = InvokeResult<bool>> + Send;

    // --- Language Packs ---

    /// Calls `langpack.getLanguages`.
    fn langpack_get_languages(
        &self,
        req: tl_fns::langpack::GetLanguages,
    ) -> impl Future<Output = InvokeResult<Vec<tl_enums::LangPackLanguage>>> + Send;

    /// Calls `langpack.getLangPack`.
    fn langpack_get_lang_pack(
        &self,
        req: tl_fns::langpack::GetLangPack,
    ) -> impl Future<Output = InvokeResult<tl_types::LangPackDifference>> + Send;

    /// Calls `langpack.getDifference`.
    fn langpack_get_difference(
        &self,
        req: tl_fns::langpack::GetDifference,
    ) -> impl Future<Output = InvokeResult<tl_types::LangPackDifference>> + Send;

    /// Calls `langpack.getLanguage`.
    fn langpack_get_language(
        &self,
        req: tl_fns::langpack::GetLanguage,
    ) -> impl Future<Output = InvokeResult<tl_types::LangPackLanguage>> + Send;

    /// Calls `langpack.getStrings`.
    fn langpack_get_strings(
        &self,
        req: tl_fns::langpack::GetStrings,
    ) -> impl Future<Output = InvokeResult<Vec<tl_enums::LangPackString>>> + Send;

    // --- Channels, Chats, Contacts & Privacy ---

    /// Calls `channels.getChannels` (used e.g. to refresh channel metadata on `updateChannel`).
    fn channels_get_channels(
        &self,
        _req: tl_fns::channels::GetChannels,
    ) -> impl Future<Output = InvokeResult<tl_enums::messages::Chats>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "channels.getChannels not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `messages.getFullChat` (used e.g. when `updateChatParticipant*` skips versions).
    fn messages_get_full_chat(
        &self,
        _req: tl_fns::messages::GetFullChat,
    ) -> impl Future<Output = InvokeResult<tl_types::messages::ChatFull>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "messages.getFullChat not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `contacts.getContacts`.
    fn contacts_get_contacts(
        &self,
        _req: tl_fns::contacts::GetContacts,
    ) -> impl Future<Output = InvokeResult<tl_enums::contacts::Contacts>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "contacts.getContacts not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `contacts.resolveUsername`.
    fn contacts_resolve_username(
        &self,
        _req: tl_fns::contacts::ResolveUsername,
    ) -> impl Future<Output = InvokeResult<tl_types::contacts::ResolvedPeer>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "contacts.resolveUsername not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `contacts.getBlocked`.
    fn contacts_get_blocked(
        &self,
        _req: tl_fns::contacts::GetBlocked,
    ) -> impl Future<Output = InvokeResult<tl_enums::contacts::Blocked>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "contacts.getBlocked not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `contacts.block`.
    fn contacts_block(
        &self,
        _req: tl_fns::contacts::Block,
    ) -> impl Future<Output = InvokeResult<bool>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "contacts.block not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `contacts.unblock`.
    fn contacts_unblock(
        &self,
        _req: tl_fns::contacts::Unblock,
    ) -> impl Future<Output = InvokeResult<bool>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "contacts.unblock not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `account.getPrivacy`.
    fn account_get_privacy(
        &self,
        _req: tl_fns::account::GetPrivacy,
    ) -> impl Future<Output = InvokeResult<tl_types::account::PrivacyRules>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "account.getPrivacy not implemented on this Invoker".to_owned(),
        )))
    }

    /// Calls `account.setPrivacy`.
    fn account_set_privacy(
        &self,
        _req: tl_fns::account::SetPrivacy,
    ) -> impl Future<Output = InvokeResult<tl_types::account::PrivacyRules>> + Send {
        std::future::ready(Err(InvocationError::Transport(
            "account.setPrivacy not implemented on this Invoker".to_owned(),
        )))
    }
}

impl<C: RpcClient> Invoker for C {
    fn main_dc_id(&self) -> DcId {
        RpcClient::main_dc_id(self)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        RpcClient::set_main_dc_id(self, dc_id);
    }

    async fn help_get_nearest_dc(&self) -> InvokeResult<tl_types::NearestDc> {
        let tl_enums::NearestDc::NearestDc(dc) = self
            .invoke(&tl_fns::help::GetNearestDc)
            .await
            .map_err(Into::into)?;
        Ok(dc)
    }

    async fn help_get_countries_list(
        &self,
        req: tl_fns::help::GetCountriesList,
    ) -> InvokeResult<tl_enums::help::CountriesList> {
        self.invoke(&req).await.map_err(Into::into)
    }

    // Match tdesktop `intro/` widgets (`intro_phone.cpp:219`, `intro_code.cpp:294,398,414`,
    // `intro_signup.cpp:198`, `intro_password_check.cpp:226,383`, `intro_qr.cpp:278,499`):
    // login RPCs pass `.handleFloodErrors()` so `FLOOD_WAIT_X` surfaces immediately to the UI.
    async fn auth_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> InvokeResult<tl_enums::auth::SentCode> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> InvokeResult<tl_enums::auth::SentCode> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_cancel_code(&self, req: tl_fns::auth::CancelCode) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn auth_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn account_get_password(&self) -> InvokeResult<tl_types::account::Password> {
        let tl_enums::account::Password::Password(pwd) = self
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(Into::into)?;
        Ok(pwd)
    }

    async fn auth_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_request_password_recovery(
        &self,
    ) -> InvokeResult<tl_types::auth::PasswordRecovery> {
        let tl_enums::auth::PasswordRecovery::PasswordRecovery(rec) = self
            .invoke_handle_flood(&tl_fns::auth::RequestPasswordRecovery)
            .await
            .map_err(Into::into)?;
        Ok(rec)
    }

    async fn auth_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> InvokeResult<bool> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> InvokeResult<tl_enums::auth::Authorization> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn account_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> InvokeResult<tl_types::account::SentEmailCode> {
        let tl_enums::account::SentEmailCode::SentEmailCode(sent) =
            self.invoke_handle_flood(&req).await.map_err(Into::into)?;
        Ok(sent)
    }

    async fn account_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> InvokeResult<tl_enums::account::EmailVerified> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn account_delete_account(
        &self,
        req: tl_fns::account::DeleteAccount,
    ) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn auth_export_login_token(
        &self,
        req: tl_fns::auth::ExportLoginToken,
    ) -> InvokeResult<tl_enums::auth::LoginToken> {
        self.invoke_handle_flood(&req).await.map_err(Into::into)
    }

    async fn auth_import_login_token(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::ImportLoginToken,
    ) -> InvokeResult<tl_enums::auth::LoginToken> {
        self.invoke_in_dc_handle_flood(dc_id, &req)
            .await
            .map_err(Into::into)
    }

    async fn auth_log_out(&self) -> InvokeResult<tl_types::auth::LoggedOut> {
        let tl_enums::auth::LoggedOut::LoggedOut(logged_out) = self
            .invoke(&tl_fns::auth::LogOut)
            .await
            .map_err(Into::into)?;
        Ok(logged_out)
    }

    async fn messages_get_dialogs(
        &self,
        req: tl_fns::messages::GetDialogs,
    ) -> InvokeResult<tl_enums::messages::Dialogs> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_get_pinned_dialogs(
        &self,
        req: tl_fns::messages::GetPinnedDialogs,
    ) -> InvokeResult<tl_types::messages::PeerDialogs> {
        let tl_enums::messages::PeerDialogs::PeerDialogs(pd) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(pd)
    }

    async fn messages_reorder_pinned_dialogs(
        &self,
        req: tl_fns::messages::ReorderPinnedDialogs,
    ) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_get_peer_dialogs(
        &self,
        req: tl_fns::messages::GetPeerDialogs,
    ) -> InvokeResult<tl_types::messages::PeerDialogs> {
        let tl_enums::messages::PeerDialogs::PeerDialogs(pd) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(pd)
    }

    async fn messages_get_dialog_filters(&self) -> InvokeResult<tl_types::messages::DialogFilters> {
        let tl_enums::messages::DialogFilters::DialogFilters(filters) = self
            .invoke(&tl_fns::messages::GetDialogFilters)
            .await
            .map_err(Into::into)?;
        Ok(filters)
    }

    async fn messages_mark_dialog_unread(
        &self,
        req: tl_fns::messages::MarkDialogUnread,
    ) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn folders_edit_peer_folders(
        &self,
        req: tl_fns::folders::EditPeerFolders,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_get_history(
        &self,
        req: tl_fns::messages::GetHistory,
    ) -> InvokeResult<tl_enums::messages::Messages> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_get_replies(
        &self,
        req: tl_fns::messages::GetReplies,
    ) -> InvokeResult<tl_enums::messages::Messages> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_send_message(
        &self,
        req: tl_fns::messages::SendMessage,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_send_media(
        &self,
        req: tl_fns::messages::SendMedia,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_send_multi_media(
        &self,
        req: tl_fns::messages::SendMultiMedia,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_edit_message(
        &self,
        req: tl_fns::messages::EditMessage,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_delete_messages(
        &self,
        req: tl_fns::messages::DeleteMessages,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        let tl_enums::messages::AffectedMessages::AffectedMessages(affected) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(affected)
    }

    async fn channels_delete_messages(
        &self,
        req: tl_fns::channels::DeleteMessages,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        let tl_enums::messages::AffectedMessages::AffectedMessages(affected) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(affected)
    }

    async fn messages_delete_history(
        &self,
        req: tl_fns::messages::DeleteHistory,
    ) -> InvokeResult<tl_types::messages::AffectedHistory> {
        let tl_enums::messages::AffectedHistory::AffectedHistory(affected) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(affected)
    }

    async fn channels_delete_history(
        &self,
        req: tl_fns::channels::DeleteHistory,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_forward_messages(
        &self,
        req: tl_fns::messages::ForwardMessages,
    ) -> InvokeResult<tl_enums::Updates> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_read_history(
        &self,
        req: tl_fns::messages::ReadHistory,
    ) -> InvokeResult<tl_types::messages::AffectedMessages> {
        let tl_enums::messages::AffectedMessages::AffectedMessages(affected) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(affected)
    }

    async fn channels_read_history(
        &self,
        req: tl_fns::channels::ReadHistory,
    ) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_set_typing(&self, req: tl_fns::messages::SetTyping) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_save_draft(&self, req: tl_fns::messages::SaveDraft) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn updates_get_state(&self) -> InvokeResult<tl_types::updates::State> {
        let tl_enums::updates::State::State(state) = self
            .invoke(&tl_fns::updates::GetState)
            .await
            .map_err(Into::into)?;
        Ok(state)
    }

    async fn updates_get_difference(
        &self,
        req: tl_fns::updates::GetDifference,
    ) -> InvokeResult<tl_enums::updates::Difference> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn updates_get_channel_difference(
        &self,
        req: tl_fns::updates::GetChannelDifference,
    ) -> InvokeResult<tl_enums::updates::ChannelDifference> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn account_update_status(
        &self,
        req: tl_fns::account::UpdateStatus,
    ) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn langpack_get_languages(
        &self,
        req: tl_fns::langpack::GetLanguages,
    ) -> InvokeResult<Vec<tl_enums::LangPackLanguage>> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn langpack_get_lang_pack(
        &self,
        req: tl_fns::langpack::GetLangPack,
    ) -> InvokeResult<tl_types::LangPackDifference> {
        let tl_enums::LangPackDifference::LangPackDifference(diff) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(diff)
    }

    async fn langpack_get_difference(
        &self,
        req: tl_fns::langpack::GetDifference,
    ) -> InvokeResult<tl_types::LangPackDifference> {
        let tl_enums::LangPackDifference::LangPackDifference(diff) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(diff)
    }

    async fn langpack_get_language(
        &self,
        req: tl_fns::langpack::GetLanguage,
    ) -> InvokeResult<tl_types::LangPackLanguage> {
        let tl_enums::LangPackLanguage::LangPackLanguage(lang) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(lang)
    }

    async fn langpack_get_strings(
        &self,
        req: tl_fns::langpack::GetStrings,
    ) -> InvokeResult<Vec<tl_enums::LangPackString>> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn channels_get_channels(
        &self,
        req: tl_fns::channels::GetChannels,
    ) -> InvokeResult<tl_enums::messages::Chats> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn messages_get_full_chat(
        &self,
        req: tl_fns::messages::GetFullChat,
    ) -> InvokeResult<tl_types::messages::ChatFull> {
        let tl_enums::messages::ChatFull::ChatFull(full) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(full)
    }

    async fn contacts_get_contacts(
        &self,
        req: tl_fns::contacts::GetContacts,
    ) -> InvokeResult<tl_enums::contacts::Contacts> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn contacts_resolve_username(
        &self,
        req: tl_fns::contacts::ResolveUsername,
    ) -> InvokeResult<tl_types::contacts::ResolvedPeer> {
        let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(resolved)
    }

    async fn contacts_get_blocked(
        &self,
        req: tl_fns::contacts::GetBlocked,
    ) -> InvokeResult<tl_enums::contacts::Blocked> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn contacts_block(&self, req: tl_fns::contacts::Block) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn contacts_unblock(&self, req: tl_fns::contacts::Unblock) -> InvokeResult<bool> {
        self.invoke(&req).await.map_err(Into::into)
    }

    async fn account_get_privacy(
        &self,
        req: tl_fns::account::GetPrivacy,
    ) -> InvokeResult<tl_types::account::PrivacyRules> {
        let tl_enums::account::PrivacyRules::PrivacyRules(rules) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(rules)
    }

    async fn account_set_privacy(
        &self,
        req: tl_fns::account::SetPrivacy,
    ) -> InvokeResult<tl_types::account::PrivacyRules> {
        let tl_enums::account::PrivacyRules::PrivacyRules(rules) =
            self.invoke(&req).await.map_err(Into::into)?;
        Ok(rules)
    }
}
