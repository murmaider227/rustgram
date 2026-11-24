# `tg-app::bots` — Bot UI Widgets & Integration Guide

This module implements the egui bot UI widgets on top of `crates/tg-bots` without editing `crates/tg-app/src/history/`. All data flows through [`backend::BotsBackend`], with an offline fixture in [`mock::MockBotsBackend`] and a standalone interactive demo host in [`AppBotsHost`] (`Ctrl+Shift+K`).

## Module Map

| File | Responsibility | tdesktop Origin |
|---|---|---|
| `backend.rs` | `BotsBackend` trait, `BotChatSnapshot`, `BotChatMessage`, `BotPeerCandidate`, `SubmittedPoll` | `api/api_bot.h`, `bot_attach_web_view.h` |
| `mock.rs` | `MockBotsBackend` demo bot (`@catalog_demo_bot`) covering all inline/reply button kinds, inline gallery & list results, commands, attach menu bots, URL auth, and game scores | Offline fixture |
| `inline_keyboard.rs` | `show_inline_keyboard`, `InlineKeyboardState`, `InlineKeyboardParams`, `InlineKeyboardActivation` | `history/history_item_components.cpp`, `history/view/history_view_element.cpp` |
| `reply_keyboard.rs` | `ReplyKeyboardWidget`, `paint_reply_keyboard_toggle_button`, `ReplyKeyboardActivation` | `chat_helpers/bot_keyboard.cpp`, `history/history_widget.cpp` |
| `inline_results.rs` | `InlineResultsPanel`, `InlineResultsEvent` (400 ms debounce, mosaic gallery + list mode, `switch_pm`, `next_offset` pagination, all 12 result kinds) | `inline_bots/inline_results_widget.cpp`, `inline_results_inner.cpp`, `inline_bot_layout_internal.cpp` |
| `menus.rs` | `paint_bot_menu_button`, `show_bot_commands_popup`, `show_bot_info_header`, `show_attach_menu_bots_popup` | `history_view_bot_menu_button.cpp`, `field_autocomplete.cpp`, `history_view_empty_list_bubble.cpp`, `bot_attach_web_view.cpp` |
| `boxes.rs` | `BotModalBox`, `show_bot_modal_box`, `paint_bot_toast` (callback alert/toast, URL confirm, URL auth with "allow to send messages", 2FA password, RequestPhone/Location/Poll/Peer, Share Game Score, Attach disclaimer) | `boxes/url_auth_box.cpp`, `api/api_bot.cpp`, `window/window_peer_menu.cpp` |
| `ui_helpers.rs` | Semantic bot button colors (`BotButtonColors`), corner icons, animated loading spinner, procedural art/map thumbnails | `ui/chat/chat.style` |
| `mod.rs` | `BotsDemoScreen` + `AppBotsHost` (`Ctrl+Shift+K` overlay) + unit tests + screenshot generator | — |

## Exact Integration Points for `crates/tg-app/src/history/`

Because another agent owns `history/`, wire the widgets at these exact locations when integrating:

### 1. Inline Keyboard Under a Message Bubble
- **File**: `crates/tg-app/src/history/bubble.rs` (right after painting the message bubble frame) and `crates/tg-app/src/history/inner.rs` (store an `InlineKeyboardState` field on `HistoryInnerWidget` and pass bubble width / animation clock).
- **Call**:
  ```rust
  use crate::bots::inline_keyboard::{
      INLINE_KEYBOARD_TOP_SKIP, InlineKeyboardParams, show_inline_keyboard,
  };

  if let Some(markup) = message.reply_markup.as_ref().filter(|m| m.is_inline()) {
      ui.add_space(INLINE_KEYBOARD_TOP_SKIP);
      let params = InlineKeyboardParams::for_message(
          FullMsgId::new(peer_id, msg_id),
          message_bot_id,
          bot_verified,
          bubble_rect.width(),
          ui.input(|i| i.time),
      );
      if let Some(activation) = show_inline_keyboard(
          ui,
          markup,
          params,
          &mut self.inline_kb_state,
          palette,
      ) {
          // Dispatch `activation.action` (Callback -> set_loading + RPC; Url -> BotModalBox::UrlConfirm;
          // Auth -> requestUrlAuth + BotModalBox::UrlAuth; CopyText -> clipboard + toast;
          // SwitchInline -> fill compose input with `@bot query`).
      }
  }
  ```

