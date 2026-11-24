//! WP-66 Contacts, Spam Bar, Contact Tokens, Fee-Waiver Tokens, Birthdays, Contact Notes,
//! Own/Contact Profile Photos, and Promo-Data Hiding RPCs (`13 contacts.*`, `2 photos.*`,
//! `messages.{reportSpam,hidePeerSettingsBar}`, `help.hidePromoData`).
//!
//! Port origin (Telegram Desktop, Layer 233):
//! - `Telegram/SourceFiles/boxes/contacts_box.cpp` & `boxes/peers/edit_contact_box.cpp` (`contacts.deleteContacts`)
//! - `Telegram/SourceFiles/history/view/history_view_contact_status.cpp`
//!   (`contacts.acceptContact`, `messages.reportSpam`, `messages.hidePeerSettingsBar`)
//! - `Telegram/SourceFiles/info/profile/info_profile_qr_link.cpp` (`contacts.exportContactToken`)
//! - `Telegram/SourceFiles/window/window_session_controller_contact.cpp` (`contacts.importContactToken`)
//! - `Telegram/SourceFiles/api/api_premium.cpp`
//!   (`contacts.exportNoPaidMessagesToken`, `contacts.importNoPaidMessagesToken`)
//! - `Telegram/SourceFiles/info/profile/info_profile_actions.cpp` (`contacts.updateContactNote`)
//! - `Telegram/SourceFiles/data/components/promo_suggestions.cpp` (`contacts.getBirthdays`)
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`contacts.blockFromReplies`, `help.hidePromoData`)
//! - `Telegram/SourceFiles/api/api_stories.cpp` (`contacts.setBlocked`)
//! - `Telegram/SourceFiles/data/components/top_peers.cpp` (`contacts.toggleTopPeers`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`contacts.getStatuses`)
//! - `Telegram/SourceFiles/api/api_peer_search.cpp` (`contacts.getSponsoredPeers`)
//! - `Telegram/SourceFiles/api/api_peer_photo.cpp` (`photos.deletePhotos`, `photos.uploadContactProfilePhoto`)

use tg_data::{
    Birthday, BlockStatus, ContactStatus, LastseenStatus, PeerBarSettings, PeerId, PeerUpdateFlags,
    PhotoId, Session, TextWithEntities, UserFlags, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_user, apply_tl_users_and_chats, input_peer_for, input_user_for, peer_id_from_tl,
    text_with_entities_to_tl, user_status_from_tl,
};
use crate::error::ApiError;
use crate::rpc::RpcClient;

/// Exported temporary contact QR / deep-link token (`contacts.exportContactToken`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedContactToken {
    /// Shareable contact URL (e.g. `https://t.me/contact/<token>`).
    pub url: String,
    /// Unix expiration timestamp in seconds.
    pub expires: i32,
}

/// Exported paid-message fee-waiver token (`contacts.exportNoPaidMessagesToken`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedNoPaidMessagesToken {
    /// Raw opaque token string.
    pub token: String,
    /// Shareable deep-link URL carrying the token.
    pub url: String,
    /// Optional Unix expiration timestamp (`None` when permanent).
    pub expires: Option<i32>,
}

/// Outcome of `contacts.importNoPaidMessagesToken` (`Api::Premium::importNoPaidMessagesToken`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPaidMessagesTokenImport {
    /// Resolved user peer ID.
    pub peer_id: PeerId,
    /// `true` if the user required Stars per message before import and no longer does after.
    pub price_removed: bool,
}

/// Single contact birthday entry returned by `contacts.getBirthdays`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactBirthday {
    /// Contact user ID.
    pub user_id: UserId,
    /// Contact birthday.
    pub birthday: Birthday,
}

/// Options for `contacts.blockFromReplies` (`window/window_peer_menu.cpp:2845-2854`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockFromRepliesOptions {
    /// Delete the single reply message (`flags.0`).
    pub delete_message: bool,
    /// Delete all replies from that sender (`flags.1`).
    pub delete_history: bool,
    /// Report the sender for spam (`flags.2`).
    pub report_spam: bool,
}

/// Updated contact status entry returned by `contacts.getStatuses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactStatusUpdate {
    /// Contact user ID.
    pub user_id: UserId,
    /// Updated `LastseenStatus` (merged against cached pseudo-online state if loaded).
    pub status: LastseenStatus,
    /// Whether the user was loaded in `Session` and its `lastseen` changed.
    pub changed: bool,
}

/// Sponsored peer search entry returned by `contacts.getSponsoredPeers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsoredPeerEntry {
    /// Target sponsored peer ID (hydrated into `Session`).
    pub peer_id: PeerId,
    /// Opaque server `random_id` bytes.
    pub random_id: Vec<u8>,
    /// Optional sponsor disclosure string (`sponsor_info`).
    pub sponsor_info: Option<String>,
    /// Optional extra info string (`additional_info`).
    pub additional_info: Option<String>,
}

