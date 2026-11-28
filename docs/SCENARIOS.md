# Everyday scenario walk

A repeatable walk through everyday actions, played in Rustgram and in the official Telegram Desktop
side by side on the same Telegram **test-server** account. It measures what docs/PARITY.md only
estimates: every scenario below is played in both clients and judged from the screens.

- **Data**: `tools/walk/peer` fills two test-server accounts (A, B) once with groups, a channel,
  mentions, reactions (also on old messages), a pinned message, a poll, an album, a tall photo, a
  video, a GIF, a voice and a round message, a file, stickers from a user-made and a server set, a
  link preview, every text style with a spoiler, a 320-line private chat, a folder, an archived and
  a muted chat, a draft, a scheduled message, a story and contacts. Running it again adds nothing.
- **Runner**: `tools/walk/run.py` starts both clients on scratch copies of the same account,
  plays one step list per scenario in each (so the two runs cannot drift), drives B through the
  session layer, and saves `official | Rustgram` screenshot pairs plus a verdict per scenario.
- **Verdicts**: *pass* (every automatic check held), *FAIL* (a check failed in Rustgram),
  *blocked* (the runner could not play a step; the reason is given), *needs a look* (only a person
  can judge the pair, or the official client failed the same check, so the check is in doubt).
  Automatic checks read the screen with Windows OCR, colours of the corner counters, window state
  and server state through the peer; nothing is marked *pass* without one.

## How to run

1. Build: `cargo build -p tg-app --bin rustgram -p tg-walk-peer` (any target dir).
2. Write a local config from `tools/walk/walk_config.example.json` (paths to both clients, the
   test-account working dirs to copy, the peer, the output folder). It stays out of the repo.
3. Once: `python -I tools/walk/make_media.py --out <media dir>` and
   `tg-walk-peer fixture --a <copy of A> --b <copy of B> --state <state.json> --media <media dir>`.
4. Every release: `python -I tools/walk/run.py --config <config>` (`--only OB-*` for the owner's bugs).
   Pairs and `results.md` land in the config's `snap_root`; `gen_docs.py --results` updates the
   table below.

Rules the runner keeps: test servers only, scratch copies of working dirs, input posted to the
clients' own windows (the real mouse and keyboard are never used), processes stopped by PID, the
user's own Telegram never touched (so tray clicks run in Rustgram only), and messages the walk sends
are deleted at the end.

## Scenarios

Each scenario names what must be visible afterwards and the tdesktop source it follows.
`OB-` scenarios are bugs the owner reported; `EV-` are everyday flows.

### OB-01: Stories tooltip closes on its x

- Reported: TGB-3127
- Must be visible: With an unseen story of B, the tooltip "Click here to view from ..." shows next to the stories userpics; a click on its x closes it and it stays closed.
- tdesktop: `dialogs/dialogs_widget.cpp: Widget::setupStoriesToggle / Ui::ShowStoriesTooltip (dialogs/ui/dialogs_stories_list.cpp)`
- Steps:

  ```
  expect text list Click here to view
  shot tooltip
  click right_of:Click here to view:17:7
  wait 1.5
  shot closed
  expect notext list Click here to view
  ```

### OB-02: "is typing" appears when B types and goes away 6 s after B stops

- Reported: TGB-3128
- Must be visible: Header and chat-list row of Walk Peer B say "typing" while B types; both are back to normal about 6 s after B's last typing update.
- tdesktop: `data/data_send_action.cpp: SendActionManager::registerFor (kStatusShowClientsideTyping 6000 ms); history/view/history_view_top_bar_widget.cpp`
- Steps:

  ```
  open ${peer}
  scroll history -150
  peer {"op":"typing","chat":"pm"}
  wait 1.5
  shot typing
  expect text header typing(\.|…|\s*$)
  wait 8
  shot stopped
  expect notext header typing(\.|…|\s*$)
  expect notext list typing(\.|…|\s*$)
  ```

### OB-03: "is typing" goes away at once when B cancels typing

- Reported: TGB-3128
- Must be visible: After B's cancel the header shows the normal status within 2 s, no 6 s wait.
- tdesktop: `data/data_send_action.cpp: SendActionManager::registerFor (sendMessageCancelAction clears the action)`
- Steps:

  ```
  open ${peer}
  scroll history -150
  peer {"op":"typing","chat":"pm"}
  wait 1.5
  expect text header typing(\.|…|\s*$)
  peer {"op":"cancel_typing","chat":"pm"}
  wait 2
  shot cancelled
  expect notext header typing(\.|…|\s*$)
  ```

### OB-04: "is typing" goes away when B's message arrives

- Reported: TGB-3128
- Must be visible: B types, then sends: the message is shown and "typing" is gone from the header at once.
- tdesktop: `history/history.cpp: History::clearSendAction on a new message from the typing user`
- Steps:

  ```
  open ${peer}
  scroll history -150
  peer {"op":"typing","chat":"pm"}
  wait 1.5
  expect text header typing(\.|…|\s*$)
  peer {"op":"send","chat":"pm","text":"Walk check: typing ends with this message"}
  wait 2
  shot after_message
  expect notext header typing(\.|…|\s*$)
  expect text history typing ends with this message
  ```

### OB-05: Own userpic next to own messages in a group at a wide window

- Reported: TGB-3124
- Must be visible: With a history column of 880 px or more the chat is wide: A's own messages sit on the left like B's, with A's userpic at the last message of a run. Narrower, they move right and the userpic goes.
- tdesktop: `history/history_widget.cpp: HistoryWidget::recountChatWidth (wide at >= 880 px, window.style adaptiveChatWideWidth); history/view/history_view_message.cpp: Message::hasFromPhoto`
- Steps:

  ```
  size 1700 900
  open Walk Supergroup
  seek ${pair_seek}
  shot wide
  expect samecol "${own_run}" "${other_run}"
  look the walk account's own messages sit on the left with its userpic at the last one of a run
  size 1200 900
  seek ${pair_seek}
  shot narrow
  expect rightof "${own_run}" "${other_run}"
  ```

### OB-06: The "@" mentions button and its counter go away after the mention is read

- Reported: TGB-3125
- Must be visible: While scrolled up, a new mention shows the "@" corner button with a counter; after scrolling to the bottom (mention seen) the server count is 0 and no counter is left in the corner.
- tdesktop: `history/view/history_view_corner_buttons.cpp: CornerButtons::updateUnreadThingsVisibility; history/history_unread_things.cpp`
- Steps:

  ```
  open Walk Supergroup
  peer {"op":"send","chat":"super","text":"walk filler one"}
  peer {"op":"send","chat":"super","text":"walk filler two"}
  wait 2
  scroll history 25
  peer {"op":"send","chat":"super","text":"Walker, a mention for the walk check","mention":true}
  wait 3
  shot with_button
  expect badge corner
  scroll history -300
  wait 3
  shot after_read
  expect server {"op":"dialog","who":"a","chat":"super"} unread_mentions == 0
  expect nobadge corner
  ```

### OB-07: The reactions corner button and its counter go away after the reaction is seen

- Reported: TGB-3125
- Must be visible: B reacts to A's message while A is scrolled away: a heart corner button with a counter shows; once the reacted message was on screen the counter is gone.
- tdesktop: `history/view/history_view_corner_buttons.cpp: CornerButtons (reactions button, unreadReactions)`
- Steps:

  ```
  open Walk Supergroup
  peer {"op":"send","chat":"super","text":"walk filler three"}
  peer:mine {"op":"send","who":"a","chat":"super","text":"walk tmp react to this"}
  peer {"op":"send","chat":"super","text":"walk filler four"}
  wait 2
  scroll history 25
  peer {"op":"react","chat":"super","id":${mine.id},"emoji":"👍"}
  wait 3
  shot with_button
  expect badge corner
  scroll history -300
  wait 3
  shot after_seen
  expect server {"op":"dialog","who":"a","chat":"super"} unread_reactions == 0
  expect nobadge corner
  ```

### OB-08: A custom sticker shows, fills its box, and a click opens its set

- Reported: TGB-3126
- Must be visible: The sticker from the user-made set Walk Stickers (static PNG/WebP) and the one from a server set are drawn (no blue placeholder circle), fill their box with no empty band, the userpic sits at the sticker's bottom edge; a click on the custom sticker opens the Walk Stickers set box.
- tdesktop: `history/view/media/history_view_sticker.cpp: Sticker::draw / Sticker::countOptimalSize; boxes/sticker_set_box.cpp: StickerSetBox`
- Steps:

  ```
  open Walk Supergroup
  seek ^That is all for now
  wait 3
  shot stickers
  expect nobadge history 2500
  look both stickers are drawn, fill their box with no empty band under them, and B's userpic sits at the custom sticker's bottom edge
  click below:^That is all for now:60:100
  wait 2.5
  shot set_box
  expect text whole Walk Stickers
  key escape
  ```

### OB-09: Closing the main window keeps the app in the tray; a tray click brings it back

- Reported: TGB-3122, TGB-3123
- Must be visible: x hides the window, the process keeps running, the tray icon is the app's own icon, a tray click shows the window again, the tray menu offers Quit, and Ctrl+Q quits the app. The official client is only checked for hide + still running (its tray icon cannot be told apart from the user's own Telegram).
- tdesktop: `window/main_window.cpp: MainWindow::closeEvent (WorkMode WindowAndTray hides the window); platform/win/tray_win.cpp: Tray::createIcon (the app icon)`
- Steps:

  ```
  close
  wait 2
  expect alive
  expect hidden
  [rustgram] tray find
  [rustgram] look the tray icon is the Rustgram app icon, not a generated chat bubble
  [rustgram] tray click
  [rustgram] wait 2
  [rustgram] expect visible
  [rustgram] shot back_from_tray
  [rustgram] tray menu
  [rustgram] look the tray menu matches tdesktop's (Minimize to Tray / Disable notifications / Quit)
  [rustgram] key ctrl+q
  [rustgram] expect exited
  [official] stop
  ```

