# Telegram Desktop API Request Reference (`tg-api`)

This document inventories every Telegram MTProto/TL API method called by Telegram Desktop
(a Telegram Desktop checkout at commit `c2d0ad9`, API layer 233) for core messaging features,
including exact flags, limits, offsets, retry counts, and debounce/throttle timers.

---

## 1. Login, Intro & Logout

Sources: `intro/intro_phone.cpp`, `intro/intro_code.cpp`, `intro/intro_email.cpp`,
`intro/intro_password_check.cpp`, `intro/intro_qr.cpp`, `intro/intro_signup.cpp`,
`intro/intro_step.cpp`, `intro/intro_widget.cpp`, `main/main_account.cpp`,
`mtproto/mtp_instance.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Nearest DC / Country | `help.getNearestDc` | Sent on `Intro::Widget` start (`EnterPoint::Start`). Calls `suggestMainDcId(nearest_dc)` and updates default country ISO2. Cancelled when user moves to Phone or QR step. |
| Send login code | `auth.sendCode` | `phone_number`, `api_id`, `api_hash` (from config), `settings: codeSettings { flags: 0, allow_flashcall: false, current_number: false, allow_app_hash: false, allow_missed_call: false, allow_firebase: false }`. `.handleFloodErrors()`. Validates `digits > 1` before sending. 1s poll timer cancels request if stuck in negative state >= 1000ms. `PHONE_MIGRATE_X` handled by switching main DC (`setMainDcId`) and retrying. Response `auth.sentCode` stores `phone_code_hash`; `next_type == auth.codeTypeCall` sets `callStatus = Waiting`, `callTimeout = timeout.unwrap_or(60)`s. `auth.sentCodeTypeSetUpEmailRequired` transitions to email setup. `FlashCall`, `MissedCall`, `FirebaseSms`, `SmsWord`, `SmsPhrase` are logged/skipped as unexpected on desktop. |
| Resend code / Phone call | `auth.resendCode` | `flags: 0`, `phone_number`, `phone_code_hash`, `reason: empty`. Triggered either automatically when `callTimeout` reaches `0` (ticked every 1000ms) or manually via "Send code via SMS" (`noTelegramCode`). Ignores `SEND_CODE_UNAVAILABLE`. |
| Cancel code | `auth.cancelCode` | `phone_number`, `phone_code_hash`. Sent when backing out of `CodeWidget`. |
| Setup login email | `account.sendVerifyEmailCode` | `purpose: emailVerifyPurposeLoginSetup { phone_number, phone_code_hash }`, `email`. Returns `account.sentEmailCode { email_pattern, length }`. |
| Verify login email | `account.verifyEmail` | `purpose: emailVerifyPurposeLoginSetup { phone_number, phone_code_hash }`, `verification: emailVerificationCode { code }`. Expects `account.emailVerifiedLogin { email, sent_code }`. |
| Sign in | `auth.signIn` | `flags: f_email_verification` (if logging in with email code) or `f_phone_code` (if SMS/App/Call/Fragment code), `phone_number`, `phone_code_hash`, `phone_code`, `email_verification`. `.handleFloodErrors()`. `SESSION_PASSWORD_NEEDED` triggers `account.getPassword`. `auth.authorizationSignUpRequired` extracts `terms_of_service` and moves to `SignupWidget`. `PHONE_NUMBER_INVALID` / `PHONE_CODE_EXPIRED` / `PHONE_NUMBER_BANNED` go back to phone input. |
| 2FA state | `account.getPassword` | Returns `account.password` with `current_algo`, `srp_id`, `srp_B`, `hint`, `has_recovery`, `has_secure_values`, `secure_random`, `new_algo`, `new_secure_algo`. If `!current_algo || !srp_id || !srp_B`, logs error and stays on step. |
| 2FA SRP check | `auth.checkPassword` | `password: inputCheckPasswordSRP { srp_id, A, M1 }`. `.handleFloodErrors()`. On `SRP_ID_INVALID`: if elapsed since last `SRP_ID_INVALID` >= `kHandleSrpIdInvalidTimeout` (60,000 ms), refreshes `account.getPassword` and retries; otherwise shows server error. `PASSWORD_HASH_INVALID` / `SRP_PASSWORD_CHANGED` shows bad password error; `PASSWORD_EMPTY` / `AUTH_KEY_UNREGISTERED` goes back. |
| 2FA recovery start | `auth.requestPasswordRecovery` | Sends email recovery code and returns `auth.passwordRecovery { email_pattern }`. |
| 2FA recovery verify | `auth.checkRecoveryPassword` | `code` (trimmed). `.handleFloodErrors()`. On success opens password reset (`auth.recoverPassword` with `code`, optional `new_settings`). `PASSWORD_RECOVERY_EXPIRED` clears email pattern; `CODE_INVALID` shows wrong code error. |
| 2FA recovery finish | `auth.recoverPassword` | `code`, optional `new_settings: account.passwordInputSettings`. Returns `auth.Authorization`. |
| Reset account (forgot 2FA) | `account.deleteAccount` | `flags: 0`, `reason: "Forgot password"`, `password: empty`. On `2FA_CONFIRM_WAIT_<secs>` displays formatted wait time; on `2FA_RECENT_CONFIRM` displays cancelled message. |
| Sign up | `auth.signUp` | `flags: 0`, `phone_number`, `phone_code_hash`, `first_name`, `last_name` (trimmed). Requires accepting `terms_of_service` first if `terms.popup == true`. `.handleFloodErrors()`. |
| QR login export | `auth.exportLoginToken` | `api_id`, `api_hash`, `except_ids: []`. Encodes `token` with base64url as `tg://login?token=<encoded>`. Schedules refresh timer for `max(expires - now, 1) * 1000` ms. On `updateLoginToken`: if request in flight sets `_forceRefresh = true`, else immediately calls `exportLoginToken`. `SESSION_PASSWORD_NEEDED` calls `account.getPassword`. |
| QR login migrate | `auth.importLoginToken` | Sent when `exportLoginToken` returns `auth.loginTokenMigrateTo { dc_id, token }`. Calls `setMainDcId(dc_id)` and invokes `importLoginToken(token)` explicitly `.toDC(dc_id)`. |
| Post-auth preload | `messages.getDialogFilters` | Called by `Intro::Step::finish` before creating `Main::Session`, preloading folders/filters (falls back to empty list on error). |
| Logout | `auth.logOut` | Called by `MTP::Instance::logout` via `Main::Account::logOut()`, followed by session teardown and key cleanup. |

