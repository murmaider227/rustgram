//! Albums and scheduled media sent without waiting on the UI thread (TGB-3130).
//!
//! tdesktop creates one local item per file of an album, all with the album's `groupId`, and
//! queues every file on the `Uploader` (`ApiWrap::sendFiles`, `apiwrap.cpp:4396-4466`). Each
//! uploaded file goes through `messages.uploadMedia` (`uploadAlbumMedia`, `apiwrap.cpp:5275`)
//! into the `SendingAlbum` (`sendAlbumWithUploaded`, `apiwrap.cpp:5588`), and once every file
//! still in the album has its media, `sendAlbumIfReady` (`apiwrap.cpp:5621-5735`) sends one
//! `messages.sendMultiMedia`, or `messages.sendMedia` when a single file is left, or nothing
//! when none is. A cancelled file leaves the album and its local item is destroyed
//! (`cancelLocalItem` -> `sendAlbumWithCancelled`, `apiwrap.cpp:4525-4530`, `5603-5619`); a
//! failed `messages.sendMultiMedia` fails every item (`sendMessageFail` per item,
//! `apiwrap.cpp:5725-5730`).
//!
//! Here the album takes one place in the ordered media queue of
//! [`crate::optimistic_send::OptimisticSends::begin_media_send`] (TGB-3168): it starts after
//! the media sent before it and the media sent after it wait for its `messages.sendMultiMedia`.
//! Every file gets its pending bubble at once and its own row in the uploads bar (so each can
//! be cancelled, like tdesktop's per-item cancel); the next history drain turns the bubbles into
//! the server messages ([`crate::optimistic_send::SendResolution`]).
//!
//! One difference: when a file's upload fails, tdesktop leaves that item with a failed upload
//! and the album waits until the user deletes it (`uploadAlbumMedia`'s empty `failed`). That
//! would hold the ordered queue for ever, so the album fails as a whole instead, the way a
//! failed `messages.sendMultiMedia` fails it.
//!
//! A scheduled send (album or single file) belongs in the scheduled section, not the history:
//! it stores no local message, runs in the same queue, and the server's
//! `updateNewScheduledMessage` puts it in that section. A failed upload stays in the uploads
//! bar, marked failed.

use super::*;

/// One file of an album sent through [`LiveSession::send_album_queued`].
pub(crate) struct QueuedAlbumItem {
    /// The file and how it is sent.
    pub media: OutgoingMedia,
    /// Its own caption (else the album caption goes on the first file).
    pub caption: Option<TextWithEntities>,
    /// What its pending bubble shows.
    pub shown: HistoryMedia,
}

/// A file of the album on its way to the server.
struct ItemJob {
    media: OutgoingMedia,
    upload_id: u64,
    random_id: i64,
    cancel: CancelToken,
    caption: (String, Vec<tl_enums::MessageEntity>),
}

/// The parts of `messages.sendMultiMedia` / `messages.sendMedia` shared by every file.
struct AlbumRequest {
    input_peer: tl_enums::InputPeer,
    reply_to: Option<tl_enums::InputReplyTo>,
    send_as: Option<tl_enums::InputPeer>,
    silent: bool,
    clear_draft: bool,
    invert_media: bool,
    schedule_date: Option<i32>,
    effect: Option<i64>,
}

/// What became of the album once its place in the queue came.
struct AlbumRun {
    /// Per file: its media went to the server.
    sent: Vec<bool>,
    /// Per file: it was cancelled before the album was sent.
    cancelled: Vec<bool>,
    /// The server's answer (`None` when every file was cancelled), or the error.
    result: Result<Option<tl_enums::Updates>, String>,
}

/// A file's local message (none for a scheduled send).
struct LocalItem {
    local_id: DataMsgId,
    random_id: i64,
    caption: TextWithEntities,
    shown: HistoryMedia,
}