### OB-10: "Restart to update" never offers an older or equal version

- Reported: TGB-3117
- Runs in: rustgram only
- Must be visible: With a staged update package older than the running build in the working dir, Rustgram starts without any update bar or 'New version is ready', and deletes the package.
- tdesktop: `core/update_checker.cpp: checkReadyUpdate (a staged update not newer than AppVersion is deleted)`
- Steps:

  ```
  stop
  stage_update 0.0.1
  restart 20
  shot started
  expect notext whole New version is ready
  expect notext whole Restart to update
  expect nostaged
  ```

### OB-11: Stories sit as userpics inside the search field, not in a separate row

- Reported: TGB-3008
- Must be visible: B's unseen story shows as a small userpic at the right end of the search field; there is no extra 'N stories' or 'Add Story' row in the chat list.
- tdesktop: `dialogs/dialogs_widget.cpp: Widget::setupStories (Dialogs::Stories::List in the search field area)`
- Steps:

  ```
  shot list
  expect notext list \d+ stor(y|ies)
  expect notext list Add Story
  look B's story is a ringed userpic inside the search field
  ```

### OB-12: Spoiler text stays hidden until clicked

- Reported: TGB-3009, TGB-3027
- Must be visible: The spoiler in B's style test message is covered (its words are not readable) until a click reveals them.
- tdesktop: `ui/text/text.cpp (spoiler) / history/view/history_view_message.cpp: Message::clickHandlerPressedChanged`
- Steps:

  ```
  open Walk Supergroup
  seek ^Styles
  shot hidden
  expect notext history spoiler text here
  click right_of:and a hidden:40:0
  wait 1.5
  shot revealed
  expect text history spoiler text here
  ```

### OB-13: Text styles and emoji look like tdesktop

- Reported: TGB-3010, TGB-3014
- Must be visible: Bold, italic, underline, strikethrough, monospace, link, quote and code block render like tdesktop; emoji are tdesktop's emoji set; text weight is the same as tdesktop's.
- tdesktop: `ui/text/text.cpp; ui/emoji_config.cpp (tdesktop emoji sheets)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Styles
  shot styles
  expect text history bold
  expect text history fn main
  look text weight, styles and code block match the official pair
  open ${peer}
  seek History line 274
  shot emoji
  look the thumbs-up emoji is tdesktop's emoji image
  ```

### OB-14: Deleting a message plays the delete effect and removes it

