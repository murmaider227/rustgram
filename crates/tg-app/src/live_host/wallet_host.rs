//! Shell-host `AppWalletHost` bindings for `LiveWalletBackend`.

use std::sync::Arc;

use tg_app_live::wallet::{
    LiveWalletBackend, RpcOnrampProvider, wallet_app_config_from_env, wallet_dir_for_account,
};
use tg_session::AccountManager;

use crate::live_host::multi_account::MultiAccountWorkspaceHost;
use crate::wallet::AppWalletHost;
use crate::window::controller::WindowController;
use crate::window::deep_links::WalletDeepLinkAction;

/// Installs a per-account [`LiveWalletBackend`] into [`AppWalletHost`] for `slot` and kicks off
/// an asynchronous `help.getAppConfig` sync to apply tdesktop's wallet availability flags.
pub fn install_live_wallet_for_account(
    ctx: &egui::Context,
    slot: i32,
    host: &MultiAccountWorkspaceHost,
    manager: &AccountManager,
) {
    let Some(entry) = host.account(slot) else {
        return;
    };
    let session = entry.workspace.live_session();
    let key_part = manager.account_key_file_part(slot);
    let wallet_dir = wallet_dir_for_account(manager.working_dir(), &key_part);
    let account_id = entry.user_id.0.max(1);
    let config = wallet_app_config_from_env();
    let mut backend = LiveWalletBackend::new(
        account_id,
        wallet_dir,
        session.runtime().tokio_handle().clone(),
        config,
    );
    backend.attach_mtproto_rpc(session.runtime().rpc().clone(), 4);
    backend.set_onramp_provider(Arc::new(RpcOnrampProvider::new(
        session.runtime().rpc().clone(),
    )));
    backend.spawn_app_config_sync(session);
    AppWalletHost::set_account_backend(ctx, slot, Box::new(backend), slot == host.active_slot());
}

/// Ensures every running live account in `host` has its own [`LiveWalletBackend`] installed in
/// [`AppWalletHost`], switches the active wallet panel when accounts change, and removes
/// logged-out accounts.
pub fn sync_live_wallets_for_host(
    ctx: &egui::Context,
    host: &MultiAccountWorkspaceHost,
    manager: &AccountManager,
) {
    let slots = host.slot_indices();
    AppWalletHost::retain_accounts(ctx, &slots);
    for slot in slots {
        if !AppWalletHost::has_account_backend(ctx, slot) {
            install_live_wallet_for_account(ctx, slot, host, manager);
        }
    }
    AppWalletHost::activate_account(ctx, host.active_slot());
}

/// Dispatches a [`WalletDeepLinkAction`] (`tc://`, `tg://sendgrams`, `ton://transfer`) into
/// [`AppWalletHost`], showing an error toast on `controller` if the wallet is disabled or
/// the link fails validation.
pub fn handle_wallet_deep_link(
    ctx: &egui::Context,
    action: &WalletDeepLinkAction,
    controller: &mut WindowController,
) {
    if let Err(err) = AppWalletHost::handle_deep_link_action(ctx, action) {
        controller.show_toast(err);
    }
}