impl LiveSession {
    /// Sends an album the way tdesktop does: a pending bubble for every file at once (the
    /// first one is returned, the others come with the next history drain), the uploads and
    /// one `messages.sendMultiMedia` in the background, in order with the other media sends.
    pub(crate) fn send_album_queued(
        &self,
        peer_id: PeerId,
        mut items: Vec<QueuedAlbumItem>,
        options: SendMediaOptions,
    ) -> Result<HistoryMessage, String> {
        if items.is_empty() {
            return Err("Album must contain at least one item.".to_owned());
        }
        // A single file is not an album (`apiwrap.cpp:5660`).
        if items.len() == 1 {
            let item = items.remove(0);
            let mut single = options;
            if let Some(caption) = item.caption {
                single.caption = caption;
            }
            return if single.schedule_date.is_some() {
                self.send_scheduled_media_queued(peer_id, item.media, single, item.shown)
            } else {
                self.send_uploaded_media_queued(peer_id, item.media, single, item.shown)
            };
        }

        let input_peer = resolve_input_peer_sync(self, peer_id)
            .ok_or_else(|| "Peer not found in session.".to_owned())?;
        let send_as = options
            .send_as
            .and_then(|pid| resolve_input_peer_sync(self, pid));
        let reply_to = options
            .reply_to
            .map(|mid| make_input_reply_to(mid, options.reply_quote.clone()));

        // `SendingAlbum`: the album caption goes on the first file unless files have their own.
        let has_per_item_caption = items.iter().any(|i| i.caption.is_some());
        let captions: Vec<TextWithEntities> = items
            .iter_mut()
            .enumerate()
            .map(|(idx, item)| match item.caption.take() {
                Some(caption) => caption,
                None if !has_per_item_caption && idx == 0 => options.caption.clone(),
                None => TextWithEntities::default(),
            })
            .collect();
        let tl_captions: Vec<(String, Vec<tl_enums::MessageEntity>)> =
            read_session_sync(self, |s| {
                captions
                    .iter()
                    .map(|c| (c.text.clone(), ui_text_to_tl_entities(c, s)))
                    .collect()
            });

        // Every file gets its own row in the uploads bar and its own cancel.
        let state = upload_state_for(self);
        let mut jobs = Vec::with_capacity(items.len());
        let mut shown = Vec::with_capacity(items.len());
        {
            let mut guard = state
                .lock()
                .map_err(|_| "upload state lock poisoned".to_owned())?;
            for (item, caption) in items.into_iter().zip(tl_captions) {
                let upload_id = guard.next_upload_id;
                guard.next_upload_id = guard.next_upload_id.wrapping_add(1);
                let random_id = guard.next_random_id();
                let cancel = CancelToken::new();
                let total_bytes = item.media.total_upload_bytes();
                if total_bytes > 0 {
                    guard.active_uploads.insert(
                        upload_id,
                        TrackedMediaUpload {
                            status: MediaUploadStatus {
                                upload_id,
                                peer_id,
                                file_name: item.media.display_name(),
                                uploaded_bytes: 0,
                                total_bytes,
                                failed: false,
                                cancelled: false,
                            },
                            cancel: cancel.clone(),
                        },
                    );
                }
                shown.push(item.shown);
                jobs.push(ItemJob {
                    media: item.media,
                    upload_id,
                    random_id,
                    cancel,
                    caption,
                });
            }
        }

        let data_peer = to_data_peer(peer_id);
        let locals: Vec<LocalItem> = jobs
            .iter()
            .zip(captions)
            .zip(shown)
            .map(|((job, caption), shown)| LocalItem {
                local_id: self.sends.next_local_id(),
                random_id: job.random_id,
                caption,
                shown,
            })
            .collect();
        let scheduled = options.schedule_date.is_some();
        let returned = if scheduled {
            let first = &locals[0];
            let mut placeholder = fallback_sent_history_message(
                self,
                peer_id,
                first.local_id,
                &first.caption,
                Some(first.shown.clone()),
                options.silent,
                options.schedule_date,
            );
            placeholder.is_sending = true;
            placeholder
        } else {
            match self.store_album_locals(data_peer, &locals, &options) {
                Some(pending) => pending,
                None => {
                    for job in &jobs {
                        finish_tracked_upload(&state, job.upload_id, Some("not stored"));
                    }
                    return Err("sent album was not stored in session".to_owned());
                }
            }
        };
        self.bridge_state().notify_ui();

        let request = AlbumRequest {
            input_peer,
            reply_to,
            send_as,
            silent: options.silent,
            clear_draft: options.clear_draft,
            invert_media: options.invert_media,
            schedule_date: options.schedule_date,
            effect: options.effect_id.map(|id| id as i64),
        };
        let live = self.clone();
        let handle = self.runtime().tokio_handle().clone();
        let rpc = self.runtime().rpc().clone();
        let cfg = effective_upload_config(self);
        let bridge = Arc::clone(self.bridge_state());
        let effect_id = options.effect_id;
        let silent = options.silent;
        // Taken here, on the caller's thread, so the queue order is the order of the sends.
        let mut in_flight = self.sends.begin_media_send();
        drop(self.runtime().tokio_handle().spawn_blocking(move || {
            let previous = in_flight.previous();
            let upload_ids: Vec<u64> = jobs.iter().map(|job| job.upload_id).collect();
            let progress_state = Arc::clone(&state);
            let run = block_on_runtime(&handle, async move {
                previous.await;
                upload_and_send_album(&rpc, request, jobs, cfg, &progress_state, &bridge).await
            });
            for (i, upload_id) in upload_ids.iter().enumerate() {
                let error = match &run.result {
                    _ if run.cancelled[i] => Some("upload cancelled"),
                    Err(err) => Some(err.as_str()),
                    Ok(_) => None,
                };
                finish_tracked_upload(&state, *upload_id, error);
            }
            if scheduled {
                if let Ok(Some(updates)) = &run.result {
                    live.apply_updates_to_session(updates);
                }
            } else {
                live.resolve_album(data_peer, peer_id, &locals, &run, silent, effect_id);
            }
            live.bridge_state().mark_views_changed();
            live.bridge_state().notify_ui();
        }));
        Ok(returned)
    }