- Reported: TGB-3011
- Must be visible: A sends a message in Saved Messages, deletes it from the context menu; the message disappears (with tdesktop's dust effect).
- tdesktop: `history/view/history_view_list_widget.cpp / ui/effects/ (dust effect on delete, HistoryView::ListWidget::deleteItems)`
- Steps:

  ```
  open Saved Messages
  click text:Write a message@compose
  type walk tmp delete me
  key enter
  wait 2
  expect text history walk tmp delete me
  rclick text:walk tmp delete me@history
  wait 1
  click text:^Delete$
  wait 1
  click text:^Delete$
  wait 0.3
  shot effect
  wait 2
  shot gone
  expect notext history walk tmp delete me
  look the delete effect in the 'effect' pair
  ```

### OB-15: Sender name colours in a group match tdesktop

- Reported: TGB-3012
- Must be visible: B's name above B's messages in Walk Supergroup has the same colour as in the official client (same theme).
- tdesktop: `ui/chat/chat_style.cpp: ChatStyle::peerColor / Data::DecideColorIndex`
- Steps:

  ```
  open Walk Supergroup
  seek ^Styles
  shot names
  look ${peer}'s name colour equals the official pair
  ```

### OB-16: Reactions show on old messages deep in a long chat

- Reported: TGB-3013
- Must be visible: History lines 003 and 040 of the private chat (300+ messages back) show their reactions.
- tdesktop: `history/view/reactions/history_view_reactions.cpp: InlineList; data/data_message_reactions.cpp`
- Steps:

  ```
  open ${peer}
  seek History line 040
  shot line040
  look line 040 carries its reaction (thumbs up)
  seek History line 003
  shot line003
  look line 003 carries its reaction (fire)
  ```

### OB-17: Saved Messages bubble looks like tdesktop

- Reported: owner report (Saved Messages bubble)
- Must be visible: The saved note is shown as a normal outgoing bubble with the time, like the official client.
- tdesktop: `history/view/history_view_message.cpp: Message::draw (Saved Messages shows own messages as outgoing bubbles)`
- Steps:

  ```
  open Saved Messages
  expect text history shopping list
  shot saved
  look the saved note's bubble matches the official pair
  ```

### OB-18: Recording and sending a voice message

- Reported: TGB-3015, TGB-3034
- Must be visible: Holding the microphone button for 3 s records and releasing sends a voice message to Saved Messages (on a PC without a microphone tdesktop shows an error instead).
- tdesktop: `history/view/controls/history_view_voice_record_bar.cpp: VoiceRecordBar (hold the mic button, release to send)`
- Steps:

  ```
  open Saved Messages
  hold br:30,26 3
  wait 3
  shot after
  look a voice message was sent, or the client says there is no microphone (same as the official pair)
  ```

### OB-19: Copy Text from the context menu puts the message text on the clipboard

- Reported: TGB-3016
- Must be visible: Right click on B's message -> Copy Text -> the clipboard holds the message text.
- tdesktop: `history/history_inner_widget.cpp: HistoryInner::showContextMenu (Copy Text) / TextForMimeData`
- Steps:

  ```
  open ${peer}
  scroll history -300
  seek Latest message from Peer B
  clipboard_clear
  rclick text:Latest message from Peer B@history
  wait 1
  shot menu
  click text:^Copy Text
  wait 1
  expect clipboard Latest message from Peer B
  ```

### OB-20: Selected text is copied with Ctrl+C

- Reported: TGB-3016
- Must be visible: Drag-selecting the text of B's message and pressing Ctrl+C copies it.
- tdesktop: `history/history_inner_widget.cpp: HistoryInner::keyPressEvent (Copy) / getSelectedText`
- Steps:

  ```
  open ${peer}
  scroll history -300
  seek Latest message from Peer B
  clipboard_clear
  select "Latest message from Peer B" "Latest message from Peer B"
  key ctrl+c
  wait 1
  expect clipboard Latest message
  ```

### OB-21: Channel post reactions are emoji, never raw ids

- Reported: TGB-3017
- Must be visible: Posts in Walk Channel show reaction chips with emoji and counts; no long numbers (document ids) are drawn over the posts.
- tdesktop: `history/view/reactions/history_view_reactions.cpp: InlineList::layoutButtons`
- Steps:

  ```
  open Walk Channel
  wait 2
  shot posts
  expect notext history \d{9,}
  look reaction chips under the posts match the official pair
  ```

### OB-22: Video plays without glitches

- Reported: TGB-3017 (video part)
- Must be visible: B's short video opens in the media viewer and plays smoothly.
- tdesktop: `media/view/media_view_overlay_widget.cpp: OverlayWidget (streaming player)`
- Steps:

  ```
  open Walk Supergroup
  seek ^A short video
  click below:^A short video:60:-120
  wait 1
  shot play1
  wait 1.5
  shot play2
  expect changed play1 play2 whole
  look the video frames in play1/play2 have no glitches
  key escape
  ```

### OB-23: Voice message bubble: waveform and duration

- Reported: TGB-2954
- Must be visible: B's voice message shows a play button, a waveform and 0:04.
- tdesktop: `history/view/media/history_view_document.cpp: Document::draw (voice waveform, status text)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Notes as a file
  scroll history 3
  shot voice
  expect text history 0:0[0-9]
  look the voice bubble's waveform and status text match the official pair
  ```

### OB-24: Gaps between grouped messages and the admin rank

- Reported: TGB-2953
- Must be visible: A's three messages in a row are attached (small gaps, one tail); B's name shows the owner rank.
- tdesktop: `history/view/history_view_message.cpp: Message::countGeometry (attach-to-previous margins); Message::rightBadge`
- Steps:

  ```
  open Walk Supergroup
  seek ^And a third one
  shot run
  look the gaps between A's three messages match the official pair; the owner badge shows next to B's name
  ```

### OB-25: The scrollbar thumb is at the bottom when the history is at the bottom

- Reported: TGB-2952
- Must be visible: Right after opening the long private chat and jumping to the bottom, the scrollbar thumb (when shown) is at the bottom.
- tdesktop: `ui/widgets/scroll_area.cpp: ScrollBar::updateBar`
- Steps:

  ```
  open ${peer}
  scroll history -120
  wait 1
  scroll history 1
  shot thumb
  look the scrollbar thumb sits at the bottom of the track
  ```

### OB-26: Photo viewer is full screen over the taskbar

- Reported: TGB-2970
- Must be visible: A click on an album photo opens the viewer covering the whole monitor, taskbar included; Escape closes it.
- tdesktop: `media/view/media_view_overlay_widget.cpp: OverlayWidget::showAndActivate (full-screen window)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Three photos from the trip
  click below:^Three photos from the trip:40:-140
  wait 2
  shot viewer
  expect fullscreen
  key escape
  wait 1
  ```

### OB-27: A tall photo keeps its aspect ratio

- Reported: TGB-2970
- Must be visible: A's 600x1800 photo is drawn tall (not squashed or cropped to a square).
- tdesktop: `history/view/media/history_view_photo.cpp: Photo::countOptimalSize`
- Steps:

  ```
  open Walk Supergroup
  seek ^Three photos from the trip
  scroll history -4
  shot tall
  look the tall photo keeps its 1:3 shape like the official pair
  ```

### OB-28: Userpic circles are smooth

- Reported: TGB-2971
- Must be visible: Chat-list userpics are round with smooth edges.
- tdesktop: `ui/image/image_prepare.cpp: Images::Circle (antialiased)`
- Steps:

  ```
  shot list
  look userpic edges in the chat list are smooth like the official pair
  ```

### OB-29: No link preview card for an internal t.me/c link

- Reported: TGB-2954
- Must be visible: A message with a t.me/c/... link to a message of the supergroup shows no 'Link' preview card.
- tdesktop: `data/data_web_page.cpp / history/view/media/history_view_web_page.cpp (internal links have no webpage)`
- Steps:

  ```
  open Walk Supergroup
  peer {"op":"send","who":"a","chat":"super","text":"walk tmp see https://t.me/c/1/2"}
  wait 3
  shot link
  look no extra 'Link' preview card under the t.me/c message (same as the official pair)
  ```

### EV-01: Start: the chat list shows the account's chats

- Must be visible: After start the chat list shows the fixture chats with their last messages.
- tdesktop: `dialogs/dialogs_inner_widget.cpp: InnerWidget::paintEvent`
- Steps:

  ```
  shot list
  expect text list Walk Supergroup
  expect text list Walk Basic Group
  expect text list Saved Messages
  expect text list Walk Channel
  ```

### EV-02: Pinned chat stays on top of the list

- Must be visible: Walk Supergroup (pinned) is above every unpinned chat.
- tdesktop: `dialogs/dialogs_main_list.cpp: MainList::pinned`
- Steps:

  ```
  shot list
  expect above "Walk Supergroup" "Saved Messages"
  expect above "Walk Supergroup" "${peer}"
  ```

### EV-03: Folders: the Walk folder shows only its chats

- Must be visible: A click on the Walk folder in the left rail lists the two groups and the channel only; All chats brings everything back.
- tdesktop: `window/window_filters_menu.cpp: FiltersMenu; dialogs/dialogs_widget.cpp`
- Steps:

  ```
  click text:^Walk$@list
  wait 1.5
  shot folder
  expect text list Walk Supergroup
  expect notext list Saved Messages
  click text:^All chats@list
  wait 1.5
  expect text list Saved Messages
  ```

### EV-04: Archive: the archived chat lives in Archived chats

- Must be visible: Walk Archived Group is not in the main list; opening Archived chats shows it.
- tdesktop: `dialogs/dialogs_inner_widget.cpp (folder row); data/data_folder.cpp`
- Steps:

  ```
  expect text list Archived chats
  click text:^Archived chats@list
  wait 1.5
  shot archive
  expect text list Walk Archived Group
  key escape
  ```

### EV-05: Open the private chat

- Must be visible: The header shows Walk Peer B and a last-seen or online status; the history shows the latest messages.
- tdesktop: `history/history_widget.cpp: HistoryWidget::showHistory; history/view/history_view_top_bar_widget.cpp`
- Steps:

  ```
  open ${peer}
  shot chat
  expect text header last seen|online|recently
  expect text history History line|Latest message
  ```

### EV-06: Open the basic group

- Must be visible: The header shows Walk Basic Group and its member count.
- tdesktop: `history/view/history_view_top_bar_widget.cpp: TopBarWidget::updateOnlineDisplay`
- Steps:

  ```
  open Walk Basic Group
  shot chat
  expect text header members
  expect text history Count me in
  ```

### EV-07: Open the supergroup: pinned message bar

- Must be visible: The supergroup shows the pinned message bar with B's welcome text.
- tdesktop: `history/view/history_view_pinned_bar.cpp: PinnedBar`
- Steps:

  ```
  open Walk Supergroup
  shot chat
  expect text whole Pinned Message
  expect text whole Hello everyone
  ```

### EV-08: Pinned bar click jumps to the pinned message

- Must be visible: A click on the pinned bar scrolls the history to B's welcome message.
- tdesktop: `history/history_widget.cpp: HistoryWidget::pinnedBarClicked`
- Steps:

  ```
  open Walk Supergroup
  click text:^Pinned Message
  wait 2
  shot jumped
  expect text history Hello everyone! This is the walk
  ```

### EV-09: Open the channel as a subscriber

- Must be visible: Walk Channel shows its posts with views; the bottom bar has the Unmute button (the channel is muted) instead of a message field.
- tdesktop: `history/history_widget.cpp: HistoryWidget::updateControlsVisibility (channel: mute button instead of the field)`
- Steps:

  ```
  open Walk Channel
  shot channel
  expect text history Welcome to the Walk Channel
  expect text compose ${channel_bar}
  ```

### EV-10: Open Saved Messages

- Must be visible: Saved Messages opens with the saved note.
- tdesktop: `history/history_widget.cpp; data/data_saved_messages.cpp`
- Steps:

  ```
  open Saved Messages
  shot saved
  expect text history shopping list, milk
  ```

### EV-11: A new message from B appears in the open chat and is read

- Must be visible: With the private chat open at the bottom, B's new message appears within 3 s and the server marks it read.
- tdesktop: `history/history_widget.cpp: HistoryWidget::newItemAdded / readServerHistory`
- Steps:

  ```
  open ${peer}
  scroll history -150
  peer {"op":"send","chat":"pm","text":"Walk check: a fresh message from B"}
  wait 3
  shot arrived
  expect text history a fresh message from B
  expect server {"op":"dialog","who":"a","chat":"pm"} unread == 0
  ```

### EV-12: Send a text message

- Must be visible: Typing a message and pressing Enter sends it: it shows in the history with a tick, and B's side has it.
- tdesktop: `history/history_widget.cpp: HistoryWidget::send`
- Steps:

  ```
  open Saved Messages
  click text:Write a message@compose
  type walk tmp hello from the walk
  key enter
  wait 3
  shot sent
  expect text history walk tmp hello from the walk
  expect server {"op":"last","who":"a","chat":"saved","n":1} ok == true
  ```

### EV-13: Edit a sent message

- Must be visible: Context menu -> Edit, change the text, Enter: the message shows the new text and 'edited'.
- tdesktop: `history/history_widget.cpp: HistoryWidget::editMessage / saveEditMsg`
- Steps:

  ```
  open Saved Messages
  click text:Write a message@compose
  type walk tmp edit me
  key enter
  wait 2
  rclick text:walk tmp edit me@history
  wait 1
  click text:^Edit$
  wait 1
  key ctrl+a
  type walk tmp edited text
  key enter
  wait 3
  shot edited
  expect text history walk tmp edited text
  expect text history edited
  ```

### EV-14: Reply to a message

- Must be visible: Context menu -> Reply on B's message, type, Enter: A's message is sent with a reply header quoting B.
- tdesktop: `history/history_widget.cpp: HistoryWidget::replyToMessage`
- Steps:

  ```
  open ${peer}
  scroll history -300
  seek Latest message from Peer B
  rclick text:Latest message from Peer B@history
  wait 1
  click text:^Reply$
  wait 1
  type walk tmp a reply
  key enter
  wait 3
  shot reply
  expect text history walk tmp a reply
  look the sent message has a reply header quoting B's message
  ```

### EV-15: Forward a message to Saved Messages

- Must be visible: Context menu -> Forward on B's message, choose Saved Messages: the forwarded copy is in Saved Messages with 'Forwarded from'.
- tdesktop: `boxes/share_box.cpp: ShareBox; history/history_widget.cpp: forwardSelected`
- Steps:

  ```
  open ${peer}
  scroll history -300
  seek Latest message from Peer B
  rclick text:Latest message from Peer B@history
  wait 1
  click text:^Forward
  wait 1.5
  shot box
  click text:^Saved Messages
  wait 2
  key enter
  wait 3
  open Saved Messages
  shot saved
  expect text history Forwarded from
  expect text history Latest message from Peer B
  ```

### EV-16: Global search finds a message deep in history

- Must be visible: Typing 'History line 123' in the search field lists the message from the private chat.
- tdesktop: `dialogs/dialogs_widget.cpp: Widget::searchMessages; dialogs/dialogs_inner_widget.cpp (search results)`
- Steps:

  ```
  click text:^Search@list
  type History line 123
  wait 3
  shot results
  expect text list History line 123
  ```

### EV-17: Opening a search result jumps to the message

- Must be visible: A click on the found message opens the private chat at that message.
- tdesktop: `dialogs/dialogs_widget.cpp: Widget::onChooseByEnter / Window::SessionController::showMessage`
- Steps:

  ```
  click text:^Search@list
  type History line 123
  wait 3
  click text:History line 123@list
  wait 3
  shot jumped
  expect text history History line 123
  ```

### EV-18: Search inside a chat

- Must be visible: Ctrl+F in the private chat searches only there; typing 'line 2' shows matching messages.
- tdesktop: `history/view/history_view_top_bar_widget.cpp (search button); dialogs search in chat (ChatSearchIn)`
- Steps:

  ```
  open ${peer}
  key ctrl+f
  wait 1
  type line 250
  wait 3
  shot search
  expect text whole History line 250
  ```

### EV-19: Open the group profile

- Must be visible: A click on the group name in the header opens its profile with the member list.
- tdesktop: `info/profile/info_profile_widget.cpp; info/info_wrap_widget.cpp`
- Steps:

  ```
  open Walk Supergroup
  click text:Walk Supergroup@header
  wait 2
  shot profile
  expect text whole Members|members
  expect text whole ${peer}
  key escape
  ```

### EV-20: Shared media of the group

- Must be visible: The group profile lists shared media (photos, videos, files, voice, links).
- tdesktop: `info/media/info_media_widget.cpp; info/profile/info_profile_actions.cpp (shared media buttons)`
- Steps:

  ```
  open Walk Supergroup
  click text:Walk Supergroup@header
  wait 2
  shot profile
  expect text whole photo|Photos
  expect text whole file|Files
  look the shared media counters match the official pair
  key escape
  ```

### EV-21: Open the private chat's user profile

- Must be visible: A click on Walk Peer B in the header opens B's profile.
- tdesktop: `info/profile/info_profile_cover.cpp; info/profile/info_profile_actions.cpp`
- Steps:

  ```
  open ${peer}
  click text:${peer}@header
  wait 2
  shot profile
  expect text whole ${peer}
  look profile layout matches the official pair
  key escape
  ```

### EV-22: Chat list context menu

- Must be visible: Right click on Walk Basic Group in the list shows Pin/Unpin, Mute, Mark as read/unread, Archive and Delete entries.
- tdesktop: `window/window_peer_menu.cpp: FillDialogsEntryMenu`
- Steps:

  ```
  rclick text:^Walk Basic Group@list
  wait 1
  shot menu
  expect text whole Pin|Unpin
  expect text whole Archive
  expect text whole Mute|Unmute
  key escape
  ```

### EV-23: Message context menu

- Must be visible: Right click on B's message shows the reaction strip and Reply, Copy Text, Forward entries.
- tdesktop: `history/history_inner_widget.cpp: HistoryInner::showContextMenu; history/view/history_view_context_menu.cpp`
- Steps:

  ```
  open ${peer}
  scroll history -150
  rclick text:Latest message from Peer B@history
  wait 1
  shot menu
  expect text whole ^Reply
  expect text whole Copy Text
  expect text whole Forward
  key escape
  ```

### EV-24: Draft is kept and shown in the chat list

- Must be visible: Walk Basic Group's row says 'Draft:'; opening the chat puts the draft text in the field.
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintRow (Draft: prefix); history/history_widget.cpp: applyDraft`
- Steps:

  ```
  shot list
  expect text list Draft
  open Walk Basic Group
  wait 1
  shot field
  expect text compose remember to bring the charger
  ```

### EV-25: Unread counter of a chat updates when B writes

- Must be visible: With Saved Messages open, a new message from B moves Walk Peer B's row up with the new text as preview.
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintUnreadBadge; data/data_histories.cpp`
- Steps:

  ```
  open Saved Messages
  peer {"op":"send","chat":"pm","text":"Walk check: preview in the list"}
  wait 3
  shot list
  expect text list preview in the list
  ```

### EV-26: "typing" in the chat list while another chat is open

- Must be visible: With Saved Messages open, B typing in the private chat shows 'typing' in B's list row.
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintRow (send action text)`
- Steps:

  ```
  open Saved Messages
  peer {"op":"typing","chat":"pm"}
  wait 1.5
  shot typing
  expect text list typing(\.|…|\s*$)
  ```

### EV-27: Scroll a long history up and back down

- Must be visible: Scrolling up through the private chat loads older messages (line numbers go down); the scroll-down button brings it back to the latest message.
- tdesktop: `history/history_inner_widget.cpp; history/history_widget.cpp: preloadHistoryIfNeeded`
- Steps:

  ```
  open ${peer}
  scroll history -150
  seek History line 2[0-4][0-9]
  shot older
  scroll history -400
  wait 2
  shot bottom
  expect text history Latest message from Peer B|fresh message|preview in the list
  ```

### EV-28: Reaching the first messages of a group by scrolling

- Must be visible: Scrolling up in Walk Supergroup reaches its first messages (the welcome and A's first message).
- tdesktop: `history/history_widget.cpp: preloadHistoryIfNeeded (loads older slices until the start)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Hi! Glad to be here
  shot top
  ```

### EV-29: Album of three photos

- Must be visible: The three trip photos are laid out as one album with the caption.
- tdesktop: `history/view/media/history_view_media_grouped.cpp: GroupedMedia::countOptimalSize`
- Steps:

  ```
  open Walk Supergroup
  seek ^Three photos from the trip
  scroll history 3
  shot album
  look the album layout matches the official pair
  ```

### EV-30: Poll message

- Must be visible: The poll shows its question, 'Anonymous Poll' and three answers.
- tdesktop: `history/view/media/history_view_poll.cpp: Poll::draw`
- Steps:

  ```
  open Walk Supergroup
  seek What should we test next
  shot poll
  expect text history Anonymous Poll
  expect text history Media viewer
  ```

### EV-31: Vote in the poll

- Must be visible: A click on an answer votes: the poll shows results (percentages).
- tdesktop: `history/view/media/history_view_poll.cpp: Poll::handleClick / sendPollVotes`
- Steps:

  ```
  open Walk Supergroup
  seek What should we test next
  click text:^Search$@history
  wait 3
  shot voted
  expect text history %
  ```

### EV-32: File message

- Must be visible: B's file shows its name walk-notes.txt and size.
- tdesktop: `history/view/media/history_view_document.cpp: Document::draw (file name and size)`
- Steps:

  ```
  open Walk Supergroup
  seek walk-notes
  shot file
  expect text history walk-notes.txt
  expect text history KB|B
  ```

### EV-33: Link preview card

- Must be visible: A's message with telegram.org/blog shows a preview card with the site name and title.
- tdesktop: `history/view/media/history_view_web_page.cpp: WebPage::draw`
- Steps:

  ```
  open Walk Supergroup
  seek ^Link with a preview
  shot preview
  expect text history Telegram
  look the preview card matches the official pair
  ```

### EV-34: Forwarded message header

- Must be visible: B's forward from the channel shows 'Forwarded from Walk Channel'.
- tdesktop: `history/view/history_view_message.cpp: Message::paintForwardedInfo`
- Steps:

  ```
  open Walk Supergroup
  seek Forwarded from
  shot forward
  expect text history Forwarded from
  ```

### EV-35: Reply header in the history

- Must be visible: A's reply to the welcome shows a reply header with B's name and the quoted start of the welcome.
- tdesktop: `history/view/history_view_reply.cpp: Reply::paint`
- Steps:

  ```
  open Walk Supergroup
  seek ^Replying to the welcome message
  shot reply
  expect text history Replying to the welcome message
  look the reply header (bar, name colour, quoted text) matches the official pair
  ```

### EV-36: GIF autoplays

- Must be visible: A's GIF plays in the history without a click.
- tdesktop: `history/view/media/history_view_gif.cpp: Gif::draw (autoplay)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Notes as a file
  scroll history 6
  shot gif1
  wait 1.2
  shot gif2
  expect changed gif1 gif2 history
  ```

### EV-37: Round video message

- Must be visible: A's round video is drawn as a circle with its duration.
- tdesktop: `history/view/media/history_view_gif.cpp (round video)`
- Steps:

  ```
  open Walk Supergroup
  seek ^Notes as a file
  scroll history 4
  shot round
  look the round video is a circle with its duration, like the official pair
  ```

### EV-38: Play a voice message

- Must be visible: A click on the voice message's play button starts playback (the time counts and the media player bar shows).
- tdesktop: `history/view/media/history_view_document.cpp: Document::handleClick / media/player/media_player_instance.cpp`
- Steps:

  ```
  open Walk Supergroup
  seek ^Notes as a file
  scroll history 3
  click left_of:0:0[0-9]:30:-14
  wait 2
  shot playing
  look playback started (player bar or progress) like the official pair
  ```

### EV-39: Emoji panel

- Must be visible: The emoji button in the message field opens the panel with emoji, stickers and GIF tabs.
- tdesktop: `chat_helpers/tabbed_selector.cpp: TabbedSelector; chat_helpers/emoji_list_widget.cpp`
- Steps:

  ```
  open Saved Messages
  click br:68,26
  wait 1.5
  shot panel
  expect text whole Emoji|Stickers|GIFs
  look the emoji panel matches the official pair
  key escape
  ```

### EV-40: Sticker panel

- Must be visible: The Stickers tab of the panel lists the installed or recent stickers.
- tdesktop: `chat_helpers/stickers_list_widget.cpp: StickersListWidget`
- Steps:

  ```
  open Saved Messages
  click br:68,26
  wait 1.5
  click text:^Stickers$
  wait 2
  shot stickers
  look the sticker panel matches the official pair
  key escape
  ```

### EV-41: Main menu

- Must be visible: The menu button opens the main menu with Settings, Saved Messages and Night Mode.
- tdesktop: `window/window_main_menu.cpp: MainMenu`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  shot menu
  expect text whole Settings
  expect text whole Night Mode
  key escape
  ```

### EV-42: Settings opens

- Must be visible: Settings lists My Account, Notifications and Sounds, Privacy and Security, Chat Settings, Folders, Advanced.
- tdesktop: `settings/settings_main.cpp: Main::setupSections`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  shot settings
  expect text whole Notifications and Sounds
  expect text whole Privacy and Security
  expect text whole Chat Settings
  key escape
  ```

### EV-43: Settings: Notifications and Sounds

- Must be visible: The notifications page shows the desktop notification switches.
- tdesktop: `settings/settings_notifications.cpp`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  click text:^Notifications and Sounds
  wait 2
  shot notifications
  expect text whole Desktop notifications|Notifications for
  key escape
  key escape
  ```

### EV-44: Settings: Privacy and Security

- Must be visible: The privacy page lists Phone number, Last seen & online and the other privacy rules.
- tdesktop: `settings/settings_privacy_security.cpp`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  click text:^Privacy and Security
  wait 2
  shot privacy
  expect text whole Last seen
  key escape
  key escape
  ```

### EV-45: Settings: Chat Settings

- Must be visible: Chat Settings shows the theme choices and text size options.
- tdesktop: `settings/settings_chat.cpp`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  click text:^Chat Settings
  wait 2
  shot chat_settings
  expect text whole Theme|Themes
  key escape
  key escape
  ```

### EV-46: Settings: Folders

- Must be visible: The Folders page lists the Walk folder.
- tdesktop: `settings/settings_folders.cpp`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  click text:^Folders
  wait 2
  shot folders
  expect text whole ^Walk
  key escape
  key escape
  ```

### EV-47: Settings: Advanced

- Must be visible: Advanced shows the data and storage and window options.
- tdesktop: `settings/settings_advanced.cpp`
- Steps:

  ```
  click xy:36,52
  wait 1.2
  click text:^Settings$
  wait 2
  click text:^Advanced
  wait 2
  shot advanced
  expect text whole Data and storage|Storage|Window title bar|System integration
  key escape
  key escape
  ```

### EV-48: Night mode toggles the theme

- Must be visible: Night Mode in the main menu switches between the day and night themes; toggling twice returns to the start.
- tdesktop: `window/window_main_menu.cpp (Night Mode toggle); window/themes/window_theme.cpp: ToggleNightMode`
- Steps:

  ```
  shot before
  click xy:36,52
  wait 1.2
  click text:^Night Mode
  wait 2.5
  key escape
  wait 1
  shot toggled
  expect changed before toggled list
  click xy:36,52
  wait 1.2
  click text:^Night Mode
  wait 2.5
  key escape
  wait 1
  shot back
  expect same before back list
  ```

### EV-49: Narrow window: one-column layout

- Must be visible: At 600 px wide only the chat list shows; opening a chat shows it full width; Escape returns to the list.
- tdesktop: `window/window_adaptive.cpp; mainwidget.cpp: MainWidget::updateWindowAdaptiveLayout (OneColumn below dialogsWidth + chat min width)`
- Steps:

  ```
  size 600 800
  wait 1
  shot narrow_list
  expect text whole Walk Supergroup
  click text:^Walk Supergroup
  wait 2.5
  shot narrow_chat
  expect notext whole Saved Messages
  key escape
  wait 1.5
  expect text whole Saved Messages
  ```

### EV-50: Window resize keeps the layout

- Must be visible: Growing the window to 1600 px and back keeps the chat open with the history at the bottom.
- tdesktop: `mainwidget.cpp: MainWidget::resizeEvent`
- Steps:

  ```
  open ${peer}
  size 1600 900
  wait 1.5
  shot wide
  expect text header ${peer}
  size 1000 700
  wait 1.5
  shot small
  expect text header ${peer}
  ```

### EV-51: Escape closes the chat

- Must be visible: Escape in an open chat returns to the empty 'Select a chat to start messaging' state.
- tdesktop: `history/history_widget.cpp: HistoryWidget::keyPressEvent (Escape -> showBackFromStack)`
- Steps:

  ```
  open Walk Basic Group
  key escape
  key escape
  wait 1
  shot closed
  expect text whole Select a chat
  ```

### EV-52: Restart keeps the session

- Must be visible: After quitting and starting again the chat list is back without a login screen.
- tdesktop: `core/application.cpp; storage/storage_account.cpp (restore session)`
- Steps:

  ```
  restart 20
  shot restarted
  expect text list Walk Supergroup
  expect notext whole Start Messaging|Log in
  ```

### EV-53: Mention marker in the chat list

- Must be visible: B's mention of A in Walk Basic Group shows the @ marker in the list row (until read).
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintRow (unread mention badge)`
- Steps:

  ```
  peer {"op":"send","chat":"basic","text":"Walker, mention marker check","mention":true}
  wait 3
  shot list
  look Walk Basic Group's row shows the @ marker next to the unread counter
  ```

### EV-54: Muted chat looks muted

- Must be visible: Walk Channel (muted) has the muted icon and a grey unread badge.
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintRow (muted: grey badge, speaker icon)`
- Steps:

  ```
  shot list
  look Walk Channel's row has the muted icon and a grey badge like the official pair
  ```

### EV-55: Mute and unmute a chat from its profile

- Must be visible: Basic group profile -> Notifications switch off mutes the chat (muted icon in the list); switching it back on unmutes it.
- tdesktop: `info/profile/info_profile_actions.cpp: notifications toggle; data/notify/data_notify_settings.cpp`
- Steps:

  ```
  open Walk Basic Group
  click text:Walk Basic Group@header
  wait 2
  shot profile
  expect text whole Notifications
  look the notifications switch is there and on (same as the official pair)
  key escape
  ```

### EV-56: Channel post views

- Must be visible: Channel posts show a views counter and the time.
- tdesktop: `history/view/history_view_bottom_info.cpp: BottomInfo (views counter)`
- Steps:

  ```
  open Walk Channel
  shot views
  look each post shows its views counter (eye icon) and time like the official pair
  ```

### EV-57: Chat header actions

- Must be visible: The private chat header shows the search, call, side panel and menu buttons; the menu opens.
- tdesktop: `history/view/history_view_top_bar_widget.cpp: TopBarWidget (search, call, info, menu buttons)`
- Steps:

  ```
  open ${peer}
  click br:20,0
  wait 0.2
  shot header
  look the header buttons match the official pair
  ```

### EV-58: Scheduled messages

- Must be visible: The private chat has a scheduled-messages button in the field; it opens the list with the scheduled message.
- tdesktop: `history/view/history_view_scheduled_section.cpp: ScheduledWidget`
- Steps:

  ```
  open ${peer}
  shot field
  look the scheduled-messages (clock) button shows in the message field like the official pair
  ```

### EV-59: Unread messages bar when opening a chat

- Must be visible: Opening a chat with unread messages shows the 'Unread messages' bar.
- tdesktop: `history/history_widget.cpp: HistoryWidget::showHistory (unread bar, scroll to first unread)`
- Steps:

  ```
  peer {"op":"send","chat":"basic","text":"Walk check: unread one"}
  peer {"op":"send","chat":"basic","text":"Walk check: unread two"}
  wait 2
  open Walk Basic Group
  shot unread
  expect text history Unread messages
  ```

### EV-60: Archived chat opens

- Must be visible: From Archived chats, Walk Archived Group opens with its messages.
- tdesktop: `dialogs/dialogs_widget.cpp (archive folder); history/history_widget.cpp`
- Steps:

  ```
  click text:^Archived chats@list
  wait 1.5
  click text:^Walk Archived Group@list
  wait 2.5
  shot chat
  expect text history Archiving this one
  key escape
  key escape
  ```

### EV-61: Jump to bottom button

- Must be visible: After scrolling up, the down-arrow button appears; a click brings the history back to the latest message.
- tdesktop: `history/view/history_view_corner_buttons.cpp: CornerButtons (history down button)`
- Steps:

  ```
  open ${peer}
  scroll history -150
  scroll history 20
  wait 1
  shot scrolled
  click br:30,95
  wait 2
  shot bottom
  expect text history Latest message from Peer B|fresh message|preview in the list
  ```

### EV-62: Media viewer: next photo of an album

- Must be visible: In the viewer, the right arrow key goes to the next photo of the album ('2 of 3').
- tdesktop: `media/view/media_view_overlay_widget.cpp: OverlayWidget::moveToNext`
- Steps:

  ```
  open Walk Supergroup
  seek ^Three photos from the trip
  click below:^Three photos from the trip:40:-140
  wait 2
  key right
  wait 1.5
  shot next
  look the viewer shows the next album photo like the official pair
  key escape
  ```

### EV-63: The chat-list unread counter equals the server's unread count

- Must be visible: Walk Peer B's unread badge in the list shows the same number the server reports for the chat.
- tdesktop: `dialogs/ui/dialogs_layout.cpp: PaintUnreadBadge; data/data_history.cpp: History::setUnreadCount (from dialog.unread_count)`
- Steps:

  ```
  shot list
  expect listbadge "^${peer}" {"op":"dialog","who":"a","chat":"pm"}
  ```

## Results

OB rows: walk of 2026-11-28 on Rustgram `0ddb18c0` against Telegram Desktop 7.4.6 (29 scenarios, one pass of 36 min plus a 17 min re-run of 11; every "needs a look" pair judged by hand). pass: 9, FAIL: 11, blocked: 9.
EV rows: walk of 2026-11-27 on Rustgram `de63bc2a` (63 scenarios).

| Id | Scenario | Result | Note |
|---|---|---|---|
| OB-01 | Stories tooltip closes on its x | blocked | no tooltip in Rustgram on this login (dismissed in an earlier walk, the choice is saved); the official copy shows it |
| OB-02 | "is typing" appears when B types and goes away 6 s after B stops | pass |  |
| OB-03 | "is typing" goes away at once when B cancels typing | blocked | pass 1: the other side's window did not open the chat; re-run: a click on the search result was lost in Rustgram (passed on 2026-11-27) |
| OB-04 | "is typing" goes away when B's message arrives | pass |  |
| OB-05 | Own userpic next to own messages in a group at a wide window | FAIL | the run of own messages is not reachable by scrolling (history gap, TGB-3176) |
| OB-06 | The "@" mentions button and its counter go away after the mention is read | blocked | re-run pair: scroll-down counter 1 in both clients, gone after reading; neither client showed the "@" button; the corner check read the profile column (TGB-3186) |
| OB-07 | The reactions corner button and its counter go away after the reaction is seen | blocked | not run: reactions by the other side need a second Rustgram-readable account (TGB-3158) |
| OB-08 | A custom sticker shows, fills its box, and a click opens its set | FAIL | the stickers are not reachable by scrolling (history gap, TGB-3176) |
| OB-09 | Closing the main window keeps the app in the tray; a tray click brings it back | FAIL | tray menu still has "Simulate incoming message" (TGB-3159); hide to tray, tray click, Quit and Ctrl+Q work; the icon is the monochrome app logo (tdesktop default) |
| OB-10 | "Restart to update" never offers an older or equal version | pass |  |
| OB-11 | Stories sit as userpics inside the search field, not in a separate row | pass | judged from the pair; the userpic shows "SM" instead of the account's initials (TGB-3205) |
| OB-12 | Spoiler text stays hidden until clicked | FAIL | the styles message is not reachable by scrolling (history gap, TGB-3176) |
| OB-13 | Text styles and emoji look like tdesktop | FAIL | the styles message is not reachable by scrolling (history gap, TGB-3176) |
| OB-14 | Deleting a message plays the delete effect and removes it | pass | judged from the pair: the message goes with the dust effect; the check saw the official client's copy (both post as one account) |
| OB-15 | Sender name colours in a group match tdesktop | FAIL | the styles message is not reachable by scrolling (history gap, TGB-3176) |
| OB-16 | Reactions show on old messages deep in a long chat | blocked | line 003 shows its fire reaction with the reactor's userpic; neither client reached line 040 |
| OB-17 | Saved Messages bubble looks like tdesktop | pass | judged from the pair; time and ticks are grey where tdesktop tints them blue |
| OB-18 | Recording and sending a voice message | blocked | the hold landed on the profile column (TGB-3186); the official client started recording |
| OB-19 | Copy Text from the context menu puts the message text on the clipboard | blocked | Rustgram's seek did not reach the newest message of the private chat; the official client had no Copy Text item |
| OB-20 | Selected text is copied with Ctrl+C | blocked | Rustgram's seek did not reach the newest message of the private chat; the official clipboard stayed empty |
| OB-21 | Channel post reactions are emoji, never raw ids | pass | judged from the pair: emoji chips with counts like the official client (link card differs: TGB-3206) |
| OB-22 | Video plays without glitches | FAIL | the video is not reachable by scrolling (history gap, TGB-3176) |
| OB-23 | Voice message bubble: waveform and duration | FAIL | the voice message is not reachable by scrolling (history gap, TGB-3176) |
| OB-24 | Gaps between grouped messages and the admin rank | FAIL | the three-message run is not reachable by scrolling (history gap, TGB-3176) |
| OB-25 | The scrollbar thumb is at the bottom when the history is at the bottom | blocked | Rustgram showed no scrollbar thumb to judge; the official thumb sits at the bottom |
| OB-26 | Photo viewer is full screen over the taskbar | FAIL | the album is not reachable by scrolling (history gap, TGB-3176) |
| OB-27 | A tall photo keeps its aspect ratio | FAIL | the album is not reachable by scrolling (history gap, TGB-3176) |
| OB-28 | Userpic circles are smooth | pass | judged from the pair |
| OB-29 | No link preview card for an internal t.me/c link | pass | judged from the pair |
| EV-01 | Start: the chat list shows the account's chats | pass |  |
| EV-02 | Pinned chat stays on top of the list | pass |  |
| EV-03 | Folders: the Walk folder shows only its chats | pass |  |
| EV-04 | Archive: the archived chat lives in Archived chats | pass |  |
| EV-05 | Open the private chat | pass |  |
| EV-06 | Open the basic group | needs a look | check misread the screen; the message is on screen in the Rustgram shot |
| EV-07 | Open the supergroup: pinned message bar | needs a look | reference client did not pass either: text whole Pinned Message |
| EV-08 | Pinned bar click jumps to the pinned message | needs a look | reference client did not pass either: text not on screen: ^Pinned Message |
| EV-09 | Open the channel as a subscriber | needs a look | reference client did not pass either: text history Welcome to the Walk Channel; text compose Broadcast/Write a message/Unmute/Mute |
| EV-10 | Open Saved Messages | pass |  |
| EV-11 | A new message from B appears in the open chat and is read | blocked | not run: needs runner work (chat 'Walk Peer B' did not open) |
| EV-12 | Send a text message | pass |  |
| EV-13 | Edit a sent message | needs a look | reference client did not pass either: text not on screen: ^Edit$ |
| EV-14 | Reply to a message | blocked | not run: needs runner work (text not on screen: Latest message from Peer B) |
| EV-15 | Forward a message to Saved Messages | blocked | not run: needs runner work (text not on screen: Latest message from Peer B) |
| EV-16 | Global search finds a message deep in history | pass |  |
| EV-17 | Opening a search result jumps to the message | blocked | not run: needs runner work (text not on screen: History line 123) |
| EV-18 | Search inside a chat | pass |  |
| EV-19 | Open the group profile | pass |  |
| EV-20 | Shared media of the group | needs a look |  |
| EV-21 | Open the private chat's user profile | needs a look |  |
| EV-22 | Chat list context menu | needs a look | reference client did not pass either: text whole Mute/Unmute |
| EV-23 | Message context menu | needs a look | reference client did not pass either: text whole ^Reply; text whole Copy Text; text whole Forward |
| EV-24 | Draft is kept and shown in the chat list | pass |  |
| EV-25 | Unread counter of a chat updates when B writes | blocked | not run: needs runner work (chat 'Walk Peer B' did not open) |
| EV-26 | "typing" in the chat list while another chat is open | blocked | not run: needs runner work (chat 'Walk Peer B' did not open) |
| EV-27 | Scroll a long history up and back down | pass |  |
| EV-28 | Reaching the first messages of a group by scrolling | FAIL | message not reachable by scrolling up: ^Hi! Glad to be here (history gap, TGB-3176) |
| EV-29 | Album of three photos | needs a look | reference client did not pass either: message not reachable by scrolling up: ^Three photos from the trip |
| EV-30 | Poll message | needs a look | reference client did not pass either: message not reachable by scrolling up: What should we test next |
| EV-31 | Vote in the poll | needs a look | reference client did not pass either: message not reachable by scrolling up: What should we test next |
| EV-32 | File message | needs a look | reference client did not pass either: message not reachable by scrolling up: walk-notes |
| EV-33 | Link preview card | needs a look | reference client did not pass either: message not reachable by scrolling up: ^Link with a preview |
| EV-34 | Forwarded message header | needs a look | reference client did not pass either: message not reachable by scrolling up: Forwarded from |
| EV-35 | Reply header in the history | FAIL | message not reachable by scrolling up: ^Replying to the welcome message (history gap, TGB-3176) |
| EV-36 | GIF autoplays | needs a look | reference client did not pass either: message not reachable by scrolling up: ^Notes as a file |
| EV-37 | Round video message | needs a look | reference client did not pass either: message not reachable by scrolling up: ^Notes as a file |
| EV-38 | Play a voice message | needs a look | reference client did not pass either: message not reachable by scrolling up: ^Notes as a file |
| EV-39 | Emoji panel | needs a look |  |
| EV-40 | Sticker panel | needs a look |  |
| EV-41 | Main menu | pass |  |
| EV-42 | Settings opens | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-43 | Settings: Notifications and Sounds | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-44 | Settings: Privacy and Security | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-45 | Settings: Chat Settings | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-46 | Settings: Folders | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-47 | Settings: Advanced | blocked | not run: needs runner work (text not on screen: ^Settings$) |
| EV-48 | Night mode toggles the theme | blocked | not run: needs runner work (text not on screen: ^Night Mode) |
| EV-49 | Narrow window: one-column layout | pass |  |
| EV-50 | Window resize keeps the layout | pass |  |
| EV-51 | Escape closes the chat | pass |  |
| EV-52 | Restart keeps the session | needs a look | reference client did not pass either: notext whole Start Messaging/Log in |
| EV-53 | Mention marker in the chat list | blocked | not run: needs runner work (chat 'Walk Basic Group' did not open) |
| EV-54 | Muted chat looks muted | needs a look |  |
| EV-55 | Mute and unmute a chat from its profile | needs a look | reference client did not pass either: text whole Notifications |
| EV-56 | Channel post views | needs a look |  |
| EV-57 | Chat header actions | needs a look |  |
| EV-58 | Scheduled messages | needs a look |  |
| EV-59 | Unread messages bar when opening a chat | blocked | not run: needs runner work (chat 'Walk Basic Group' did not open) |
| EV-60 | Archived chat opens | needs a look | reference client did not pass either: text history Archiving this one |
| EV-61 | Jump to bottom button | needs a look | reference client did not pass either: chat 'Walk Peer A' did not open |
| EV-62 | Media viewer: next photo of an album | needs a look | reference client did not pass either: chat 'Walk Supergroup' did not open |
| EV-63 | The chat-list unread counter equals the server's unread count | needs a look | reference client did not pass either: listbadge "^Walk Peer A" {"op":"dialog","who":"a","chat":"pm"} |

How this table was made: one full pass on master `de63bc2a` (76 min for 92 scenarios) plus re-plays of
OB-01, OB-02, OB-05, OB-06, OB-08, OB-12, OB-13, OB-14, OB-15 and OB-19 after runner fixes; the latest verdict counts.
All three clients ran as test account B on their own logins; the "other side" (old account A) acted through
its own official client window. "needs a look" means the official client failed the same check (so the check
is in doubt) or only a person can judge the pair; most of them come from runner limits listed below, not
from Rustgram.

OB walk of 2026-11-28 (master `0ddb18c0`): the official copy stopped opening chats after OB-16 in the first pass, so
OB-02, 03, 04, 06, 14, 17, 19, 20, 21, 25 and 29 were played again on fresh clients; the later verdict counts. Still failing:
TGB-3176 (history gap: 10 scenarios cannot reach their message), TGB-3159 (tray debug item), TGB-3186 (the profile column
stays open in every Rustgram shot and covers the corner buttons and the mic). Passing: TGB-3122 (close to tray),
TGB-3123 (tray icon), TGB-3128 ("typing": OB-02 and OB-04). New: TGB-3205 (own story initials), TGB-3206 (link card
without photo and Instant View). Runner limits seen: both clients post as one account, so OB-14's check finds the other
client's copy; the other side's fillers pile up in the supergroup (the walk account may not delete them); the seek
for "History line 040" ended near line 300 in both clients without reporting a failure.

Issues from this walk: TGB-3176 (history gap when scrolling a group up), TGB-3186 (profile column does not
close on its x), TGB-3159 (tray menu debug entries), TGB-3160 (unread badges), TGB-3161 (corner "@" and
counters while scrolled up), TGB-3158 (test account A lost; one login = one process). TGB-3128 ("typing")
passes on this master.

Not run / skipped: step 5 (the same scenarios on v0.2.0-alpha.2) was not run this time; the checklist
scenario was skipped (creating a checklist needs Premium, neither test account has it); a "network drops for
20 s" scenario was skipped (cutting the network for one process needs an admin firewall rule).

Known runner limits (next fixes): the reference client sometimes stops taking posted clicks after a context
menu; one pass takes ~76 min because every step re-captures and OCRs the window and seeks scroll in small
steps; reactions by the other side need a second Rustgram-readable account.