/// Parameters for `photos.uploadContactProfilePhoto` (`api/api_peer_photo.cpp:611-631, 770-796`).
#[derive(Debug, Clone, PartialEq)]
pub struct UploadContactProfilePhotoParams {
    /// Target contact user ID.
    pub user_id: UserId,
    /// Suggest the photo to the contact (`flags.3 = suggest`).
    pub suggest: bool,
    /// Save as a personal contact photo visible only to us (`flags.4 = save`).
    pub save: bool,
    /// Optional uploaded static photo file (`flags.0 = file`).
    pub file: Option<tl_enums::InputFile>,
    /// Optional uploaded animated video avatar (`flags.1 = video`).
    pub video: Option<tl_enums::InputFile>,
    /// Optional video frame timestamp (`flags.2 = video_start_ts`; only encoded when `video.is_some()`).
    pub video_start_ts: Option<f64>,
    /// Optional emoji-markup avatar (`flags.5 = video_emoji_markup`).
    pub video_emoji_markup: Option<tl_enums::VideoSize>,
}

impl UploadContactProfilePhotoParams {
    /// Builds `PeerPhoto::clearPersonal` (`api/api_peer_photo.cpp:611-631`):
    /// `save = true`, `suggest = false`, and all media/markup fields `None`.
    #[must_use]
    pub const fn clear_personal(user_id: UserId) -> Self {
        Self {
            user_id,
            suggest: false,
            save: true,
            file: None,
            video: None,
            video_start_ts: None,
            video_emoji_markup: None,
        }
    }
}

fn resolve_input_user_strict(
    session: &Session,
    user_id: UserId,
) -> Result<tl_enums::InputUser, ApiError> {
    let iu = input_user_for(session, user_id);
    if matches!(iu, tl_enums::InputUser::Empty) {
        return Err(ApiError::Validation(format!(
            "user {} is not loaded with an access hash",
            user_id.0
        )));
    }
    Ok(iu)
}

fn resolve_input_peer_strict(
    session: &Session,
    peer_id: PeerId,
) -> Result<tl_enums::InputPeer, ApiError> {
    let ip = input_peer_for(session, peer_id);
    if matches!(ip, tl_enums::InputPeer::Empty) {
        return Err(ApiError::Validation(format!(
            "peer {} is not loaded",
            peer_id.raw()
        )));
    }
    Ok(ip)
}

/// Clears `PeerBarSettings` on `peer_id` in `session` (`PeerData::setBarSettings(0)`).
pub fn clear_peer_bar_settings(session: &mut Session, peer_id: PeerId) {
    if let Some(peer) = session.peer_mut(peer_id) {
        peer.common_mut().bar_settings = PeerBarSettings::empty();
        session
            .changes()
            .peer_updated(peer_id, PeerUpdateFlags::FULL_INFO);
    }
}

/// Clears local contact status (`ContactStatus::NotContact`, `CONTACT | MUTUAL_CONTACT` flags)
/// for each user in `user_ids` and emits `PeerUpdateFlags::IS_CONTACT`.
pub fn apply_deleted_contacts_local(session: &mut Session, user_ids: &[UserId]) {
    for &uid in user_ids {
        if let Some(user) = session.user_mut(uid) {
            user.contact_status = ContactStatus::NotContact;
            user.flags
                .remove(UserFlags::CONTACT | UserFlags::MUTUAL_CONTACT);
            session
                .changes()
                .peer_updated(PeerId::from_user(uid), PeerUpdateFlags::IS_CONTACT);
        }
    }
}

/// Applies a `contacts.contactBirthdays` response into `session` (`promo_suggestions.cpp:295-322`),
/// updating `user.birthday` and returning non-self, accessible, non-blocked contact birthdays.
pub fn apply_contact_birthdays(
    session: &mut Session,
    resp: tl_enums::contacts::ContactBirthdays,
) -> Vec<ContactBirthday> {
    let tl_enums::contacts::ContactBirthdays::ContactBirthdays(resp) = resp;
    apply_tl_users_and_chats(session, &resp.users, &[]);

    let mut out = Vec::with_capacity(resp.contacts.len());
    for entry in resp.contacts {
        let tl_enums::ContactBirthday::ContactBirthday(cb) = entry;
        let tl_enums::Birthday::Birthday(bd) = cb.birthday;
        let uid = UserId::from_tl(cb.contact_id);
        let birthday = Birthday {
            day: bd.day.clamp(0, 255) as u8,
            month: bd.month.clamp(0, 255) as u8,
            year: bd.year.unwrap_or(0).clamp(0, u16::MAX as i32) as u16,
        };
        let include = if let Some(user) = session.user_mut(uid) {
            user.birthday = Some(birthday);
            let skip = user.is_self()
                || user.is_inaccessible()
                || user.common.block_status == BlockStatus::Blocked
                || user.flags.contains(UserFlags::BLOCKED);
            session
                .changes()
                .peer_updated(PeerId::from_user(uid), PeerUpdateFlags::BIRTHDAY);
            !skip
        } else {
            false
        };
        if include {
            out.push(ContactBirthday {
                user_id: uid,
                birthday,
            });
        }
    }
    out
}