### 2. Reply Keyboard Replacing / Below the Compose Area
- **File**: `crates/tg-app/src/history/compose.rs` (compose bar row) and `crates/tg-app/src/history/mod.rs` (`HistoryWidget` state).
- **State**: Add `reply_keyboard: ReplyKeyboardWidget` on `HistoryWidget`. Whenever the active peer changes or a new message arrives with `defines_reply_keyboard()`, call:
  ```rust
  self.reply_keyboard.update_markup(Some(full_msg_id), Some(&markup), false);
  ```
- **Compose placeholder**: Pass `self.reply_keyboard.placeholder()` into `ComposeControls` as the input field hint text.
- **Compose toggle button**: Inside `ComposeControls::show` (next to the Emoji/Attach icons), render:
  ```rust
  if self.reply_keyboard.has_markup()
      && paint_reply_keyboard_toggle_button(
          ui,
          palette,
          self.reply_keyboard.is_shown(),
          true,
      )
      .clicked()
  {
      self.reply_keyboard.toggle_visible();
  }
  ```
- **Keyboard panel**: Immediately below `ComposeControls::show` in `HistoryWidget::show_with_backend`, call:
  ```rust
  if let Some(activation) = self.reply_keyboard.show(ui, bot_id, topic_root_id, palette) {
      // Handle SendCommand / ConfirmSharePhone / ShowLocationUnavailable / CreatePoll / ChoosePeer / OpenWebView
  }
  ```

### 3. Inline Bot Results Panel Above the Compose Field
- **File**: `crates/tg-app/src/history/compose_extras.rs` (replaces the lightweight `InlineBotPanelState` hook) and `crates/tg-app/src/history/compose.rs`.
- **On compose text change**:
  ```rust
  self.inline_results.sync_from_compose_text(
      &compose_text,
      resolved_bot_id,
      &resolved_bot_username,
      active_peer_id,
      now_ms,
  );
  ```
- **Per frame**: Call `self.inline_results.poll_debounce(now_ms, bots_backend)` and render `self.inline_results.show(ui, panel_width, max_height, palette, bots_backend)` anchored directly above `self.last_compose_rect`.

### 4. Bot Commands Menu Button, Commands Popup, Empty Bot Info Header & Attach Menu Bots
- **Bot Menu Button**: In `crates/tg-app/src/history/compose.rs`, at the left edge of the compose bar (before the text field), call `paint_bot_menu_button(ui, &bot_info.menu_button, !commands.is_empty(), commands_popup_open, compact, palette)`.
  - `BotMenuButtonAction::ToggleCommandsPopup` toggles `show_bot_commands_popup(ui, BotCommandsPopupParams { ... }, palette)` anchored above the compose bar.
  - `BotMenuButtonAction::OpenWebApp { text, url }` opens the Mini App via `WebViewHostRequest::OpenMiniApp`.
- **Empty Bot Chat Info Header**: In `crates/tg-app/src/history/inner.rs`, when the active peer is a bot and `messages.is_empty()`, render `show_bot_info_header(ui, &bot_name, &bot_username, &bot_info, palette)` centered inside the empty history canvas.
- **Attach Menu Bots**: In `crates/tg-app/src/history/compose_boxes.rs` (or the compose Attach popup), render `show_attach_menu_bots_popup(ui, &attach_menu_store, peer_id, peer_kind, palette)` to display enabled Mini App bots alongside standard attachments.

### 5. Modal Boxes & Callback Toast
- **File**: `crates/tg-app/src/history/mod.rs` (modal layer in `HistoryWidget::show_with_backend`).
- Store `bot_modal: Option<BotModalBox>` and render `show_bot_modal_box(ui, full_rect, modal, &candidates, palette)` for:
  - `CallbackAlert`, `UrlConfirm`, `UrlAuth` ("Allow \<Bot\> to send you messages" checkbox), `CallbackPassword`, `RequestPhone`, `RequestLocation`, `RequestPoll`, `RequestPeer`, `ShareGameScore`, and `AttachBotDisclaimer`.
- Paint callback toasts via `paint_bot_toast(ui.painter(), full_rect, msg)` (or route into `HistoryAction::Toast`).

### 6. Standalone `Ctrl+Shift+K` Overlay in `crates/tg-app/src/app.rs`
To activate the `Ctrl+Shift+K` demo overlay in the running `rustgram` binary, add one call in `TgforkApp::ui` right beside `AppPaymentsHost` / `AppBusinessHost`:
```rust
crate::bots::AppBotsHost::update_and_render(
    &ctx,
    &self.controller.palette(),
    self.controller.theme_mode(),
);
```
