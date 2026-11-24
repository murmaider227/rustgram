# Wiring `tg-api` to `tg-tl`, `tg-mtproto`, `tg-crypto`, and `tg-data`

In pass 1, `tg-api` is implemented against clean internal traits (`Invoker`, `SrpCalculator`,
`PtsEngine`) and a `stand_in` module matching the cross-crate contracts in `docs/ARCHITECTURE.md`.
Once `tg-tl`, `tg-mtproto`, `tg-crypto`, and `tg-data` land, wiring them is mechanical and
requires no rewrites of the state machines or business logic.

---

## 1. Stand-in Replacements (`src/stand_in.rs`)

| `tg_api::stand_in` Item | Target Crate & Symbol |
|---|---|
| `PeerId`, `UserId`, `ChatId`, `ChannelId`, `MsgId`, `FullMsgId`, `SERVER_MAX_MSG_ID`, `SHOW_AT_UNREAD_MSG_ID`, `SHOW_AT_THE_END_MSG_ID` | `tg_data::ids::*` (exact same bit-packing: shift 0=User, 1=Chat, 2=Channel, 48-bit bare ID mask `0x0000_FFFF_FFFF_FFFF`) |
| `DcId`, `TimeId`, `FolderId`, `FilterId` | `tg_data` / `tg_mtproto` type aliases |
| `RpcError`, `InvocationError` | `tg_mtproto::{RpcError, InvocationError}` |
| `PasswordKdfAlgoModPow`, `PasswordKdfAlgo`, `SrpCheckInput`, `SrpError`, `SrpCalculator` | Implement `SrpCalculator` as a thin wrapper delegating to `tg_crypto::srp::compute_check` (and password hash helper) |

---

## 2. Implementing `Invoker` Over `tg_mtproto::Client`

Create a concrete `MtprotoInvoker` struct holding `Arc<tg_mtproto::Client>` (and an `AtomicI32`
or client-backed `main_dc_id`), implementing `tg_api::Invoker`:

| `Invoker` Method | `tg_tl::functions` Type | `tg_tl::enums` Return Type |
|---|---|---|
| `help_get_nearest_dc` | `functions::help::GetNearestDc` | `enums::NearestDc` |
| `auth_send_code` | `functions::auth::SendCode` | `enums::auth::SentCode` |
| `auth_resend_code` | `functions::auth::ResendCode` | `enums::auth::SentCode` |
| `auth_cancel_code` | `functions::auth::CancelCode` | `bool` |
| `auth_sign_in` | `functions::auth::SignIn` | `enums::auth::Authorization` |
| `auth_sign_up` | `functions::auth::SignUp` | `enums::auth::Authorization` |
| `auth_check_password` | `functions::auth::CheckPassword` | `enums::auth::Authorization` |
| `auth_request_password_recovery` | `functions::auth::RequestPasswordRecovery` | `enums::auth::PasswordRecovery` |
| `auth_check_recovery_password` | `functions::auth::CheckRecoveryPassword` | `bool` |
| `auth_recover_password` | `functions::auth::RecoverPassword` | `enums::auth::Authorization` |
| `auth_export_login_token` | `functions::auth::ExportLoginToken` | `enums::auth::LoginToken` |
| `auth_import_login_token` | `functions::auth::ImportLoginToken` (via `client.invoke_in_dc(dc_id, ...)`) | `enums::auth::LoginToken` |
| `auth_log_out` | `functions::auth::LogOut` | `enums::auth::LoggedOut` |
| `account_get_password` | `functions::account::GetPassword` | `enums::account::Password` |
| `account_send_verify_email_code` | `functions::account::SendVerifyEmailCode` | `enums::account::SentEmailCode` |
| `account_verify_email` | `functions::account::VerifyEmail` | `enums::account::EmailVerified` |
| `account_delete_account` | `functions::account::DeleteAccount` | `bool` |
| `account_update_status` | `functions::account::UpdateStatus` | `bool` |
| `messages_get_dialogs` | `functions::messages::GetDialogs` | `enums::messages::Dialogs` |
| `messages_get_pinned_dialogs` | `functions::messages::GetPinnedDialogs` | `enums::messages::PeerDialogs` |
| `messages_reorder_pinned_dialogs` | `functions::messages::ReorderPinnedDialogs` | `bool` |
| `messages_get_peer_dialogs` | `functions::messages::GetPeerDialogs` | `enums::messages::PeerDialogs` |
| `messages_get_dialog_filters` | `functions::messages::GetDialogFilters` | `enums::messages::DialogFilters` |
| `messages_mark_dialog_unread` | `functions::messages::MarkDialogUnread` | `bool` |
| `folders_edit_peer_folders` | `functions::folders::EditPeerFolders` | `enums::Updates` |
| `messages_get_history` | `functions::messages::GetHistory` | `enums::messages::Messages` |
| `messages_get_replies` | `functions::messages::GetReplies` | `enums::messages::Messages` |
| `messages_send_message` | `functions::messages::SendMessage` | `enums::Updates` |
| `messages_edit_message` | `functions::messages::EditMessage` | `enums::Updates` |
| `messages_delete_messages` | `functions::messages::DeleteMessages` | `enums::messages::AffectedMessages` |
| `channels_delete_messages` | `functions::channels::DeleteMessages` | `enums::messages::AffectedMessages` |
| `messages_delete_history` | `functions::messages::DeleteHistory` | `enums::messages::AffectedHistory` |
| `channels_delete_history` | `functions::channels::DeleteHistory` | `enums::Updates` |
| `messages_forward_messages` | `functions::messages::ForwardMessages` | `enums::Updates` |
| `messages_read_history` | `functions::messages::ReadHistory` | `enums::messages::AffectedMessages` |
| `channels_read_history` | `functions::channels::ReadHistory` | `bool` |
| `messages_set_typing` | `functions::messages::SetTyping` | `bool` |
| `messages_save_draft` | `functions::messages::SaveDraft` | `bool` |
| `updates_get_state` | `functions::updates::GetState` | `enums::updates::State` |
| `updates_get_difference` | `functions::updates::GetDifference` | `enums::updates::Difference` |
| `updates_get_channel_difference` | `functions::updates::GetChannelDifference` | `enums::updates::ChannelDifference` |