    /// Sends one scheduled file in the media queue without waiting; it stores no local message
    /// (its place is the scheduled section, which the server's update fills).
    pub(crate) fn send_scheduled_media_queued(
        &self,
        peer_id: PeerId,
        media: OutgoingMedia,
        options: SendMediaOptions,
        shown: HistoryMedia,
    ) -> Result<HistoryMessage, String> {
        let send = self.prepare_media_send(peer_id, media, &options, CancelToken::new())?;
        let mut placeholder = fallback_sent_history_message(
            self,
            peer_id,
            self.sends.next_local_id(),
            &options.caption,
            Some(shown),
            options.silent,
            options.schedule_date,
        );
        placeholder.is_sending = true;
        let live = self.clone();
        let handle = self.runtime().tokio_handle().clone();
        let mut in_flight = self.sends.begin_media_send();
        drop(self.runtime().tokio_handle().spawn_blocking(move || {
            let previous = in_flight.previous();
            let outcome = block_on_runtime(&handle, async move {
                previous.await;
                send.request.await
            });
            finish_tracked_upload(
                &send.state,
                send.upload_id,
                outcome.as_ref().err().map(String::as_str),
            );
            if let Ok(updates) = &outcome {
                live.apply_updates_to_session(updates);
            }
            live.bridge_state().mark_views_changed();
            live.bridge_state().notify_ui();
        }));
        self.bridge_state().notify_ui();
        Ok(placeholder)
    }