/// Applies `Vector<ContactStatus>` returned by `contacts.getStatuses` into `session`
/// (`apiwrap.cpp:2213-2226`).
pub fn apply_contact_statuses(
    session: &mut Session,
    statuses: &[tl_enums::ContactStatus],
) -> Vec<ContactStatusUpdate> {
    let mut out = Vec::with_capacity(statuses.len());
    for entry in statuses {
        let tl_enums::ContactStatus::ContactStatus(cs) = entry;
        let uid = UserId::from_tl(cs.user_id);
        let input = user_status_from_tl(&cs.status);
        if let Some(user) = session.user_mut(uid) {
            let next = LastseenStatus::from_api(input, user.lastseen);
            let changed = user.lastseen != next;
            if changed {
                user.lastseen = next;
                session
                    .changes()
                    .peer_updated(PeerId::from_user(uid), PeerUpdateFlags::ONLINE_STATUS);
            }
            out.push(ContactStatusUpdate {
                user_id: uid,
                status: next,
                changed,
            });
        } else {
            out.push(ContactStatusUpdate {
                user_id: uid,
                status: LastseenStatus::from_api(input, LastseenStatus::default()),
                changed: false,
            });
        }
    }
    out
}

/// Applies a `contacts.SponsoredPeers` response into `session` (`api/api_peer_search.cpp:106-126`).
pub fn apply_sponsored_peers(
    session: &mut Session,
    resp: tl_enums::contacts::SponsoredPeers,
) -> Vec<SponsoredPeerEntry> {
    match resp {
        tl_enums::contacts::SponsoredPeers::Empty => Vec::new(),
        tl_enums::contacts::SponsoredPeers::SponsoredPeers(data) => {
            apply_tl_users_and_chats(session, &data.users, &data.chats);
            let mut out = Vec::with_capacity(data.peers.len());
            for sp in data.peers {
                let tl_enums::SponsoredPeer::SponsoredPeer(sp) = sp;
                let peer_id = peer_id_from_tl(&sp.peer);
                if !peer_id.is_zero() {
                    out.push(SponsoredPeerEntry {
                        peer_id,
                        random_id: sp.random_id,
                        sponsor_info: sp.sponsor_info,
                        additional_info: sp.additional_info,
                    });
                }
            }
            out
        }
    }
}

/// Applies deleted own profile photo IDs returned by `photos.deletePhotos` to `session`.
pub fn apply_deleted_photos_local(session: &mut Session, deleted: &[i64]) {
    let self_uid = session.self_user_id();
    if let Some(self_user) = session.user_mut(self_uid) {
        let current_id = self_user.common.userpic_photo_id.to_tl();
        if current_id != 0 && deleted.contains(&current_id) {
            self_user.common.userpic_photo_id = PhotoId::ZERO;
            self_user.common.userpic_has_video = false;
            session
                .changes()
                .peer_updated(PeerId::from_user(self_uid), PeerUpdateFlags::PHOTO);
        }
    }
}

/// Applies a `photos.Photo` reply from `photos.uploadContactProfilePhoto` to `session` and
/// returns the resulting server photo ID (`0` when cleared to `photoEmpty`).
pub fn apply_uploaded_contact_photo_local(
    session: &mut Session,
    user_id: UserId,
    save: bool,
    is_clear: bool,
    has_video: bool,
    resp: tl_enums::photos::Photo,
) -> i64 {
    let tl_enums::photos::Photo::Photo(resp) = resp;
    apply_tl_users_and_chats(session, &resp.users, &[]);

    let photo_id = match &resp.photo {
        tl_enums::Photo::Photo(p) => p.id,
        tl_enums::Photo::Empty(p) => p.id,
    };

    if save && let Some(user) = session.user_mut(user_id) {
        if is_clear {
            user.flags.remove(UserFlags::PERSONAL_PHOTO);
            if matches!(resp.photo, tl_enums::Photo::Empty(_)) && resp.users.is_empty() {
                user.common.userpic_photo_id = PhotoId::ZERO;
                user.common.userpic_has_video = false;
            }
            session
                .changes()
                .peer_updated(PeerId::from_user(user_id), PeerUpdateFlags::PHOTO);
        } else if photo_id != 0 {
            user.flags.insert(UserFlags::PERSONAL_PHOTO);
            if resp.users.is_empty() {
                user.common.userpic_photo_id = PhotoId::from_tl(photo_id);
                user.common.userpic_has_video = has_video;
            }
            session
                .changes()
                .peer_updated(PeerId::from_user(user_id), PeerUpdateFlags::PHOTO);
        }
    }

    photo_id
}