---

## 3. Module-by-Module Wiring Notes

### `src/login.rs`
- Pass `ApiCredentials { api_id, api_hash }` loaded from runtime configuration (never embedded).
- Wire `SrpCalculator` to `tg_crypto::srp`:
  - `compute_password_hash` maps to PBKDF2-HMAC-SHA512 (100,000 iterations) + SHA-256 salt wrapping per `core_cloud_password.cpp:ComputeHash`.
  - `compute_check` maps to `tg_crypto::srp::compute_check`.
- Wire `mtproto_client.updates_receiver()` to forward `UpdateLoginToken` into `LoginStateMachine::on_login_token_update`.
- On `LoginStep::Authorized { authorization, preloaded_filters }`, hand `authorization.user` and `preloaded_filters` to `tg_data::Session` initialization and persist auth keys via `tg_storage`.

### `src/dialogs.rs`
- Convert `DialogsResponse` / `PeerDialogsResponse` users, chats, messages, and dialog entries into `tg_data::Session::apply_dialogs` / `process_users` / `process_chats`.
- Wire `DialogFiltersResponse` into `tg_data::ChatFilters`.

### `src/history.rs`
- Feed `HistorySlice { message_ids, messages, no_skip_range, full_count, channel_pts }` into `tg_data::History::add_slice` and `channel.pts_received`.

### `src/messaging.rs`
- Connect `MessageSender` local pending messages and `resolve_update_message_id` to `tg_data::Session` message registry (`next_local_msg_id`, `register_message_random_id`, `set_real_id`).
- Connect `ReadHistoryManager` to `tg_data::History` unread counts and apply returned `AffectedMessages` (`pts`, `pts_count`) through `UpdatesDriver::apply_pts_only`.
- Drive `ReadHistoryManager::flush_due`, `DraftManager::flush_due`, and `SendProgressManager` typing cancel timer from a `tokio::time` task or UI tick loop.

### `src/updates.rs`
- Spawn a tokio task reading `tg_mtproto::Client`'s `Receiver<UpdatesMsg>`, decoding `tg_tl::enums::Updates` into `UpdatesBatch`, and calling `UpdatesDriver::handle_updates`.
- Implement `PtsEngine` directly on `tg_data`'s pts state machine (or keep `DefaultPtsWaiter` wrapped by `tg_data::Session` to apply decoded updates to peers/histories/messages).
- Drive `UpdatesDriver::check_timeouts` on a `tokio::time::interval` (or deadline sleep) to fire `getDifference` / `getChannelDifference` when 1000ms gap timers or exponential backoff retry timers expire, and send MTProto ping when `NO_UPDATES_TIMEOUT` (60s) elapses.

---

## 4. Remaining Work After Dependencies Land

1. Add `tg-tl`, `tg-mtproto`, `tg-crypto`, `tg-data` to `crates/tg-api/Cargo.toml`.
2. Delete `src/stand_in.rs` and replace imports with `tg_data`, `tg_mtproto`, and `tg_crypto` types.
3. Implement `MtprotoInvoker` (`src/tl_invoker.rs`) with `From`/`TryFrom` conversions between `tg_api::types::*` and `tg_tl::{types, enums, functions}`.
4. Implement `CryptoSrpCalculator` wrapping `tg_crypto::srp`.
5. Add remaining secondary API wrappers as needed by `tg-app`: file upload/download chunk orchestrators (`upload.saveFilePart`/`saveBigFilePart`/`getFile` + `refreshFileReference`), full-peer/profile/photo, contacts, search, privacy, authorizations, stickers, reactions, and notify settings.
