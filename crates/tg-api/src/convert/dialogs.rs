//! Dialog, folder, filter, draft, and typing-action TL -> domain conversions.
//!
//! Port origin: `data/data_session.cpp`, `data/data_chat_filters.cpp`, `data/data_drafts.cpp`,
//! `history/history.cpp` (`applyDialog`).

use std::collections::BTreeSet;

use tg_data::{
    ARCHIVE_FOLDER_ID, ChannelId, ChatFilter, ChatFilterFlags, DialogKey, Draft, DraftKey,
    EffectId, FolderId, MsgId, PeerId, ReplyInfo, Session, TextWithEntities, TimeId, TypingAction,
    UserId, WebPageDraft,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::messages::{
    apply_tl_message, entities_from_tl, entities_to_tl, input_reply_to_from_reply_info,
    text_with_entities_from_tl,
};
use crate::convert::peers::{
    apply_tl_users_and_chats, peer_id_from_input_peer, peer_id_from_tl,
    peer_notify_settings_from_tl,
};

/// Extracts `(peer_id, msg_id, date)` from a TL `Message` without allocating domain structs.
#[must_use]
pub const fn tl_message_meta(msg: &tl_enums::Message) -> Option<(PeerId, MsgId, TimeId)> {
    match msg {
        tl_enums::Message::Message(m) => {
            Some((peer_id_from_tl(&m.peer_id), MsgId(m.id as i64), m.date))
        }
        tl_enums::Message::Service(s) => {
            Some((peer_id_from_tl(&s.peer_id), MsgId(s.id as i64), s.date))
        }
        tl_enums::Message::Empty(e) => match &e.peer_id {
            Some(p) => Some((peer_id_from_tl(p), MsgId(e.id as i64), 0)),
            None => None,
        },
    }
}

/// Converts a TL `DraftMessage` into `Option<tg_data::Draft>` (`None` for `draftMessageEmpty`).
#[must_use]
pub fn draft_from_tl(draft: &tl_enums::DraftMessage) -> Option<Draft> {
    match draft {
        tl_enums::DraftMessage::Empty(_) => None,
        tl_enums::DraftMessage::DraftMessage(d) => {
            let entities = d
                .entities
                .as_deref()
                .map(entities_from_tl)
                .unwrap_or_default();
            let reply = d.reply_to.as_ref().map(|r| match r {
                tl_enums::InputReplyTo::Message(m) => ReplyInfo {
                    reply_to_msg_id: (m.reply_to_msg_id != 0)
                        .then_some(MsgId(i64::from(m.reply_to_msg_id))),
                    top_msg_id: m.top_msg_id.map(|id| MsgId(i64::from(id))),
                    reply_to_peer_id: m
                        .reply_to_peer_id
                        .as_ref()
                        .map(|p| peer_id_from_input_peer(p, None))
                        .filter(|p| !p.is_zero()),
                    quote_text: m.quote_text.as_ref().map(|q| {
                        TextWithEntities::new(
                            q.clone(),
                            m.quote_entities
                                .as_deref()
                                .map(entities_from_tl)
                                .unwrap_or_default(),
                        )
                    }),
                    quote_offset: m.quote_offset,
                    quote: m.quote_text.is_some(),
                    monoforum_peer_id: m
                        .monoforum_peer_id
                        .as_ref()
                        .map(|p| peer_id_from_input_peer(p, None))
                        .filter(|p| !p.is_zero()),
                    todo_item_id: m.todo_item_id,
                    poll_option: m.poll_option.clone(),
                    ..ReplyInfo::default()
                },
                tl_enums::InputReplyTo::Story(s) => ReplyInfo {
                    reply_to_story: Some(tg_data::FullStoryId::new(
                        peer_id_from_input_peer(&s.peer, None),
                        s.story_id,
                    )),
                    ..ReplyInfo::default()
                },
                tl_enums::InputReplyTo::MonoForum(mf) => ReplyInfo {
                    monoforum_peer_id: Some(peer_id_from_input_peer(&mf.monoforum_peer_id, None)),
                    ..ReplyInfo::default()
                },
                tl_enums::InputReplyTo::EphemeralMessage(e) => ReplyInfo {
                    reply_to_msg_id: Some(MsgId(i64::from(e.id))),
                    ..ReplyInfo::default()
                },
            });
            let mut webpage = WebPageDraft {
                removed: d.no_webpage,
                invert: d.invert_media,
                ..WebPageDraft::default()
            };
            if let Some(tl_enums::InputMedia::WebPage(wp)) = &d.media {
                webpage.url = wp.url.clone();
                webpage.force_large_media = wp.force_large_media;
                webpage.force_small_media = wp.force_small_media;
                webpage.manual = !wp.optional;
            }
            Some(Draft {
                date: d.date,
                text: TextWithEntities::new(d.message.clone(), entities),
                reply,
                webpage,
                cursor_position: 0,
                cursor_anchor: 0,
                effect_id: d.effect.map(EffectId::from_tl),
            })
        }
    }
}

/// Converts a domain `tg_data::Draft` into a `messages.saveDraft` TL function call.
#[must_use]
pub fn draft_to_save_request(
    draft: &Draft,
    peer: tl_enums::InputPeer,
    session: &Session,
) -> tl_fns::messages::SaveDraft {
    let entities = entities_to_tl(&draft.text.entities, session);
    let media = if !draft.webpage.url.is_empty() && !draft.webpage.removed {
        Some(tl_enums::InputMedia::WebPage(tl_types::InputMediaWebPage {
            force_large_media: draft.webpage.force_large_media,
            force_small_media: draft.webpage.force_small_media,
            optional: !draft.webpage.manual,
            url: draft.webpage.url.clone(),
        }))
    } else {
        None
    };
    tl_fns::messages::SaveDraft {
        no_webpage: draft.webpage.removed,
        invert_media: draft.webpage.invert,
        reply_to: draft
            .reply
            .as_ref()
            .and_then(|r| input_reply_to_from_reply_info(r, session)),
        peer,
        message: draft.text.text.clone(),
        entities: (!entities.is_empty()).then_some(entities),
        media,
        effect: draft.effect_id.map(EffectId::to_tl),
        suggested_post: None,
        rich_message: None,
    }
}

/// Converts a TL `DialogFilter` into `Option<tg_data::ChatFilter>` (`None` for `dialogFilterDefault`).
#[must_use]
pub fn dialog_filter_from_tl(
    filter: &tl_enums::DialogFilter,
    self_id: Option<UserId>,
) -> Option<ChatFilter> {
    match filter {
        tl_enums::DialogFilter::Default => None,
        tl_enums::DialogFilter::DialogFilter(f) => {
            let mut flags = ChatFilterFlags::empty();
            flags.set(ChatFilterFlags::CONTACTS, f.contacts);
            flags.set(ChatFilterFlags::NON_CONTACTS, f.non_contacts);
            flags.set(ChatFilterFlags::GROUPS, f.groups);
            flags.set(ChatFilterFlags::CHANNELS, f.broadcasts);
            flags.set(ChatFilterFlags::BOTS, f.bots);
            flags.set(ChatFilterFlags::NO_MUTED, f.exclude_muted);
            flags.set(ChatFilterFlags::NO_READ, f.exclude_read);
            flags.set(ChatFilterFlags::NO_ARCHIVED, f.exclude_archived);
            flags.set(ChatFilterFlags::STATIC_TITLE, f.title_noanimate);

            let mut title = text_with_entities_from_tl(&f.title);
            if f.title_noanimate {
                title = title.force_custom_emoji_static();
            }
            let pinned: Vec<PeerId> = f
                .pinned_peers
                .iter()
                .map(|p| peer_id_from_input_peer(p, self_id))
                .filter(|p| !p.is_zero())
                .collect();
            let always: BTreeSet<PeerId> = f
                .include_peers
                .iter()
                .map(|p| peer_id_from_input_peer(p, self_id))
                .filter(|p| !p.is_zero())
                .collect();
            let never: BTreeSet<PeerId> = f
                .exclude_peers
                .iter()
                .map(|p| peer_id_from_input_peer(p, self_id))
                .filter(|p| !p.is_zero())
                .collect();

            Some(ChatFilter::new(
                f.id,
                title,
                f.emoticon.clone().unwrap_or_default(),
                f.color.and_then(|c| u8::try_from(c).ok()),
                flags,
                always,
                pinned,
                never,
            ))
        }
        tl_enums::DialogFilter::Chatlist(f) => {
            let mut flags = ChatFilterFlags::CHATLIST;
            flags.set(ChatFilterFlags::HAS_MY_LINKS, f.has_my_invites);
            flags.set(ChatFilterFlags::STATIC_TITLE, f.title_noanimate);

            let mut title = text_with_entities_from_tl(&f.title);
            if f.title_noanimate {
                title = title.force_custom_emoji_static();
            }
            let pinned: Vec<PeerId> = f
                .pinned_peers
                .iter()
                .map(|p| peer_id_from_input_peer(p, self_id))
                .filter(|p| !p.is_zero())
                .collect();
            let always: BTreeSet<PeerId> = f
                .include_peers
                .iter()
                .map(|p| peer_id_from_input_peer(p, self_id))
                .filter(|p| !p.is_zero())
                .collect();

            Some(ChatFilter::new(
                f.id,
                title,
                f.emoticon.clone().unwrap_or_default(),
                f.color.and_then(|c| u8::try_from(c).ok()),
                flags,
                always,
                pinned,
                BTreeSet::new(),
            ))
        }
    }
}

/// Applies a TL `Dialog` (`dialog`, `dialogFolder`, or `dialogCommunity`) into `session`.
pub fn apply_tl_dialog(
    session: &mut Session,
    tl_dialog: &tl_enums::Dialog,
    now: TimeId,
) -> Option<DialogKey> {
    apply_tl_dialog_in_folder(session, tl_dialog, None, now)
}

/// Applies a TL `Dialog` into `session` with an explicit `request_folder` fallback when the
/// server omits `dialog.folder_id` (`History::applyDialog` in `history/history.cpp:3518-3575`,
/// `Session::applyDialog` in `data/data_session.cpp:2861-2913`, `Folder::applyDialog` in
/// `data/data_folder.cpp:354-367`).
pub fn apply_tl_dialog_in_folder(
    session: &mut Session,
    tl_dialog: &tl_enums::Dialog,
    request_folder: Option<FolderId>,
    now: TimeId,
) -> Option<DialogKey> {
    match tl_dialog {
        tl_enums::Dialog::Dialog(d) => {
            let peer_id = peer_id_from_tl(&d.peer);
            if peer_id.is_zero() {
                return None;
            }
            let notify = peer_notify_settings_from_tl(&d.notify_settings);
            if let Some(ttl) = d.ttl_period
                && let Some(peer) = session.peer_mut(peer_id)
            {
                peer.common_mut().messages_ttl = ttl;
            }
            // Match `History::applyDialog` (`history/history.cpp:3521-3526`):
            // when `d.folder_id` is omitted, inherit `request_folder`.
            let target_folder = d.folder_id.or(request_folder).filter(|&fid| fid != 0);
            let is_archived = target_folder == Some(ARCHIVE_FOLDER_ID);
            // If folder changed while pinned, unpin from the old folder first
            // (`history/history.cpp:2532-2534`, `History::setFolderPointer`).
            let old_folder = session.history(peer_id).and_then(|h| h.folder);
            if old_folder != Some(target_folder)
                && session.history(peer_id).is_some_and(|h| h.is_pinned(0))
            {
                session.set_pinned(DialogKey::History(peer_id), 0, false);
            }
            {
                let history = session.ensure_history(peer_id);
                history.in_chat_list = true;
                if d.top_message > 0 {
                    let top = MsgId(i64::from(d.top_message));
                    if history.top_message_id.is_none_or(|cur| top >= cur) {
                        history.top_message_id = Some(top);
                    }
                }
                // Match `History::applyDialogFields` (`history/history.cpp:3676-3680`):
                // only update `unread_count` and `inbox_read_before` if `read_inbox_max_id + 1`
                // is at least our current `_inboxReadBefore` watermark.
                let next_inbox_before = i64::from(d.read_inbox_max_id) + 1;
                let current_inbox_before = history.inbox_read_before.map_or(1, |m| m.0);
                if next_inbox_before >= current_inbox_before {
                    history.unread_count = Some(d.unread_count.max(0));
                    if d.read_inbox_max_id > 0 {
                        history.inbox_read_before = Some(MsgId(next_inbox_before));
                    }
                }
                if d.read_outbox_max_id > 0 {
                    history.outbox_read_before = history
                        .outbox_read_before
                        .max(Some(MsgId(i64::from(d.read_outbox_max_id) + 1)));
                }
                history.unread_mark = d.unread_mark;
                history.unread_mentions_count = d.unread_mentions_count.max(0);
                history.unread_reactions_count = d.unread_reactions_count.max(0);
                history.unread_poll_votes_count = d.unread_poll_votes_count.max(0);
                history.folder = Some(target_folder);
            }
            // The mute state comes from the dialog's own settings or the per-type default, and
            // reaches change subscribers (the notification system) through `NOTIFICATIONS`.
            session.set_peer_notify(peer_id, notify, now);
            if let Some(draft_tl) = &d.draft {
                let cloud_key = DraftKey::cloud(MsgId::ZERO, PeerId::ZERO);
                match draft_from_tl(draft_tl) {
                    Some(draft) => session.set_draft(peer_id, cloud_key, draft),
                    None => {
                        session.clear_draft(peer_id, cloud_key);
                    }
                }
            }
            let key = DialogKey::History(peer_id);
            // Match `Session::setPinnedFromEntryList` (`data/data_session.cpp:2801-2810, 2871`):
            // append pinned dialogs in server order instead of rotating each one to top.
            session.add_pinned_from_entry_list(key, is_archived, d.pinned);

            // Match `Session::applyDialog` (`data/data_session.cpp:2873-2881`):
            // hide migrated basic group chats once the user is a member of the target megagroup.
            if let Some(ch_id) = peer_id.to_channel()
                && let Some(from_chat) = session.channel(ch_id).and_then(|c| c.migrated_from())
            {
                let from_pid = PeerId::from_chat(from_chat);
                if let Some(h) = session.history_mut(from_pid) {
                    h.in_chat_list = false;
                }
                session.sync_dialog_lists(from_pid);
            } else if let Some(chat_id) = peer_id.to_chat()
                && let Some(to_ch) = session.chat(chat_id).and_then(|c| c.migrated_to)
                && session.channel(to_ch).is_some_and(|c| c.am_in())
                && let Some(h) = session.history_mut(peer_id)
            {
                h.in_chat_list = false;
            }

            session.sync_dialog_lists(peer_id);
            Some(key)
        }
        tl_enums::Dialog::Folder(f) => {
            let tl_enums::Folder::Folder(folder) = &f.folder;
            if folder.id == ARCHIVE_FOLDER_ID {
                {
                    let archive = session.archive_mut();
                    if !folder.title.is_empty() {
                        archive.name = folder.title.clone();
                    }
                    archive.fixed_on_top = f.pinned;
                    archive.chats_list.update_cloud_unread(
                        f.unread_muted_messages_count,
                        f.unread_unmuted_messages_count,
                        f.unread_muted_peers_count,
                        f.unread_unmuted_peers_count,
                    );
                }
                // Match `Folder::applyDialog` (`data/data_folder.cpp:354-363`):
                // register the folder's top peer inside the archive folder so the archive row
                // appears in the main dialog list.
                let top_peer = peer_id_from_tl(&f.peer);
                if !top_peer.is_zero() {
                    if f.top_message > 0 {
                        let top = MsgId(i64::from(f.top_message));
                        let history = session.ensure_history(top_peer);
                        if history.top_message_id.is_none_or(|cur| top >= cur) {
                            history.top_message_id = Some(top);
                        }
                    }
                    session.set_history_folder(top_peer, Some(ARCHIVE_FOLDER_ID));
                } else {
                    session.archive_mut().chats_list.clear();
                    session.refresh_archive_entry();
                }
                // Match `Session::applyDialog(MTPDdialogFolder)` (`data/data_session.cpp:2892`).
                session.add_pinned_from_entry_list(DialogKey::Folder(folder.id), false, f.pinned);
                session.refresh_archive_entry();
            }
            Some(DialogKey::Folder(folder.id))
        }
        tl_enums::Dialog::Community(c) => {
            let peer_id = PeerId::from_channel(ChannelId::from_tl(c.community_id));
            let notify = peer_notify_settings_from_tl(&c.notify_settings);
            session.set_peer_notify(peer_id, notify, now);
            let key = DialogKey::History(peer_id);
            session.add_pinned_from_entry_list(key, false, c.pinned);
            Some(key)
        }
    }
}

/// Applies an entire `messages.Dialogs` response (`users`, `chats`, `messages`, `dialogs`)
/// into `session`.
pub fn apply_tl_dialogs_response(
    session: &mut Session,
    resp: &tl_enums::messages::Dialogs,
    now: TimeId,
) {
    apply_tl_dialogs_response_for_folder(session, resp, None, now);
}

/// Applies an entire `messages.Dialogs` response into `session` with an explicit
/// `request_folder` fallback (`Session::applyDialogs` in `data/data_session.cpp:2845-2859`).
pub fn apply_tl_dialogs_response_for_folder(
    session: &mut Session,
    resp: &tl_enums::messages::Dialogs,
    request_folder: Option<FolderId>,
    now: TimeId,
) {
    let (users, chats, messages, dialogs, count) = match resp {
        tl_enums::messages::Dialogs::Dialogs(d) => (
            &d.users,
            &d.chats,
            &d.messages,
            &d.dialogs,
            Some(d.dialogs.len()),
        ),
        tl_enums::messages::Dialogs::Slice(d) => (
            &d.users,
            &d.chats,
            &d.messages,
            &d.dialogs,
            Some(d.count.max(0) as usize),
        ),
        tl_enums::messages::Dialogs::NotModified(_) => return,
    };
    apply_tl_users_and_chats(session, users, chats);
    for msg in messages {
        apply_tl_message(session, msg);
    }
    for dialog in dialogs {
        apply_tl_dialog_in_folder(session, dialog, request_folder, now);
    }
    if let Some(c) = count {
        session.set_folder_cloud_list_size(request_folder.unwrap_or(0), c);
    }
}

/// Applies a `messages.PeerDialogs` response (`users`, `chats`, `messages`, `dialogs`) into `session`.
pub fn apply_tl_peer_dialogs(
    session: &mut Session,
    resp: &tl_types::messages::PeerDialogs,
    now: TimeId,
) {
    apply_tl_peer_dialogs_for_folder(session, resp, None, false, now);
}

/// Applies a `messages.PeerDialogs` response into `session` with optional folder fallback and
/// optional stale-pin clearing (`ApiWrap::sendPinnedDialogsRequest` in `apiwrap.cpp:1176-1183`).
pub fn apply_tl_peer_dialogs_for_folder(
    session: &mut Session,
    resp: &tl_types::messages::PeerDialogs,
    request_folder: Option<FolderId>,
    clear_pinned: bool,
    now: TimeId,
) {
    apply_tl_users_and_chats(session, &resp.users, &resp.chats);
    if clear_pinned {
        session.clear_pinned_chats(request_folder == Some(ARCHIVE_FOLDER_ID));
    }
    for msg in &resp.messages {
        apply_tl_message(session, msg);
    }
    for dialog in &resp.dialogs {
        apply_tl_dialog_in_folder(session, dialog, request_folder, now);
    }
}

/// Converts a TL `SendMessageAction` into `tg_data::TypingAction`.
#[must_use]
pub fn typing_action_from_tl(action: &tl_enums::SendMessageAction) -> TypingAction {
    match action {
        tl_enums::SendMessageAction::SendMessageTypingAction
        | tl_enums::SendMessageAction::SendMessageTextDraftAction(_)
        | tl_enums::SendMessageAction::InputSendMessageRichMessageDraftAction(_)
        | tl_enums::SendMessageAction::SendMessageRichMessageDraftAction(_) => TypingAction::Typing,
        tl_enums::SendMessageAction::SendMessageCancelAction
        | tl_enums::SendMessageAction::SendMessageStopDraftAction(_) => TypingAction::Cancel,
        tl_enums::SendMessageAction::SendMessageRecordVideoAction => TypingAction::RecordVideo,
        tl_enums::SendMessageAction::SendMessageUploadVideoAction(a) => {
            TypingAction::UploadVideo(a.progress)
        }
        tl_enums::SendMessageAction::SendMessageRecordAudioAction => TypingAction::RecordVoice,
        tl_enums::SendMessageAction::SendMessageUploadAudioAction(a) => {
            TypingAction::UploadVoice(a.progress)
        }
        tl_enums::SendMessageAction::SendMessageUploadPhotoAction(a) => {
            TypingAction::UploadPhoto(a.progress)
        }
        tl_enums::SendMessageAction::SendMessageUploadDocumentAction(a) => {
            TypingAction::UploadDocument(a.progress)
        }
        tl_enums::SendMessageAction::SendMessageGeoLocationAction => TypingAction::ChooseLocation,
        tl_enums::SendMessageAction::SendMessageChooseContactAction => TypingAction::ChooseContact,
        tl_enums::SendMessageAction::SendMessageGamePlayAction => TypingAction::PlayGame,
        tl_enums::SendMessageAction::SendMessageRecordRoundAction => TypingAction::RecordRound,
        tl_enums::SendMessageAction::SendMessageUploadRoundAction(a) => {
            TypingAction::UploadRound(a.progress)
        }
        tl_enums::SendMessageAction::SpeakingInGroupCallAction => TypingAction::Speaking,
        tl_enums::SendMessageAction::SendMessageChooseStickerAction => TypingAction::ChooseSticker,
        tl_enums::SendMessageAction::SendMessageHistoryImportAction(a) => {
            TypingAction::HistoryImport(a.progress)
        }
        tl_enums::SendMessageAction::SendMessageEmojiInteraction(_)
        | tl_enums::SendMessageAction::SendMessageEmojiInteractionSeen(_) => TypingAction::Other,
    }
}