    /// Stores the local message of every file (one session write) and returns the first one as
    /// its pending bubble shows it.
    fn store_album_locals(
        &self,
        peer: DataPeerId,
        locals: &[LocalItem],
        options: &SendMediaOptions,
    ) -> Option<HistoryMessage> {
        let group_id = (locals.first()?.random_id as u64) | 1;
        let reply_to = options.reply_to;
        let silent = options.silent;
        let effect_id = options.effect_id;
        let rows: Vec<(DataMsgId, i64, tg_data::TextWithEntities)> = locals
            .iter()
            .map(|l| (l.local_id, l.random_id, ui_text_to_data_text(&l.caption)))
            .collect();
        for local in locals {
            self.sends.set_local_media(
                DataFullMsgId::new(peer, local.local_id),
                local.shown.clone(),
            );
        }
        let first = DataFullMsgId::new(peer, locals[0].local_id);
        let last = rows.len() - 1;
        let stored = self.write_session(move |s| {
            let self_peer = s.self_peer_id();
            let now = now_unix_i32();
            for (i, (local_id, random_id, caption)) in rows.into_iter().enumerate() {
                let full = DataFullMsgId::new(peer, local_id);
                let mut local =
                    tg_data::Message::new_text(peer, local_id, Some(self_peer), now, caption);
                local
                    .flags
                    .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
                local.flags.set(MessageFlags::SILENT, silent);
                local.grouped_id = Some(group_id);
                // The effect shows on the album's last message, as the server puts it.
                local.effect_id = (i == last)
                    .then_some(effect_id)
                    .flatten()
                    .map(tg_data::EffectId);
                local.reply_to = reply_to.map(|id| tg_data::ReplyInfo {
                    reply_to_msg_id: Some(to_data_msg(id)),
                    ..tg_data::ReplyInfo::default()
                });
                s.register_random_id(random_id, full);
                s.insert_message(local);
            }
            s.message(first).map(|m| message_to_history_message(s, m))
        });
        match stored {
            Some(mut pending) => {
                pending.media = Some(locals[0].shown.clone());
                Some(pending)
            }
            None => {
                for local in locals {
                    self.sends
                        .take_local_media(DataFullMsgId::new(peer, local.local_id));
                }
                None
            }
        }
    }

    /// Turns the album's local messages into what happened to them: cancelled files go without
    /// an error, sent ones become the server messages, the others are marked failed.
    fn resolve_album(
        &self,
        peer: DataPeerId,
        peer_id: PeerId,
        locals: &[LocalItem],
        run: &AlbumRun,
        silent: bool,
        effect_id: Option<u64>,
    ) {
        let cancelled: Vec<&LocalItem> = locals
            .iter()
            .zip(&run.cancelled)
            .filter_map(|(l, &c)| c.then_some(l))
            .collect();
        let others: Vec<&LocalItem> = locals
            .iter()
            .zip(&run.cancelled)
            .filter_map(|(l, &c)| (!c).then_some(l))
            .collect();

        if !cancelled.is_empty() {
            // tdesktop destroys the item of a cancelled upload.
            let gone: Vec<(DataMsgId, i64)> = cancelled
                .iter()
                .map(|l| (l.local_id, l.random_id))
                .collect();
            self.write_session(move |s| {
                for (local_id, random_id) in gone {
                    s.unregister_random_id(random_id);
                    s.delete_messages(peer, &[local_id]);
                }
            });
            for local in &cancelled {
                self.sends
                    .take_local_media(DataFullMsgId::new(peer, local.local_id));
            }
            self.sends.push_resolution(SendResolution {
                peer,
                local_ids: cancelled.iter().map(|l| l.local_id).collect(),
                outcome: Err("upload cancelled".to_owned()),
                sent_ids: vec![None; cancelled.len()],
                effect_id: None,
                fallback: Vec::new(),
                cancelled: true,
            });
        }
        if others.is_empty() {
            return;
        }

        match &run.result {
            Ok(Some(updates)) => {
                let sent: Vec<&LocalItem> = locals
                    .iter()
                    .zip(&run.sent)
                    .filter_map(|(l, &s)| s.then_some(l))
                    .collect();
                self.apply_updates_to_session(updates);
                let random_ids: Vec<i64> = sent.iter().map(|l| l.random_id).collect();
                let sent_ids = server_ids_by_random_id(updates, &random_ids);
                // A server message that did not replace its local one through
                // `updateMessageID` still replaces it on screen.
                let gone: Vec<(DataMsgId, i64)> =
                    sent.iter().map(|l| (l.local_id, l.random_id)).collect();
                self.write_session(move |s| {
                    for (local_id, random_id) in gone {
                        s.unregister_random_id(random_id);
                        s.delete_messages(peer, &[local_id]);
                    }
                });
                let fallback = sent
                    .iter()
                    .zip(&sent_ids)
                    .map(|(l, id)| {
                        self.sends
                            .take_local_media(DataFullMsgId::new(peer, l.local_id));
                        fallback_sent_history_message(
                            self,
                            peer_id,
                            id.unwrap_or(DataMsgId(0)),
                            &l.caption,
                            Some(l.shown.clone()),
                            silent,
                            None,
                        )
                    })
                    .collect();
                self.sends.push_resolution(SendResolution {
                    peer,
                    local_ids: sent.iter().map(|l| l.local_id).collect(),
                    outcome: Ok(SendMessageOutcome {
                        messages: Vec::new(),
                        updates: vec![updates.clone()],
                    }),
                    sent_ids,
                    effect_id,
                    fallback,
                    cancelled: false,
                });
            }
            // Every file was cancelled meanwhile (handled above).
            Ok(None) => {}
            Err(err) => {
                let failed: Vec<(DataMsgId, i64)> =
                    others.iter().map(|l| (l.local_id, l.random_id)).collect();
                self.write_session(move |s| {
                    for (local_id, random_id) in failed {
                        s.unregister_random_id(random_id);
                        if let Some(msg) = s.message_mut(DataFullMsgId::new(peer, local_id)) {
                            msg.flags.remove(MessageFlags::BEING_SENT);
                            msg.flags.insert(MessageFlags::SENDING_FAILED);
                        }
                    }
                });
                self.sends.push_resolution(SendResolution {
                    peer,
                    local_ids: others.iter().map(|l| l.local_id).collect(),
                    outcome: Err(err.clone()),
                    sent_ids: vec![None; others.len()],
                    effect_id,
                    fallback: Vec::new(),
                    cancelled: false,
                });
            }
        }
    }
}