---

## 2. Dialogs & Folders / Chat Filters

Sources: `apiwrap.cpp`, `data/data_histories.cpp`, `data/data_chat_filters.cpp`, `api/api_chat_filters.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Load dialogs page | `messages.getDialogs` | `flags: f_exclude_pinned \| f_folder_id`. `folder_id`: `0` (main) or `1` (archive). `limit`: `kDialogsFirstLoad = 20` when `offset_date == 0` (first page), `kDialogsPerPage = 500` on subsequent pages. `hash = 0`. Initial offsets: `offset_date = 0`, `offset_id = 0`, `offset_peer = inputPeerEmpty`. Next page offsets computed by scanning `dialogs` in reverse for the last `dialog` with non-zero `peer` and `top_message`, matching its `date` in `messages` in reverse; if none found or response is `messages.dialogs` (not `dialogsSlice`), marks `listReceived = true`. Also triggers `requestPinnedDialogs(folder)` if `!pinnedReceived`. |
| Load pinned dialogs | `messages.getPinnedDialogs` | `folder_id: 0` or `1`. Clears and replaces pinned order for folder. Folder load completes only when both `listReceived && pinnedReceived`. |
| Reorder pinned dialogs | `messages.reorderPinnedDialogs` | `flags: f_force`, `folder_id`, `order: Vector<InputDialogPeer>`. |
| Reorder pinned forum topics | `messages.reorderPinnedForumTopics` | `flags: f_force`, `peer`, `order: Vector<int>`. |
| Reorder pinned saved dialogs | `messages.reorderPinnedSavedDialogs` | `flags: f_force`, `order: Vector<InputDialogPeer>`. |
| Archive / unarchive chat | `folders.editPeerFolders` | `folder_peers: [inputFolderPeer { peer, folder_id: 1 or 0 }]`. Cancels any prior in-flight archive request for the same history. |
| Single / batch dialog entries | `messages.getPeerDialogs` | Used for: (1) `Histories::requestDialogEntry` (batched on next event loop tick, postponed while delete/send/read requests are in flight for that history), (2) folder dialog entry `inputDialogPeerFolder(folder_id)`, (3) filter exceptions loading (`kLoadExceptionsPerRequest = 100` after `kLoadExceptionsAfter = 100` dialogs loaded or full list loaded). |
| Mark dialog unread/read | `messages.markDialogUnread` | `flags: f_unread` (if marking unread) `\| f_parent_peer` (for monoforum sublist), `parent_peer`, `peer: inputDialogPeer`. |
| Load dialog filters | `messages.getDialogFilters` | Loads all `DialogFilter` / `DialogFilterDefault` / `DialogFilterChatlist` entries and `tags_enabled`. Reloaded on `updateDialogFilters` or out-of-sync `updateDialogFilterOrder`. |
| Update / delete dialog filter | `messages.updateDialogFilter` | `flags: f_filter` (omitted to delete), `id`, `filter`. |
| Reorder dialog filters | `messages.updateDialogFiltersOrder` | `order: Vector<int>`. Chained `.afterRequest(_saveOrderAfterId)` when following a filter save. |
| Toggle filter tags | `messages.toggleDialogFilterTags` | `enabled: Bool`. |
| Suggested filters | `messages.getSuggestedDialogFilters` | Cached for `kRefreshSuggestedTimeout = 7200s` (2 hours). |
| Shareable folder links | `chatlists.checkChatlistInvite`, `chatlists.joinChatlistInvite`, `chatlists.getChatlistUpdates`, `chatlists.joinChatlistUpdates`, `chatlists.hideChatlistUpdates`, `chatlists.getExportedInvites`, `chatlists.editExportedInvite`, `chatlists.deleteExportedInvite` | `getChatlistUpdates` polled every `chatlist_update_period` (default `3600s`). |

---

## 3. History & Message Fetching

Sources: `apiwrap.cpp`, `data/data_search_controller.cpp`, `data/data_histories.cpp`, `data/data_replies_list.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Load history slice | `messages.getHistory` | `limit = kHistoryLimit = 50`, `offset_date = 0`, `max_id = 0`, `min_id = 0`, `hash = 0`. Deduplicated per `(peer, around_id, direction)`. Postponed/cancelled if a `Delete` request is in flight for that history. Offset rules (`offset_id` clamped to `[0, 0x3FFFFFFF]`): <br>• `Before`: `offset_id = msg_id`, `add_offset = 0` <br>• `Around`: `offset_id = msg_id`, `add_offset = -25` (`-limit / 2`) <br>• `After`: `offset_id = msg_id + 1`, `add_offset = -50` (`-limit`) <br>Empty slice expands `noSkipRange`: `Before -> [0, till]`, `Around -> [0, ServerMaxMsgId]`, `After -> [from, ServerMaxMsgId]`. |
| Load replies / topic history | `messages.getReplies` | `limit = kMessagesPerPage = 50`. Same `Before`/`Around`/`After` `add_offset` rules (`0`, `-25`, `-50`). |
| Load saved messages sublist | `messages.getSavedHistory` | `flags: f_parent_peer` (for monoforum), `limit = 50`, same `add_offset` rules. |
| Jump to date / timestamp | `messages.getHistory` / `messages.getReplies` / `messages.getSavedHistory` | `offset_id = 0`, `offset_date = when - 1`, `add_offset = -1`, `limit = 1`, `max_id = 0`, `min_id = 0`, `hash = 0`. Returns first message with `date >= when - 1`; falls back to `ShowAtUnreadMsgId`. If migrated supergroup, checks migrated legacy chat first. |
| Fetch messages by ID | `messages.getMessages` / `channels.getMessages` | Batched via `requestMessageData` with `.afterDelay(kSmallDelayMs)` (`5ms`). Channels use `channels.getMessages(inputChannel, ids)`; chats/users use `messages.getMessages(ids)`. Also used by `resolveWebPages` with `5ms` delay. |
| Chat list fake last message | `messages.getHistory` | `offset_id = 0, offset_date = 0, add_offset = 0, limit = 2`. |
| Album around item | `messages.getHistory` | `kMaxAlbumCount = 10`: `offset_id = id, add_offset = -10, limit = 19`. |