/// Deletes one or more contacts via `contacts.deleteContacts`
/// (`boxes/contacts_box.cpp:325`, `boxes/peers/edit_contact_box.cpp:762`).
///
/// Clears local contact status for the deleted users in `session` and returns the raw
/// `tl_enums::Updates` so the caller can feed it through `UpdatesHandler` / `SessionRuntime`.
pub async fn delete_contacts<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    user_ids: &[UserId],
) -> Result<tl_enums::Updates, ApiError> {
    if user_ids.is_empty() {
        return Err(ApiError::Validation(
            "delete_contacts requires at least one user".to_owned(),
        ));
    }
    let mut inputs = Vec::with_capacity(user_ids.len());
    for &uid in user_ids {
        inputs.push(resolve_input_user_strict(session, uid)?);
    }
    let updates = rpc
        .invoke(&tl_fns::contacts::DeleteContacts { id: inputs })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;

    apply_deleted_contacts_local(session, user_ids);
    Ok(updates)
}

/// Accepts / shares phone number with a contact via `contacts.acceptContact`
/// (`history/view/history_view_contact_status.cpp:916-925`).
///
/// Clears `PeerBarSettings` on the user and returns the server `Updates`.
pub async fn accept_contact<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    user_id: UserId,
) -> Result<tl_enums::Updates, ApiError> {
    let id = resolve_input_user_strict(session, user_id)?;
    let peer_id = PeerId::from_user(user_id);
    clear_peer_bar_settings(session, peer_id);

    let updates = rpc
        .invoke(&tl_fns::contacts::AcceptContact { id })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(updates)
}

/// Exports a temporary contact QR / deep-link token via `contacts.exportContactToken`
/// (`info/profile/info_profile_qr_link.cpp:197-232`).
pub async fn export_contact_token<C: RpcClient>(rpc: &C) -> Result<ExportedContactToken, ApiError> {
    let tl_enums::ExportedContactToken::ExportedContactToken(resp) = rpc
        .invoke(&tl_fns::contacts::ExportContactToken {})
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    if resp.url.is_empty() {
        return Err(ApiError::ServerError(
            "contacts.exportContactToken returned an empty URL".to_owned(),
        ));
    }
    Ok(ExportedContactToken {
        url: resp.url,
        expires: resp.expires,
    })
}

/// Resolves a temporary contact token via `contacts.importContactToken`
/// (`window/window_session_controller_contact.cpp:18-54`).
///
/// Hydrates the returned `User` into `session` and rejects deleted/inaccessible users.
pub async fn import_contact_token<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    token: &str,
) -> Result<PeerId, ApiError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(ApiError::Validation(
            "contact token must not be empty".to_owned(),
        ));
    }
    let user_tl = rpc
        .invoke(&tl_fns::contacts::ImportContactToken {
            token: trimmed.to_owned(),
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    let Some(peer_id) = apply_tl_user(session, &user_tl) else {
        return Err(ApiError::ServerError(
            "contacts.importContactToken returned an empty user".to_owned(),
        ));
    };
    if session
        .user(peer_id.to_user_or_zero())
        .is_none_or(|u| u.is_inaccessible())
    {
        return Err(ApiError::ServerError(
            "contact link is invalid or user is inaccessible".to_owned(),
        ));
    }
    Ok(peer_id)
}

/// Exports a paid-message fee-waiver token via `contacts.exportNoPaidMessagesToken`
/// (`api/api_premium.cpp:530-548`).
pub async fn export_no_paid_messages_token<C: RpcClient>(
    rpc: &C,
    revoke: bool,
    temporary: bool,
) -> Result<ExportedNoPaidMessagesToken, ApiError> {
    let tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(resp) = rpc
        .invoke(&tl_fns::contacts::ExportNoPaidMessagesToken { revoke, temporary })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(ExportedNoPaidMessagesToken {
        token: resp.token,
        url: resp.url,
        expires: resp.expires,
    })
}

/// Redeems a paid-message fee-waiver token via `contacts.importNoPaidMessagesToken`
/// (`api/api_premium.cpp:487-528`).
pub async fn import_no_paid_messages_token<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    user_id: UserId,
    token: &str,
) -> Result<NoPaidMessagesTokenImport, ApiError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(ApiError::Validation(
            "no-paid-messages token must not be empty".to_owned(),
        ));
    }
    let input_user = resolve_input_user_strict(session, user_id)?;
    let priced_before = session.user(user_id).is_some_and(|u| {
        u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE) || u.stars_per_message > 0
    });
    let user_tl = rpc
        .invoke(&tl_fns::contacts::ImportNoPaidMessagesToken {
            user_id: input_user,
            token: trimmed.to_owned(),
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    let Some(peer_id) = apply_tl_user(session, &user_tl) else {
        return Err(ApiError::ServerError(
            "contacts.importNoPaidMessagesToken returned an empty user".to_owned(),
        ));
    };
    let priced_after = session.user(peer_id.to_user_or_zero()).is_some_and(|u| {
        u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE) || u.stars_per_message > 0
    });
    Ok(NoPaidMessagesTokenImport {
        peer_id,
        price_removed: priced_before && !priced_after,
    })
}