/// Uploads the files in order (`upload.saveFilePart` + `messages.uploadMedia`), then sends what
/// is left of the album (`sendAlbumIfReady`).
async fn upload_and_send_album<C: RpcClient + Clone + 'static>(
    rpc: &C,
    request: AlbumRequest,
    jobs: Vec<ItemJob>,
    cfg: LiveUploadConfig,
    state: &Arc<Mutex<UploadLiveState>>,
    bridge: &Arc<LiveBridgeState>,
) -> AlbumRun
where
    C::Error: std::fmt::Display,
{
    let tokens: Vec<CancelToken> = jobs.iter().map(|j| j.cancel.clone()).collect();
    let mut ready: Vec<Option<tl_types::InputSingleMedia>> = Vec::with_capacity(jobs.len());
    let mut failure = None;
    for job in jobs {
        if failure.is_some() || job.cancel.is_cancelled() {
            ready.push(None);
            continue;
        }
        let cancel = job.cancel.clone();
        match upload_album_item(rpc, &request.input_peer, job, cfg, state, bridge).await {
            Ok(single) => ready.push(Some(single)),
            Err(_) if cancel.is_cancelled() => ready.push(None),
            Err(err) => {
                ready.push(None);
                failure = Some(err);
            }
        }
    }
    // A file cancelled after its upload, before the album went, leaves it too.
    for (slot, token) in ready.iter_mut().zip(&tokens) {
        if token.is_cancelled() {
            *slot = None;
        }
    }
    let cancelled: Vec<bool> = ready
        .iter()
        .zip(&tokens)
        .map(|(slot, token)| slot.is_none() && token.is_cancelled())
        .collect();
    if let Some(err) = failure {
        return AlbumRun {
            sent: vec![false; ready.len()],
            cancelled,
            result: Err(err),
        };
    }
    let sent: Vec<bool> = ready.iter().map(Option::is_some).collect();
    let mut medias: Vec<tl_types::InputSingleMedia> = ready.into_iter().flatten().collect();
    // Once sent, the album can no longer be cancelled (`album->sent`).
    let no_cancel = CancelToken::new();
    let result = match medias.len() {
        0 => Ok(None),
        1 => {
            // `sendMediaWithRandomId` for the one file left.
            let single = medias.remove(0);
            let req = tl_fns::messages::SendMedia {
                silent: request.silent,
                background: false,
                clear_draft: request.clear_draft,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: request.invert_media,
                allow_paid_floodskip: false,
                peer: request.input_peer,
                reply_to: request.reply_to,
                media: single.media,
                message: single.message,
                random_id: single.random_id,
                reply_markup: None,
                entities: single.entities,
                schedule_date: request.schedule_date,
                schedule_repeat_period: None,
                send_as: request.send_as,
                quick_reply_shortcut: None,
                effect: request.effect,
                allow_paid_stars: None,
                suggested_post: None,
            };
            invoke_with_upload_retry(rpc, &req, &[], cfg, &no_cancel)
                .await
                .map(Some)
        }
        _ => {
            let req = tl_fns::messages::SendMultiMedia {
                silent: request.silent,
                background: false,
                clear_draft: request.clear_draft,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: request.invert_media,
                allow_paid_floodskip: false,
                peer: request.input_peer,
                reply_to: request.reply_to,
                multi_media: medias
                    .into_iter()
                    .map(tl_enums::InputSingleMedia::InputSingleMedia)
                    .collect(),
                schedule_date: request.schedule_date,
                send_as: request.send_as,
                quick_reply_shortcut: None,
                effect: request.effect,
                allow_paid_stars: None,
            };
            invoke_with_upload_retry(rpc, &req, &[], cfg, &no_cancel)
                .await
                .map(Some)
        }
    };
    AlbumRun {
        sent,
        cancelled,
        result,
    }
}