---

## 4. Sending, Editing, Deleting & Forwarding

Sources: `apiwrap.cpp`, `api/api_editing.cpp`, `api/api_sending.cpp`, `data/data_histories.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Send text message | `messages.sendMessage` | Flags: `f_no_webpage`, `f_silent`, `f_clear_draft`, `f_reply_to`, `f_entities`, `f_schedule_date`, `f_schedule_repeat_period`, `f_send_as`, `f_quick_reply_shortcut`, `f_effect`, `f_invert_media`, `f_allow_paid_stars`, `f_suggested_post`, `f_rich_message`. Splits long text into chunks of `messageLengthCurrent()` (4096 default). Each chunk gets a local client `MsgId` and a 64-bit `random_id` registered in `messageIdByRandomId` and `messageSentData`. Requests for the same history are serialized via `.afterRequest(history->sendRequestId)`. Response `Updates` maps `random_id -> real_id` via `updateMessageID` or `updateShortSentMessage`. On `MESSAGE_EMPTY`, local item is destroyed; on other failures, `random_id` is unregistered, item marked failed, and text restored to draft if draft is empty. |
| Send media | `messages.sendMedia` | Same send flags as `sendMessage`, plus `media: InputMedia` (or `inputMediaPaidMedia` when `price > 0`, or `inputMediaWebPage` for manual webpage preview). Serialized per history via `sendPreparedMessage`. |
| Upload media for album | `messages.uploadMedia` | Uploads individual album items to get server `photo`/`document` handles before calling `messages.sendMultiMedia`. |
| Send album (multi-media) | `messages.sendMultiMedia` | Used when album has >= 2 items (`< 2` falls back to `messages.sendMedia`). Each `inputSingleMedia` carries its own `random_id`. |
| Send inline bot result | `messages.sendInlineBotResult` | Flags: `f_clear_draft`, `f_reply_to`, `f_silent`, `f_schedule_date`, `f_quick_reply_shortcut`, `f_hide_via`, `f_send_as`, `f_allow_paid_stars`. |
| Start bot | `messages.startBot` | Used when bot has non-empty `startToken`; otherwise sends `/start` via `messages.sendMessage`. |
| Edit message | `messages.editMessage` | Flags: `f_message` (if text non-empty or has media), `f_media` (new media or webpage), `f_no_webpage`, `f_invert_media`, `f_entities`, `f_schedule_date`, `f_schedule_repeat_period`, `f_quick_reply_shortcut_id`, `f_rich_message`. Retries once on `FILE_REFERENCE_*` after `refreshFileReference`. |
| Delete messages | `messages.deleteMessages` / `channels.deleteMessages` | Channels call `channels.deleteMessages(inputChannel, id)`. Non-channels call `messages.deleteMessages(flags: f_revoke?, id)`. Applies returned `messages.AffectedMessages` (`pts`, `pts_count`). Scheduled messages use `messages.deleteScheduledMessages`; quick replies use `messages.deleteQuickReplyMessages`. |
| Clear / delete history | `messages.deleteHistory` / `channels.deleteHistory` / `channels.deleteChannel` / `messages.deleteChat` / `messages.deleteChatUser` | Non-channel clear/delete loops `messages.deleteHistory` (flags: `f_just_clear`, `f_revoke`, `f_min_date`, `f_max_date`) while `AffectedHistory.offset > 0`. Channel clear uses `channels.deleteHistory(f_for_everyone?, channel, max_id)`. Channel creator delete uses `channels.deleteChannel`. Legacy chat leave calls `messages.deleteChatUser(chat, self)` then `deleteHistory`; creator delete calls `messages.deleteChat`. Retries dialog entry lookup up to `kDeleteHistoryRetryLimit = 8`. |
| Delete all from participant | `channels.deleteParticipantHistory` | Loops while `AffectedHistory.offset > 0`. |
| Forward messages | `messages.forwardMessages` | Batches consecutive items sharing the same source `from_peer`. Flags: `f_silent`, `f_schedule_date`, `f_schedule_repeat_period`, `f_quick_reply_shortcut`, `f_effect`, `f_drop_author`, `f_drop_media_captions`, `f_send_as`, `f_suggested_post`, `f_top_msg_id`, `f_reply_to`, `f_from_ephemeral`, `f_allow_paid_stars`. Generates a unique `random_id` per forwarded message; creates a client-side local message when `count == 1 && PreserveInfo`. |

---

## 5. Read History, Typing & Drafts

Sources: `data/data_histories.cpp`, `apiwrap.cpp`, `api/api_send_progress.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Read inbox history | `messages.readHistory` / `channels.readHistory` | Throttled by `kReadRequestTimeout = 3000ms` when unread messages still remain in the chat. Sent immediately (no delay) when `force == true`, `stillUnread == 0`, or `stillUnread` is unknown. Deduplicated against `sentReadTill` and `willReadTill`. Non-channels apply returned `AffectedMessages` (`pts`, `pts_count`). |
| Mark message contents read | `messages.readMessageContents` / `channels.readMessageContents` | Marks voice/video notes and mentions as read. Non-channels apply `AffectedMessages`. |
| Report message delivery | `messages.reportMessagesDelivery` | Batched up to `kReportDeliveriesPerRequest = 50` IDs per peer. |
| Typing / upload action | `messages.setTyping` | `flags: f_top_msg_id` if in a forum topic. Throttled: `kSendMyTypingInterval = 5000ms` (for typing/uploading), `kSendMySpeakingInterval = 3000ms` (for group call speaking); stores deadline `now + 2 * interval` and skips while `deadline > now + interval`. Auto-cancels typing after `kCancelTypingActionTimeout = 5000ms`. Skipped for self, broadcast channels (except speaking), non-support bots, and users offline for > `kSendTypingsToOfflineFor = 30s` (unless `lastseen.isRecently()`). |
| Save cloud draft | `messages.saveDraft` | Debounced by `kSaveCloudDraftTimeout = 1000ms`. Flags: `f_no_webpage`, `f_media`, `f_reply_to`, `f_entities`, `f_suggested_post`, `f_rich_message`. If draft changes while a save request is in flight, sets `changedWhileSaving = true` and reschedules after completion. Blocks app quit (`isQuitPrevent`) until all pending/in-flight draft saves finish. Retries once on `FILE_REFERENCE_*` for rich drafts. |