/// Updates or clears (when `note.text.is_empty()`) the private contact note for `user_id`
/// via `contacts.updateContactNote` (`info/profile/info_profile_actions.cpp:789-802`).
pub async fn update_contact_note<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    user_id: UserId,
    note: &TextWithEntities,
) -> Result<bool, ApiError> {
    let id = resolve_input_user_strict(session, user_id)?;
    let tl_note = if note.text.is_empty() {
        tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: String::new(),
            entities: Vec::new(),
        })
    } else {
        text_with_entities_to_tl(note, session)
    };
    let ok = rpc
        .invoke(&tl_fns::contacts::UpdateContactNote { id, note: tl_note })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    if let Some(user) = session.user_mut(user_id) {
        user.note = if note.text.is_empty() {
            TextWithEntities::default()
        } else {
            note.clone()
        };
        session.changes().peer_updated(
            PeerId::from_user(user_id),
            PeerUpdateFlags::CONTACT_NOTE | PeerUpdateFlags::FULL_INFO,
        );
    }
    Ok(ok)
}

/// Fetches contact birthdays via `contacts.getBirthdays`
/// (`data/components/promo_suggestions.cpp:284-323`).
///
/// Hydrates returned users into `session`, updates `user.birthday` for each entry, and filters
/// out `self`, inaccessible/deleted, and blocked users (matching `promo_suggestions.cpp:309-313`).
pub async fn get_birthdays<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
) -> Result<Vec<ContactBirthday>, ApiError> {
    let resp = rpc
        .invoke(&tl_fns::contacts::GetBirthdays {})
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(apply_contact_birthdays(session, resp))
}

/// Blocks a user from the Replies chat via `contacts.blockFromReplies`
/// (`window/window_peer_menu.cpp:2845-2854`).
pub async fn block_from_replies<C: RpcClient>(
    rpc: &C,
    msg_id: i32,
    options: BlockFromRepliesOptions,
) -> Result<tl_enums::Updates, ApiError> {
    if msg_id <= 0 {
        return Err(ApiError::Validation(
            "block_from_replies requires a positive msg_id".to_owned(),
        ));
    }
    let updates = rpc
        .invoke(&tl_fns::contacts::BlockFromReplies {
            delete_message: options.delete_message,
            delete_history: options.delete_history,
            report_spam: options.report_spam,
            msg_id,
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(updates)
}

/// Bulk-replaces either the stories blocklist (`my_stories_from = true`,
/// `api/api_stories.cpp:595-621`) or the general blocked-peers list (`my_stories_from = false`)
/// via `contacts.setBlocked`.
pub async fn set_blocked<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    peers: &[PeerId],
    my_stories_from: bool,
    limit: i32,
) -> Result<bool, ApiError> {
    let mut ids = Vec::with_capacity(peers.len());
    let mut resolved_peers = Vec::with_capacity(peers.len());
    for &pid in peers {
        let ip = input_peer_for(session, pid);
        if !matches!(ip, tl_enums::InputPeer::Empty) {
            ids.push(ip);
            resolved_peers.push(pid);
        }
    }
    let effective_limit = limit.max(ids.len() as i32);
    let ok = rpc
        .invoke(&tl_fns::contacts::SetBlocked {
            my_stories_from,
            id: ids,
            limit: effective_limit,
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    if ok && !my_stories_from {
        for pid in resolved_peers {
            session.set_peer_blocked(pid, true);
        }
    }
    Ok(ok)
}

/// Enables or disables server top-peers tracking via `contacts.toggleTopPeers`
/// (`data/components/top_peers.cpp:195-216`).
pub async fn toggle_top_peers<C: RpcClient>(rpc: &C, enabled: bool) -> Result<bool, ApiError> {
    rpc.invoke(&tl_fns::contacts::ToggleTopPeers { enabled })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))
}

/// Refreshes online/last-seen statuses for all contacts via `contacts.getStatuses`
/// (`apiwrap.cpp:2210-2229`).
///
/// Updates every loaded user in `session` via `LastseenStatus::from_api` (preserving local
/// pseudo-online state on `Recently`) and emits `PeerUpdateFlags::ONLINE_STATUS` on change.
pub async fn get_statuses<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
) -> Result<Vec<ContactStatusUpdate>, ApiError> {
    let statuses = rpc
        .invoke(&tl_fns::contacts::GetStatuses {})
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(apply_contact_statuses(session, &statuses))
}