/// Uploads one file and turns it into reusable album media (`uploadAlbumMedia`).
async fn upload_album_item<C: RpcClient + Clone + 'static>(
    rpc: &C,
    input_peer: &tl_enums::InputPeer,
    job: ItemJob,
    cfg: LiveUploadConfig,
    state: &Arc<Mutex<UploadLiveState>>,
    bridge: &Arc<LiveBridgeState>,
) -> Result<tl_types::InputSingleMedia, String>
where
    C::Error: std::fmt::Display,
{
    let ItemJob {
        media,
        upload_id,
        random_id,
        cancel,
        caption: (message, entities),
    } = job;
    let spoiler = media.is_spoiler();
    let prepared = upload_outgoing_media_async(rpc, input_peer, media, cfg, &cancel, |delta| {
        if let Ok(mut guard) = state.lock()
            && let Some(entry) = guard.active_uploads.get_mut(&upload_id)
        {
            entry.status.uploaded_bytes = entry
                .status
                .uploaded_bytes
                .saturating_add(delta)
                .min(entry.status.total_bytes);
        }
        bridge.notify_ui();
    })
    .await
    .map_err(|e| e.to_string())?;
    let upload_req = tl_fns::messages::UploadMedia {
        business_connection_id: None,
        peer: input_peer.clone(),
        media: prepared.input_media,
    };
    let refs: Vec<&UploadedFileResult> = prepared.uploaded_files.iter().collect();
    let uploaded = invoke_with_upload_retry(rpc, &upload_req, &refs, cfg, &cancel).await?;
    Ok(tl_types::InputSingleMedia {
        media: uploaded_message_media_to_input_media(uploaded, spoiler)?,
        random_id,
        message,
        entities: (!entities.is_empty()).then_some(entities),
    })
}

/// Server id of each file: the `updateMessageID` of its random id; without any
/// `updateMessageID`, the new messages in order.
fn server_ids_by_random_id(
    updates: &tl_enums::Updates,
    random_ids: &[i64],
) -> Vec<Option<DataMsgId>> {
    let mut by_random: HashMap<i64, DataMsgId> = HashMap::new();
    let mut visit = |update: &tl_enums::Update| {
        if let tl_enums::Update::MessageID(m) = update {
            by_random.insert(m.random_id, DataMsgId(i64::from(m.id)));
        }
    };
    match updates {
        tl_enums::Updates::Updates(u) => u.updates.iter().for_each(&mut visit),
        tl_enums::Updates::Combined(c) => c.updates.iter().for_each(&mut visit),
        tl_enums::Updates::UpdateShort(s) => visit(&s.update),
        tl_enums::Updates::UpdateShortSentMessage(s) => {
            if let [only] = random_ids {
                by_random.insert(*only, DataMsgId(i64::from(s.id)));
            }
        }
        _ => {}
    }
    let in_order = crate::session::extract_message_ids_from_updates(updates);
    random_ids
        .iter()
        .enumerate()
        .map(|(i, random_id)| {
            by_random.get(random_id).copied().or_else(|| {
                by_random
                    .is_empty()
                    .then(|| in_order.get(i).copied())
                    .flatten()
            })
        })
        .collect()
}