---

## 6. Uploads, Downloads & File References

Sources: `storage/file_upload.cpp`, `storage/download_manager_mtproto.cpp`, `apiwrap.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Upload small file (<= 10 MB) | `upload.saveFilePart` | `file_id`, `file_part`, `bytes`. Part sizes scale by document size (max `kDocumentMaxPartsCountDefault = 4000` parts): 32 KB (< 1 MB), 64 KB (<= 32 MB), 128 KB (<= 375 MB), 256 KB (<= 750 MB), 512 KB (> 750 MB). Max `kMaxUploadPerSession = 1 MB` in flight per session; up to `kMaxSessionsCount = 8` sessions; `kUploadRequestInterval = 250ms`; idle sessions killed after `kKillSessionTimeout = 15s`. |
| Upload big file (> 10 MB) | `upload.saveBigFilePart` | `file_id`, `file_part`, `file_total_parts`, `bytes`. Same part-size tiers and session scaling. |
| Download file chunk | `upload.getFile` | `flags: f_cdn_supported`. `limit = kDownloadPartSize = 128 * 1024` (128 KB). Starts at `kStartWaitedInSession = 4 * 128 KB`, scales up to `kMaxWaitedInSession = 16 * 128 KB`, up to `kMaxSessionsCount = 8` download sessions per DC. Idle sessions killed after `15s`. |
| Refresh file reference | `messages.getMessages`, `channels.getMessages`, `messages.getScheduledMessages`, `stories.getStoriesByID`, `photos.getUserPhotos`, `users.getFullUser`, `channels.getFullChannel`, `messages.getFullChat`, `messages.getPeerDialogs`, `messages.getStickerSet`, `messages.getRecentStickers`, `messages.getFavedStickers`, `messages.getSavedGifs`, `account.getWallPaper`, `account.getTheme`, `account.getSavedRingtones`, `help.getPremiumPromo`, `messages.getWebPage` | Deduplicated by `Data::FileOrigin`. Refreshes expired `FILE_REFERENCE_*` bytes on photos/documents and notifies all waiting download/edit handlers. |

---

## 7. Updates Loop & Presence

Sources: `api/api_updates.cpp`, `data/data_pts_waiter.cpp`.

| Feature | TL Method | Flags, Limits & Exact Behavior |
|---|---|---|
| Initial sync state | `updates.getState` | Called on `Api::Updates` construction while `_ptsWaiter.requesting == true`. Sets initial `pts`, `date`, `qts`, `seq`, starts `kNoUpdatesTimeout = 60s` watchdog, triggers `requestDialogs()` and `updateOnline()`. |
| Fetch global difference | `updates.getDifference` | `flags: 0`, `pts`, `date`, `qts` (limits omitted). Triggered on `updatesTooLong`, `new_session_created`, missing peer/user data in short updates, or after `PtsWaiter::kWaitForSkippedTimeout = 1000ms` on pts/seq gap (1ms if `last < count`). Processes `UpdateMessageID` first, then `new_messages`, then `other_updates`. Loops on `updates.differenceSlice`. On failure retries with exponential backoff: 1s, 2s, 4s, ..., capped at 64s. |
| Fetch channel difference | `updates.getChannelDifference` | `flags: f_force` (cleared when short-polling without a skipped-pts gap), `channel`, `filter: channelMessagesFilterEmpty` (or `channelMessagesFilter(ranges)` for range validation after `tooLong`), `pts`, `limit = kChannelGetDifferenceLimit = 100`. Loops while `!final`. Active channel short-poll uses `timeout * 1000` ms or `kWaitForChannelGetDifference = 1000ms`. Exponential backoff 1..64s on failure. |
| Online/offline status | `account.updateStatus` | `offline: Bool`. Sent when active/idle state flips or every `serverConfig.onlineUpdatePeriod`. Idle check polls every `900ms`. Going offline triggers `saveCurrentDraftToCloud()`. Blocks quit (`isQuitPrevent`) while sending offline status. |

---

## 8. Contacts, Search, Profile, Settings, Privacy, Sessions, Stickers & Reactions

Sources: `apiwrap.cpp`, `api/api_peer_search.cpp`, `api/api_messages_search.cpp`, `data/data_search_controller.cpp`,
`api/api_blocked_peers.cpp`, `api/api_peer_photo.cpp`, `api/api_authorizations.cpp`, `api/api_user_privacy.cpp`,
`api/api_global_privacy.cpp`, `api/api_self_destruct.cpp`, `data/data_message_reactions.cpp`, `api/api_who_reacted.cpp`.

| Category | TL Methods | Flags, Limits & Timers |
|---|---|---|
| **Contacts** | `contacts.getContacts(hash: 0)` | Loads full contact list once per session (triggered by `requestMoreDialogsIfNeeded`). |
| | `contacts.getStatuses()` | Refreshes online/lastseen statuses when LastSeen privacy changes. |
| | `contacts.getBlocked(flags: 0, offset, limit)` | First slice `kBlockedFirstSlice = 16`, subsequent pages `kBlockedPerPage = 40`. |
| | `contacts.block(flags: 0, id)`, `contacts.unblock(flags: 0, id)` | Deduplicated per peer. |
| **Search** | `messages.search` | In-chat search: `kSearchPerPage = 50`, `hash = 0`, optional `f_from_id`, `f_saved_peer_id`, `f_top_msg_id`, `f_saved_reaction`. Shared media search: `kSharedMediaLimit = 100` (`0` when `messageId == 0`), same `Before`/`Around`/`After` `add_offset` rules. Search debounce `kDefaultSearchTimeoutMs = 200ms`. |
| | `messages.searchGlobal` | `flags: f_folder_id`, `folder_id = 0` (excludes archive), `limit = 100` (`0` on first request without offsetPeer). |
| | `contacts.search` | Optional `f_broadcasts` / `f_bots`, `limit = 50` (typed) or `SearchPeopleLimit`. |
| | `contacts.getSponsoredPeers` | Requested when `query.len() >= 4` (`kMinSponsoredQueryLength = 4`). |
| **Profile & Peers** | `users.getFullUser`, `messages.getFullChat`, `channels.getFullChannel` | Deduplicated per peer in `_fullPeerRequests`. Self validation retries after `10,000ms` if mismatch. |
| | `messages.getPeerSettings` | Requested once per non-user peer when `barSettings` unknown. |
| | `account.updateProfile` | Bio updates set `f_about`; cancels prior in-flight bio request if text changed. |
| | `photos.uploadProfilePhoto`, `photos.deletePhotos`, `photos.getUserPhotos` | `getUserPhotos` uses `limit = 100` (or `1` with `offset = -1` for file-reference refresh). |
| | `channels.joinChannel`, `channels.leaveChannel`, `messages.migrateChat`, `messages.getCommonChats(limit: 100)` | Deduplicated per channel/chat. |
| **Settings** | `account.getNotifySettings` | Initialized on session start for `inputNotifyUsers`, `inputNotifyChats`, `inputNotifyBroadcasts`. |
| | `account.updateNotifySettings` | Debounced by `kNotifySettingSaveTimeout = 1000ms`, sent with `.afterDelay(kSmallDelayMs)` (`5ms`). |
| | `account.getContactSignUpNotification`, `account.setContactSignUpNotification` | Contact join notification toggle. |
| | `account.getAccountTTL`, `account.setAccountTTL`, `messages.getDefaultHistoryTTL`, `messages.setDefaultHistoryTTL` | Account self-destruct days and default auto-delete message timer. |
| | `help.getTermsOfServiceUpdate`, `help.acceptTermsOfService` | Polled on session start and re-scheduled clamped to `[10s, 86400s]`. |
| | `account.getWallPaper` | Cancels previous request if slug changes. |
| **Privacy** | `account.getPrivacy`, `account.setPrivacy` | Up to 14 `InputPrivacyKey` types (`PhoneCall`, `ChatInvite`, `PhoneNumber`, `AddedByPhone`, `StatusTimestamp`, `PhoneP2P`, `Forwards`, `ProfilePhoto`, `VoiceMessages`, `About`, `Birthday`, `StarGiftsAutoSave`, `NoPaidMessages`, `SavedMusic`). |
| | `account.getGlobalPrivacySettings`, `account.setGlobalPrivacySettings` | Archive-and-mute, hide read time, paid/premium DMs, disallowed gift types. |
| **Sessions** | `account.getAuthorizations`, `account.resetAuthorization(hash)`, `auth.resetAuthorizations()`, `account.setAuthorizationTTL(days)`, `account.changeAuthorizationSettings(f_call_requests_disabled, hash, ...)` | Lists active sessions, terminates single/all other sessions, updates session TTL and call settings. |
| **Stickers** | `messages.getAllStickers`, `messages.getMaskStickers`, `messages.getEmojiStickers`, `messages.getRecentStickers`, `messages.getFavedStickers`, `messages.getFeaturedStickers`, `messages.getFeaturedEmojiStickers`, `messages.getSavedGifs` | Hash-validated fetches (`Api::Count*Hash`). |
| | `messages.getStickerSet(id, hash: 0)`, `messages.installStickerSet`, `messages.uninstallStickerSet`, `messages.reorderStickerSets`, `messages.clearRecentStickers` | Batched with `.afterDelay(5ms)`. Reorder sent only after all install/uninstall requests settle. |
| | `messages.getStickers(emoticon, hash)` | Cached with `kStickersByEmojiInvalidateTimeout = 6000ms`. |
| | `messages.readFeaturedStickers` | Debounced by `kReadFeaturedSetsTimeout = 1000ms`. |
| **Reactions** | `messages.sendReaction` | `flags: f_reaction` (when non-empty) `\| f_add_to_recent`. Cancels any prior in-flight reaction request for the same `FullMsgId`. |
| | `messages.getMessagesReactions`, `messages.getMessageReactionsList`, `messages.deleteParticipantReaction`, `messages.deleteParticipantReactions` | Reaction polling, who-reacted pagination, and admin reaction removal. |