/// Queries sponsored search peers via `contacts.getSponsoredPeers`
/// (`api/api_peer_search.cpp:100-131`).
pub async fn get_sponsored_peers<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    query: &str,
) -> Result<Vec<SponsoredPeerEntry>, ApiError> {
    let resp = rpc
        .invoke(&tl_fns::contacts::GetSponsoredPeers {
            q: query.to_owned(),
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(apply_sponsored_peers(session, resp))
}

/// Reports a peer for spam via `messages.reportSpam`
/// (`history/view/history_view_contact_status.cpp:967-969`, `window/window_peer_menu.cpp:2858-2860`)
/// and clears cached `PeerBarSettings` on `peer_id`.
pub async fn report_spam<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    peer_id: PeerId,
) -> Result<bool, ApiError> {
    let peer = resolve_input_peer_strict(session, peer_id)?;
    let ok = rpc
        .invoke(&tl_fns::messages::ReportSpam { peer })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    clear_peer_bar_settings(session, peer_id);
    Ok(ok)
}

/// Dismisses the peer settings / contact status bar via `messages.hidePeerSettingsBar`
/// (`history/view/history_view_contact_status.cpp:1014-1017, 1048-1051`)
/// and clears cached `PeerBarSettings` on `peer_id`.
pub async fn hide_peer_settings_bar<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    peer_id: PeerId,
) -> Result<bool, ApiError> {
    let peer = resolve_input_peer_strict(session, peer_id)?;
    clear_peer_bar_settings(session, peer_id);
    rpc.invoke(&tl_fns::messages::HidePeerSettingsBar { peer })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))
}

/// Deletes one or more of the signed-in user's profile photos via `photos.deletePhotos`
/// (`api/api_peer_photo.cpp:601-607`).
///
/// Clears `self_user.common.userpic_photo_id` if the current primary profile photo ID is among
/// the deleted IDs returned by the server.
pub async fn delete_photos<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    photos: &[tl_enums::InputPhoto],
) -> Result<Vec<i64>, ApiError> {
    if photos.is_empty() {
        return Ok(Vec::new());
    }
    let deleted = rpc
        .invoke(&tl_fns::photos::DeletePhotos {
            id: photos.to_vec(),
        })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;

    apply_deleted_photos_local(session, &deleted);
    Ok(deleted)
}

/// Uploads, suggests, or clears a personal contact profile photo via
/// `photos.uploadContactProfilePhoto` (`api/api_peer_photo.cpp:611-631, 770-796`).
///
/// Exact tdesktop flag rules:
/// - `video_start_ts` is only included when `video.is_some()` (`api_peer_photo.cpp:776`).
/// - Clearing a personal photo sets `save = true, suggest = false` and omits `file`, `video`,
///   `video_start_ts`, and `video_emoji_markup` (`PeerPhoto::clearPersonal`, `api_peer_photo.cpp:611-618`).
pub async fn upload_contact_profile_photo<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    params: UploadContactProfilePhotoParams,
) -> Result<i64, ApiError> {
    if params.suggest && params.save {
        return Err(ApiError::Validation(
            "upload_contact_profile_photo cannot set both suggest and save".to_owned(),
        ));
    }
    if !params.suggest && !params.save {
        return Err(ApiError::Validation(
            "upload_contact_profile_photo requires either suggest or save".to_owned(),
        ));
    }
    let is_clear = params.save
        && params.file.is_none()
        && params.video.is_none()
        && params.video_emoji_markup.is_none();
    if params.suggest
        && params.file.is_none()
        && params.video.is_none()
        && params.video_emoji_markup.is_none()
    {
        return Err(ApiError::Validation(
            "suggesting a contact profile photo requires file, video, or video_emoji_markup"
                .to_owned(),
        ));
    }

    let user_id = resolve_input_user_strict(session, params.user_id)?;
    let has_video = params.video.is_some();
    let req = tl_fns::photos::UploadContactProfilePhoto {
        suggest: params.suggest,
        save: params.save,
        user_id,
        file: params.file,
        video: params.video,
        video_start_ts: if has_video {
            params.video_start_ts
        } else {
            None
        },
        video_emoji_markup: params.video_emoji_markup,
    };
    let resp = rpc
        .invoke(&req)
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;

    Ok(apply_uploaded_contact_photo_local(
        session,
        params.user_id,
        params.save,
        is_clear,
        has_video,
        resp,
    ))
}

/// Hides a promoted / PSA peer from the top of the dialog list via `help.hidePromoData`
/// (`window/window_peer_menu.cpp:525-530`).
pub async fn hide_promo_data<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
) -> Result<bool, ApiError> {
    let peer = resolve_input_peer_strict(session, peer_id)?;
    rpc.invoke(&tl_fns::help::HidePromoData { peer })
        .await
        .map_err(|e| ApiError::Invocation(e.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tg_data::{Channel, ChannelId, PeerBarSettings, PeerId, PeerUpdateFlags, Session, UserId};
    use tg_tl::{
        Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums,
        functions as tl_fns, types as tl_types,
    };

    use super::*;
    use crate::rpc::{DcId, InvocationError};

    type RpcFrame = (u32, Vec<u8>);

    #[derive(Clone, Default)]
    struct MockRpc {
        recorded: Arc<Mutex<Vec<RpcFrame>>>,
        responses: Arc<Mutex<VecDeque<RpcFrame>>>,
    }

    impl MockRpc {
        fn enqueue<T: Serializable>(&self, ctor: u32, resp: &T) {
            self.responses
                .lock()
                .expect("lock")
                .push_back((ctor, resp.to_bytes()));
        }

        fn recorded(&self) -> Vec<(u32, Vec<u8>)> {
            self.recorded.lock().expect("lock").clone()
        }
    }

    impl RpcClient for MockRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            let bytes = req.to_bytes();
            let ctor = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            self.recorded.lock().expect("lock").push((ctor, bytes));
            let (expected, raw) = self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .ok_or(InvocationError::Dropped)?;
            assert_eq!(ctor, expected);
            R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            self.invoke(req).await
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn set_main_dc_id(&self, _dc_id: DcId) {}
    }

    fn seed_session() -> Session {
        let mut s = Session::new(UserId(10));
        let mut self_u = tg_data::User::new(UserId(10), true);
        self_u.common.userpic_photo_id = PhotoId::from_tl(500);
        s.upsert_user(self_u, PeerUpdateFlags::NAME);

        let mut u = tg_data::User::new(UserId(20), false);
        u.set_access_hash(2020);
        u.contact_status = ContactStatus::Contact;
        u.flags
            .insert(UserFlags::CONTACT | UserFlags::HAS_STARS_PER_MESSAGE);
        u.stars_per_message = 15;
        u.common.bar_settings = PeerBarSettings::REPORT_SPAM | PeerBarSettings::ADD_CONTACT;
        s.upsert_user(u, PeerUpdateFlags::NAME);

        let mut ch = Channel::new(ChannelId(30));
        ch.access_hash = 3030;
        ch.common.bar_settings = PeerBarSettings::REPORT_SPAM;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);
        s
    }

    #[tokio::test]
    async fn tg_api_contacts_extras_smoke_all_18() {
        let rpc = MockRpc::default();
        let mut s = seed_session();
        let empty_upd = tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_000,
            seq: 0,
        });

        // 1. delete_contacts
        rpc.enqueue(tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID, &empty_upd);
        delete_contacts(&rpc, &mut s, &[UserId(20)]).await.unwrap();
        assert!(!s.user(UserId(20)).unwrap().is_contact());

        // 2. accept_contact
        rpc.enqueue(tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID, &empty_upd);
        accept_contact(&rpc, &mut s, UserId(20)).await.unwrap();
        assert!(s.user(UserId(20)).unwrap().common.bar_settings.is_empty());

        // 3. export_contact_token
        rpc.enqueue(
            tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID,
            &tl_enums::ExportedContactToken::ExportedContactToken(tl_types::ExportedContactToken {
                url: "https://t.me/contact/t1".to_owned(),
                expires: 1234,
            }),
        );
        let tok = export_contact_token(&rpc).await.unwrap();
        assert_eq!(tok.url, "https://t.me/contact/t1");

        // 4. import_contact_token (rejects empty locally)
        assert!(import_contact_token(&rpc, &mut s, " ").await.is_err());

        // 5. export_no_paid_messages_token
        rpc.enqueue(
            tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID,
            &tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(
                tl_types::ExportedNoPaidMessagesToken {
                    token: "nt".to_owned(),
                    url: "https://t.me/u?free=nt".to_owned(),
                    expires: None,
                },
            ),
        );
        let fee = export_no_paid_messages_token(&rpc, true, false)
            .await
            .unwrap();
        assert_eq!(fee.token, "nt");
        assert_eq!(fee.expires, None);

        // 6. import_no_paid_messages_token (empty token rejected locally)
        assert!(
            import_no_paid_messages_token(&rpc, &mut s, UserId(20), "")
                .await
                .is_err()
        );

        // 7. update_contact_note
        rpc.enqueue(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID, &true);
        assert!(
            update_contact_note(&rpc, &mut s, UserId(20), &TextWithEntities::plain("hi"))
                .await
                .unwrap()
        );
        assert_eq!(s.user(UserId(20)).unwrap().note.text, "hi");

        // 8. get_birthdays
        rpc.enqueue(
            tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID,
            &tl_enums::contacts::ContactBirthdays::ContactBirthdays(
                tl_types::contacts::ContactBirthdays {
                    contacts: vec![tl_enums::ContactBirthday::ContactBirthday(
                        tl_types::ContactBirthday {
                            contact_id: 20,
                            birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                                day: 5,
                                month: 6,
                                year: Some(2000),
                            }),
                        },
                    )],
                    users: Vec::new(),
                },
            ),
        );
        let bdays = get_birthdays(&rpc, &mut s).await.unwrap();
        assert_eq!(bdays.len(), 1);
        assert_eq!(bdays[0].birthday.day, 5);

        // 9. block_from_replies
        rpc.enqueue(
            tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID,
            &empty_upd,
        );
        block_from_replies(
            &rpc,
            42,
            BlockFromRepliesOptions {
                delete_message: true,
                delete_history: false,
                report_spam: true,
            },
        )
        .await
        .unwrap();

        // 10. set_blocked
        rpc.enqueue(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID, &true);
        assert!(
            set_blocked(&rpc, &mut s, &[PeerId::from_user(UserId(20))], true, 3)
                .await
                .unwrap()
        );

        // 11. toggle_top_peers
        rpc.enqueue(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID, &true);
        assert!(toggle_top_peers(&rpc, false).await.unwrap());

        // 12. get_statuses
        rpc.enqueue(
            tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
            &vec![tl_enums::ContactStatus::ContactStatus(
                tl_types::ContactStatus {
                    user_id: 20,
                    status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
                        expires: 1_900_000_000,
                    }),
                },
            )],
        );
        let st = get_statuses(&rpc, &mut s).await.unwrap();
        assert_eq!(st.len(), 1);
        assert!(st[0].changed);

        // 13. get_sponsored_peers
        rpc.enqueue(
            tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID,
            &tl_enums::contacts::SponsoredPeers::Empty,
        );
        assert!(
            get_sponsored_peers(&rpc, &mut s, "abc")
                .await
                .unwrap()
                .is_empty()
        );

        // 14 & 15. report_spam & hide_peer_settings_bar
        rpc.enqueue(tl_fns::messages::ReportSpam::CONSTRUCTOR_ID, &true);
        assert!(
            report_spam(&rpc, &mut s, PeerId::from_channel(ChannelId(30)))
                .await
                .unwrap()
        );
        assert!(
            s.channel(ChannelId(30))
                .unwrap()
                .common
                .bar_settings
                .is_empty()
        );

        rpc.enqueue(tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID, &true);
        assert!(
            hide_peer_settings_bar(&rpc, &mut s, PeerId::from_channel(ChannelId(30)))
                .await
                .unwrap()
        );

        // 16. delete_photos
        rpc.enqueue(tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID, &vec![500i64]);
        let del = delete_photos(
            &rpc,
            &mut s,
            &[tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: 500,
                access_hash: 5000,
                file_reference: vec![1],
            })],
        )
        .await
        .unwrap();
        assert_eq!(del, vec![500]);
        assert_eq!(
            s.user(UserId(10)).unwrap().common.userpic_photo_id,
            PhotoId::ZERO
        );

        // 17. upload_contact_profile_photo (clearPersonal)
        rpc.enqueue(
            tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
            &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
                photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
                users: Vec::new(),
            }),
        );
        let pid = upload_contact_profile_photo(
            &rpc,
            &mut s,
            UploadContactProfilePhotoParams::clear_personal(UserId(20)),
        )
        .await
        .unwrap();
        assert_eq!(pid, 0);

        // 18. hide_promo_data
        rpc.enqueue(tl_fns::help::HidePromoData::CONSTRUCTOR_ID, &true);
        assert!(
            hide_promo_data(&rpc, &s, PeerId::from_channel(ChannelId(30)))
                .await
                .unwrap()
        );

        assert_eq!(rpc.recorded().len(), 16);
    }
}
