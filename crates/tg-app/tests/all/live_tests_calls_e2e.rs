//! End-to-end integration tests for conference E2E (`tde2e`), chain block sync, emoji
//! verification, in-call plain & encrypted messages, paid Star reaction batches, screen-share
//! source enumeration, and `tg://call?slug=` conference deep links.
//!
//! Runs completely offline against an in-process `FakeConferenceRpc` (`RpcClient`) with virtual
//! audio backends (no network, no real Telegram accounts).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_calls::{
    ChainBlock, E2eKeyPair, GroupCallState, SUBCHAIN_BROADCAST, SUBCHAIN_STATE,
    types::{CallId, UserId as CallsUserId},
};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_voip::{
    AudioBackendKind, BroadcastChunkScale, GroupMediaConfig, JitterBufferConfig, MediaEngineConfig,
    NetworkType, TransportWireMode, VOIP_SAMPLE_RATE, VoiceDspConfig,
};
use tokio::sync::broadcast;

use tg_app::calls::AppCallsHost;
use tg_app::calls::backend::{CallsBackend, from_calls_peer_id};
use tg_app::calls::group_panel::GroupCallPanelState;
use tg_app::calls::screen_picker::ScreenSourceKind;
use tg_app::calls::theme::CallPalette;
use tg_app::live::LiveSession;
use tg_app::live::convert::now_unix_i32;
use tg_app::stand_in::types::PeerId;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn make_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 11),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Lab".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn make_tl_channel(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: true,
        call_not_empty: true,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: true,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 3),
        title: title.to_owned(),
        username: Some("streamlab".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(120),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn make_virtual_configs() -> (MediaEngineConfig, GroupMediaConfig) {
    let media = MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 2,
        dsp: VoiceDspConfig::bypass(),
        jitter: JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        },
        wire_mode: TransportWireMode::MtProtoRaw,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_network_type: NetworkType::WiFi,
        quality_tick_ms: 250,
        ..MediaEngineConfig::default()
    };
    let group = GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 2,
        dsp: VoiceDspConfig::bypass(),
        jitter: JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        },
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_muted: true,
        broadcast_scale: BroadcastChunkScale::Ms1000,
        ..GroupMediaConfig::default()
    };
    (media, group)
}

#[derive(Debug, Clone)]
struct RecordedPlainMessage {
    sender_uid: i64,
    random_id: i64,
    text: String,
    allow_paid_stars: Option<i64>,
}

#[derive(Debug, Clone)]
struct RecordedDeleteParticipants {
    only_left: bool,
    kick: bool,
    ids: Vec<i64>,
    block: Vec<u8>,
}

struct FakeConferenceServer {
    call_id: i64,
    access_hash: i64,
    slug: String,
    version: i32,
    next_msg_id: i32,
    subchain_0: Vec<Vec<u8>>,
    subchain_1: Vec<Vec<u8>>,
    updates_tx: HashMap<i64, broadcast::Sender<tl_enums::Updates>>,
    users: HashMap<i64, tl_types::User>,
    participants: Vec<(i64, i32)>, // (uid, ssrc)
    created_calls: Vec<tl_fns::phone::CreateConferenceCall>,
    joined_calls: Vec<tl_fns::phone::JoinGroupCall>,
    chain_block_queries: Vec<(tl_enums::InputGroupCall, i32, i32, i32)>,
    broadcasts_sent: Vec<Vec<u8>>,
    deleted_participants: Vec<RecordedDeleteParticipants>,
    encrypted_messages_sent: Vec<(i64, Vec<u8>)>,
    plain_messages_sent: Vec<RecordedPlainMessage>,
    deleted_message_ids: Vec<Vec<i32>>,
    stars_total: i64,
    stars_donors: Vec<(i64, i64)>,
}

impl FakeConferenceServer {
    fn new(call_id: i64, access_hash: i64, slug: &str) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            call_id,
            access_hash,
            slug: slug.to_owned(),
            version: 1,
            next_msg_id: 500,
            subchain_0: Vec::new(),
            subchain_1: Vec::new(),
            updates_tx: HashMap::new(),
            users: HashMap::new(),
            participants: Vec::new(),
            created_calls: Vec::new(),
            joined_calls: Vec::new(),
            chain_block_queries: Vec::new(),
            broadcasts_sent: Vec::new(),
            deleted_participants: Vec::new(),
            encrypted_messages_sent: Vec::new(),
            plain_messages_sent: Vec::new(),
            deleted_message_ids: Vec::new(),
            stars_total: 0,
            stars_donors: Vec::new(),
        }))
    }

    fn input_call(&self) -> tl_enums::InputGroupCall {
        tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: self.call_id,
            access_hash: self.access_hash,
        })
    }

    fn active_group_call_tl(&self, conference: bool, rtmp_stream: bool) -> tl_enums::GroupCall {
        tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
            join_muted: false,
            can_change_join_muted: true,
            join_date_asc: false,
            schedule_start_subscribed: false,
            can_start_video: true,
            record_video_active: false,
            rtmp_stream,
            listeners_hidden: false,
            conference,
            creator: true,
            messages_enabled: true,
            can_change_messages_enabled: true,
            min: false,
            id: self.call_id,
            access_hash: self.access_hash,
            participants_count: self.participants.len().max(1) as i32,
            title: Some(if conference {
                "Encrypted Sync".to_owned()
            } else {
                "Stream Room".to_owned()
            }),
            stream_dc_id: None,
            record_start_date: None,
            schedule_date: None,
            unmuted_video_count: None,
            unmuted_video_limit: 8,
            version: self.version,
            invite_link: Some(format!("https://t.me/call/?slug={}", self.slug)),
            send_paid_messages_stars: None,
            default_send_as: None,
        })
    }

    fn wrap_updates(&self, updates: Vec<tl_enums::Update>) -> tl_enums::Updates {
        tl_enums::Updates::Updates(tl_types::Updates {
            updates,
            users: self
                .users
                .values()
                .cloned()
                .map(tl_enums::User::User)
                .collect(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 1,
        })
    }

    fn push_update_to(&self, uid: i64, update: tl_enums::Update) {
        if let Some(tx) = self.updates_tx.get(&uid) {
            let _ = tx.send(self.wrap_updates(vec![update]));
        }
    }

    fn broadcast_update(&self, update: tl_enums::Update, exclude_uid: Option<i64>) {
        for (&uid, tx) in &self.updates_tx {
            if Some(uid) == exclude_uid {
                continue;
            }
            let _ = tx.send(self.wrap_updates(vec![update.clone()]));
        }
    }

    fn make_participant_tl(
        &self,
        uid: i64,
        ssrc: i32,
        is_self: bool,
    ) -> tl_enums::GroupCallParticipant {
        tl_enums::GroupCallParticipant::GroupCallParticipant(tl_types::GroupCallParticipant {
            muted: true,
            left: false,
            can_self_unmute: true,
            just_joined: false,
            versioned: true,
            min: false,
            muted_by_you: false,
            volume_by_admin: false,
            self_: is_self,
            video_joined: false,
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid }),
            date: now_unix_i32(),
            active_date: None,
            source: ssrc,
            volume: Some(10_000),
            about: None,
            raise_hand_rating: None,
            video: None,
            presentation: None,
            paid_stars_total: None,
        })
    }
}

#[derive(Clone)]
struct FakeConferenceRpc {
    uid: i64,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    server: Arc<Mutex<FakeConferenceServer>>,
    recorded_ctors: Arc<Mutex<Vec<u32>>>,
}

impl FakeConferenceRpc {
    fn register(server: &Arc<Mutex<FakeConferenceServer>>, user: tl_types::User) -> Self {
        let uid = user.id;
        let (updates_tx, _) = broadcast::channel(128);
        {
            let mut srv = server.lock().expect("lock fake server");
            srv.users.insert(uid, user);
            srv.updates_tx.insert(uid, updates_tx.clone());
        }
        Self {
            uid,
            updates_tx,
            server: Arc::clone(server),
            recorded_ctors: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl RpcClient for FakeConferenceRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded_ctors.lock().expect("lock ctors").push(ctor);
        let resp_bytes = handle_conference_rpc(self.uid, ctor, &bytes, &self.server)?;
        R::Return::from_bytes(&resp_bytes).map_err(|e| InvocationError::Deserialize(e.to_string()))
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

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn extract_ssrc_from_params(params: &tl_enums::DataJSON, fallback: i32) -> i32 {
    let tl_enums::DataJSON::DataJSON(dj) = params;
    serde_json::from_str::<serde_json::Value>(&dj.data)
        .ok()
        .and_then(|v| v["ssrc"].as_i64())
        .map_or(fallback, |s| s as i32)
}

fn handle_conference_rpc(
    uid: i64,
    ctor: u32,
    bytes: &[u8],
    server: &Arc<Mutex<FakeConferenceServer>>,
) -> Result<Vec<u8>, InvocationError> {
    let mut srv = server.lock().expect("lock fake server");

    match ctor {
        tl_fns::phone::CreateConferenceCall::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::CreateConferenceCall::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let zero_block = req.block.clone().unwrap_or_default();
            let ssrc = req.params.as_ref().map_or(10_000 + uid as i32, |p| {
                extract_ssrc_from_params(p, 10_000 + uid as i32)
            });
            srv.created_calls.push(req);
            srv.subchain_0.clear();
            srv.subchain_0.push(zero_block.clone());
            srv.participants.clear();
            srv.participants.push((uid, ssrc));
            let upd_call = tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
                live_story: false,
                peer: None,
                call: srv.active_group_call_tl(true, false),
            });
            let upd_chain =
                tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                    call: srv.input_call(),
                    sub_chain_id: SUBCHAIN_STATE,
                    blocks: vec![zero_block],
                    next_offset: srv.subchain_0.len() as i32,
                });
            let upd_parts =
                tl_enums::Update::GroupCallParticipants(tl_types::UpdateGroupCallParticipants {
                    call: srv.input_call(),
                    participants: vec![srv.make_participant_tl(uid, ssrc, true)],
                    version: srv.version,
                });
            Ok(srv
                .wrap_updates(vec![upd_call, upd_chain, upd_parts])
                .to_bytes())
        }
        tl_fns::phone::GetGroupCallChainBlocks::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::GetGroupCallChainBlocks::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            srv.chain_block_queries.push((
                req.call.clone(),
                req.sub_chain_id,
                req.offset,
                req.limit,
            ));

            let chain = if req.sub_chain_id == SUBCHAIN_STATE {
                &srv.subchain_0
            } else {
                &srv.subchain_1
            };
            let (blocks, next_offset) = if req.offset < 0 {
                // `getLastChainBlock`: offset = -1, limit = 1 -> return latest block.
                let last = chain.last().cloned().into_iter().collect::<Vec<_>>();
                (last, chain.len() as i32)
            } else {
                let start = (req.offset as usize).min(chain.len());
                let end = start
                    .saturating_add(req.limit.max(0) as usize)
                    .min(chain.len());
                (chain[start..end].to_vec(), end as i32)
            };
            let upd =
                tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                    call: srv.input_call(),
                    sub_chain_id: req.sub_chain_id,
                    blocks,
                    next_offset,
                });
            Ok(srv.wrap_updates(vec![upd]).to_bytes())
        }
        tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::JoinGroupCall::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let join_block = req.block.clone();
            let is_conference = req.public_key.is_some() && join_block.is_some();
            let ssrc = extract_ssrc_from_params(&req.params, 10_000 + uid as i32);
            srv.joined_calls.push(req);

            if let Some(slot) = srv.participants.iter_mut().find(|(u, _)| *u == uid) {
                slot.1 = ssrc;
            } else {
                srv.participants.push((uid, ssrc));
            }
            srv.version += 1;

            let mut updates = vec![tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
                live_story: false,
                peer: None,
                call: srv.active_group_call_tl(is_conference, !is_conference),
            })];

            if let Some(block) = join_block {
                srv.subchain_0.push(block.clone());
                let next_offset = srv.subchain_0.len() as i32;
                let chain_upd =
                    tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                        call: srv.input_call(),
                        sub_chain_id: SUBCHAIN_STATE,
                        blocks: vec![block],
                        next_offset,
                    });
                updates.push(chain_upd.clone());
                // Fan out the new chain block to all other participants already in the call!
                srv.broadcast_update(chain_upd, Some(uid));
            }

            let part_list: Vec<tl_enums::GroupCallParticipant> = srv
                .participants
                .iter()
                .map(|&(p_uid, p_ssrc)| srv.make_participant_tl(p_uid, p_ssrc, p_uid == uid))
                .collect();
            updates.push(tl_enums::Update::GroupCallParticipants(
                tl_types::UpdateGroupCallParticipants {
                    call: srv.input_call(),
                    participants: part_list,
                    version: srv.version,
                },
            ));

            // Notify existing participants about the newcomer so SSRC <-> user gets registered.
            let joined_upd =
                tl_enums::Update::GroupCallParticipants(tl_types::UpdateGroupCallParticipants {
                    call: srv.input_call(),
                    participants: vec![srv.make_participant_tl(uid, ssrc, false)],
                    version: srv.version,
                });
            srv.broadcast_update(joined_upd, Some(uid));

            Ok(srv.wrap_updates(updates).to_bytes())
        }
        tl_fns::phone::SendConferenceCallBroadcast::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::SendConferenceCallBroadcast::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let blk = req.block;
            srv.broadcasts_sent.push(blk.clone());
            srv.subchain_1.push(blk.clone());
            let next_offset = srv.subchain_1.len() as i32;
            let upd =
                tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                    call: srv.input_call(),
                    sub_chain_id: SUBCHAIN_BROADCAST,
                    blocks: vec![blk],
                    next_offset,
                });
            // Broadcast verification subchain-1 block to ALL peers (including sender via response
            // and peers via push updates).
            srv.broadcast_update(upd.clone(), Some(uid));
            Ok(srv.wrap_updates(vec![upd]).to_bytes())
        }
        tl_fns::phone::DeleteConferenceCallParticipants::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::DeleteConferenceCallParticipants::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let block = req.block.clone();
            srv.deleted_participants.push(RecordedDeleteParticipants {
                only_left: req.only_left,
                kick: req.kick,
                ids: req.ids.clone(),
                block: block.clone(),
            });
            srv.subchain_0.push(block.clone());
            let next_offset = srv.subchain_0.len() as i32;
            let upd =
                tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                    call: srv.input_call(),
                    sub_chain_id: SUBCHAIN_STATE,
                    blocks: vec![block],
                    next_offset,
                });
            srv.broadcast_update(upd.clone(), Some(uid));
            Ok(srv.wrap_updates(vec![upd]).to_bytes())
        }
        tl_fns::phone::SendGroupCallEncryptedMessage::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::SendGroupCallEncryptedMessage::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            srv.encrypted_messages_sent
                .push((uid, req.encrypted_message.clone()));
            let upd = tl_enums::Update::GroupCallEncryptedMessage(
                tl_types::UpdateGroupCallEncryptedMessage {
                    call: srv.input_call(),
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid }),
                    encrypted_message: req.encrypted_message,
                },
            );
            srv.broadcast_update(upd, Some(uid));
            Ok(true.to_bytes())
        }
        tl_fns::phone::SendGroupCallMessage::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::SendGroupCallMessage::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let text = match &req.message {
                tl_enums::TextWithEntities::TextWithEntities(t) => t.text.clone(),
            };
            srv.plain_messages_sent.push(RecordedPlainMessage {
                sender_uid: uid,
                random_id: req.random_id,
                text: text.clone(),
                allow_paid_stars: req.allow_paid_stars,
            });
            if let Some(stars) = req.allow_paid_stars {
                srv.stars_total += stars;
                if let Some(entry) = srv.stars_donors.iter_mut().find(|(u, _)| *u == uid) {
                    entry.1 += stars;
                } else {
                    srv.stars_donors.push((uid, stars));
                }
            }
            let msg_id = srv.next_msg_id;
            srv.next_msg_id += 1;

            let upd_id = tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                id: msg_id,
                random_id: req.random_id,
            });
            let upd_msg = tl_enums::Update::GroupCallMessage(tl_types::UpdateGroupCallMessage {
                call: srv.input_call(),
                message: tl_enums::GroupCallMessage::GroupCallMessage(tl_types::GroupCallMessage {
                    from_admin: true,
                    id: msg_id,
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid }),
                    date: now_unix_i32(),
                    message: req.message,
                    paid_message_stars: req.allow_paid_stars,
                }),
            });
            srv.broadcast_update(upd_msg.clone(), Some(uid));
            Ok(srv.wrap_updates(vec![upd_id, upd_msg]).to_bytes())
        }
        tl_fns::phone::DeleteGroupCallMessages::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::DeleteGroupCallMessages::from_bytes(bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            srv.deleted_message_ids.push(req.messages.clone());
            let upd = tl_enums::Update::DeleteGroupCallMessages(
                tl_types::UpdateDeleteGroupCallMessages {
                    call: srv.input_call(),
                    messages: req.messages,
                },
            );
            srv.broadcast_update(upd.clone(), Some(uid));
            Ok(srv.wrap_updates(vec![upd]).to_bytes())
        }
        tl_fns::phone::GetGroupCallStars::CONSTRUCTOR_ID => {
            let top_donors = srv
                .stars_donors
                .iter()
                .map(|&(donor_uid, stars)| {
                    tl_enums::GroupCallDonor::GroupCallDonor(tl_types::GroupCallDonor {
                        top: true,
                        my: donor_uid == uid,
                        peer_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: donor_uid,
                        })),
                        stars,
                    })
                })
                .collect();
            let resp =
                tl_enums::phone::GroupCallStars::GroupCallStars(tl_types::phone::GroupCallStars {
                    total_stars: srv.stars_total,
                    top_donors,
                    chats: Vec::new(),
                    users: srv
                        .users
                        .values()
                        .cloned()
                        .map(tl_enums::User::User)
                        .collect(),
                });
            Ok(resp.to_bytes())
        }
        tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID => {
            let resp = tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
                call: srv.active_group_call_tl(false, true),
                participants: srv
                    .participants
                    .iter()
                    .map(|&(p_uid, p_ssrc)| srv.make_participant_tl(p_uid, p_ssrc, p_uid == uid))
                    .collect(),
                participants_next_offset: String::new(),
                chats: Vec::new(),
                users: srv
                    .users
                    .values()
                    .cloned()
                    .map(tl_enums::User::User)
                    .collect(),
            });
            Ok(resp.to_bytes())
        }
        tl_fns::phone::LeaveGroupCall::CONSTRUCTOR_ID
        | tl_fns::phone::EditGroupCallParticipant::CONSTRUCTOR_ID => {
            Ok(srv.wrap_updates(Vec::new()).to_bytes())
        }
        other => Err(InvocationError::Rpc(RpcError {
            code: 400,
            name: format!("UNHANDLED_CTOR_0x{other:08x}"),
            value: None,
        })),
    }
}

fn build_live_session(
    rpc: FakeConferenceRpc,
    self_user: &tl_types::User,
    extra_users: &[tl_types::User],
    extra_chats: &[tl_enums::Chat],
) -> LiveSession {
    let self_uid = DataUserId(self_user.id as u64);
    let rt = SessionRuntime::start(
        self_uid,
        tg_app::live::ErasedRpcClient::new(rpc),
        SessionRuntimeConfig::default(),
    )
    .expect("session runtime");
    let handle = rt.tokio_handle().clone();
    let session_arc = Arc::clone(rt.session());
    let mut all_users = vec![tl_enums::User::User(self_user.clone())];
    all_users.extend(extra_users.iter().cloned().map(tl_enums::User::User));
    let chats = extra_chats.to_vec();
    handle.block_on(async move {
        let mut s = session_arc.write().await;
        tg_api::convert::apply_tl_users_and_chats(&mut s, &all_users, &chats);
    });
    let mut live = LiveSession::new(Arc::new(rt), self_user.id as u64);
    let (mcfg, gcfg) = make_virtual_configs();
    live.set_calls_media_config(mcfg, gcfg);
    live
}

fn pump_until(
    sessions: &mut [&mut LiveSession],
    start_ms: &mut u64,
    mut pred: impl FnMut(&[&mut LiveSession]) -> bool,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        *start_ms += 25;
        for s in sessions.iter_mut() {
            CallsBackend::tick(*s, *start_ms);
        }
        if pred(sessions) {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    panic!("timed out waiting for conference/calls condition");
}

#[test]
fn conference_create_join_chain_update_and_emoji_verification() {
    let alice_user = make_tl_user(101, "Alpha", "alpha_e2e", true);
    let bob_user = make_tl_user(202, "Beta", "beta_e2e", true);
    let server = FakeConferenceServer::new(777_001, 999_001, "conf-slug-42");

    let rpc_alice = FakeConferenceRpc::register(&server, alice_user.clone());
    let rpc_bob = FakeConferenceRpc::register(&server, bob_user.clone());

    let mut alice =
        build_live_session(rpc_alice, &alice_user, std::slice::from_ref(&bob_user), &[]);
    let mut bob = build_live_session(rpc_bob, &bob_user, std::slice::from_ref(&alice_user), &[]);

    let alice_kp = E2eKeyPair::from_seed([0x11; 32]);
    let bob_kp = E2eKeyPair::from_seed([0x22; 32]);
    let mut now_ms = 1_000_u64;

    // 1. Alice creates the E2E conference (`phone.createConferenceCall` with zero block + public key).
    alice.create_conference_call_with_key_pair(alice_kp, now_ms);
    assert!(
        alice.group_call_e2e_frame_encryption_enabled(),
        "GroupMediaEngine must enable E2E frame encryption for conferences"
    );
    assert!(alice.group_call_e2e_encrypt_decrypt().is_some());

    pump_until(&mut [&mut alice], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1)
    });

    {
        let srv = server.lock().expect("lock server");
        assert_eq!(srv.created_calls.len(), 1);
        let created = &srv.created_calls[0];
        assert!(created.join);
        assert!(created.public_key.is_some());
        let blk0 = ChainBlock::from_tl_bytes(created.block.as_ref().expect("zero block"))
            .expect("valid zero block");
        assert_eq!(blk0.height, 0);
    }

    // 2. Bob joins by slug (`getLastChainBlock` -> self-add block 1 -> `phone.joinGroupCall`).
    bob.join_conference_call_by_slug_with_key_pair("conf-slug-42".to_owned(), bob_kp, now_ms);
    assert!(bob.group_call_e2e_frame_encryption_enabled());

    // Pump both Alice and Bob until:
    // - Bob fetches block 0, submits block 1, joins;
    // - Alice receives block 1 via `UpdateGroupCallChainBlocks`;
    // - Both exchange commit & reveal broadcasts via `phone.sendConferenceCallBroadcast`;
    // - Both converge on the exact same 4-emoji verification fingerprint!
    pump_until(&mut [&mut alice, &mut bob], &mut now_ms, |ss| {
        let a = ss[0].active_group_call();
        let b = ss[1].active_group_call();
        matches!((a, b), (Some(ag), Some(bg))
            if ag.e2e_verified_count == 2
                && bg.e2e_verified_count == 2
                && ag.emoji_fingerprint.is_some()
                && bg.emoji_fingerprint.is_some())
    });

    let alice_snap = alice.active_group_call().expect("alice snapshot").clone();
    let bob_snap = bob.active_group_call().expect("bob snapshot").clone();
    assert_eq!(alice_snap.emoji_fingerprint, bob_snap.emoji_fingerprint);
    assert_eq!(alice_snap.e2e_verified_count, 2);
    assert_eq!(bob_snap.e2e_verified_count, 2);

    {
        let srv = server.lock().expect("lock server");
        assert!(
            srv.chain_block_queries
                .iter()
                .any(|(_, sc, off, lim)| *sc == SUBCHAIN_STATE && *off == -1 && *lim == 1),
            "expected getLastChainBlock(subchain=0, offset=-1, limit=1)"
        );
        assert_eq!(srv.joined_calls.len(), 1);
        let join_req = &srv.joined_calls[0];
        assert!(join_req.public_key.is_some());
        let blk1 = ChainBlock::from_tl_bytes(join_req.block.as_ref().expect("join block"))
            .expect("valid block 1");
        assert_eq!(blk1.height, 1);
        assert!(
            srv.broadcasts_sent.len() >= 4,
            "expected commit+reveal broadcasts from both participants"
        );
    }

    // 3. Alice removes Bob via `phone.deleteConferenceCallParticipants` with a valid remove block.
    CallsBackend::remove_conference_participants(&mut alice, &[202], true, now_ms);
    pump_until(&mut [&mut alice], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| g.e2e_verified_count == 1)
    });
    {
        let srv = server.lock().expect("lock server");
        assert_eq!(srv.deleted_participants.len(), 1);
        let del = &srv.deleted_participants[0];
        assert!(del.kick && !del.only_left);
        assert_eq!(del.ids, vec![202]);
        let blk2 = ChainBlock::from_tl_bytes(&del.block).expect("valid remove block");
        assert_eq!(blk2.height, 2);
    }

    CallsBackend::leave_group_call(&mut alice, false, now_ms);
    CallsBackend::leave_group_call(&mut bob, false, now_ms);
}

#[test]
fn conference_encrypted_in_call_message_send_and_receive() {
    let alice_user = make_tl_user(111, "CipherA", "cipher_a", true);
    let bob_user = make_tl_user(222, "CipherB", "cipher_b", true);
    let server = FakeConferenceServer::new(777_002, 999_002, "conf-msg-slug");

    let rpc_alice = FakeConferenceRpc::register(&server, alice_user.clone());
    let rpc_bob = FakeConferenceRpc::register(&server, bob_user.clone());

    let mut alice =
        build_live_session(rpc_alice, &alice_user, std::slice::from_ref(&bob_user), &[]);
    let mut bob = build_live_session(rpc_bob, &bob_user, std::slice::from_ref(&alice_user), &[]);

    let mut now_ms = 5_000_u64;
    alice.create_conference_call_with_key_pair(E2eKeyPair::from_seed([0x33; 32]), now_ms);
    pump_until(&mut [&mut alice], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| g.e2e_verified_count == 1)
    });

    bob.join_conference_call_by_slug_with_key_pair(
        "conf-msg-slug".to_owned(),
        E2eKeyPair::from_seed([0x44; 32]),
        now_ms,
    );
    pump_until(&mut [&mut alice, &mut bob], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| g.e2e_verified_count == 2)
            && ss[1]
                .active_group_call()
                .is_some_and(|g| g.e2e_verified_count == 2)
    });

    // Alice sends an E2E encrypted in-call message; server never sees plaintext, Bob decrypts it.
    let secret_text = "Confidential conference note #42";
    CallsBackend::send_group_call_message(&mut alice, secret_text.to_owned(), 0, now_ms);

    pump_until(&mut [&mut alice, &mut bob], &mut now_ms, |ss| {
        let a_ok = ss[0].active_group_call().is_some_and(|g| {
            g.chat_messages
                .iter()
                .any(|m| m.text == secret_text && m.id > 0)
        });
        let b_ok = ss[1].active_group_call().is_some_and(|g| {
            g.chat_messages
                .iter()
                .any(|m| m.text == secret_text && !m.mine)
        });
        a_ok && b_ok
    });

    {
        let srv = server.lock().expect("lock server");
        assert_eq!(srv.encrypted_messages_sent.len(), 1);
        let (sender, ciphertext) = &srv.encrypted_messages_sent[0];
        assert_eq!(*sender, 111);
        assert!(
            !ciphertext
                .windows(secret_text.len())
                .any(|w| w == secret_text.as_bytes()),
            "wire payload must be E2E encrypted, not plaintext"
        );
    }

    // Verify SSRC <-> user E2E frame encryption roundtrip using the shared key.
    let alice_e2e = alice
        .group_call_e2e_encrypt_decrypt()
        .expect("alice encrypt_decrypt");
    let bob_e2e = bob
        .group_call_e2e_encrypt_decrypt()
        .expect("bob encrypt_decrypt");
    let frame = b"\xAAmedia-payload-opus";
    let encrypted_frame = alice_e2e
        .encrypt_frame(0, frame, 1)
        .expect("encrypt media frame");
    let decrypted_frame = bob_e2e
        .decrypt_frame(CallsUserId(111), 0, &encrypted_frame)
        .expect("decrypt media frame");
    assert_eq!(decrypted_frame, frame);

    CallsBackend::leave_group_call(&mut alice, false, now_ms);
    CallsBackend::leave_group_call(&mut bob, false, now_ms);
}

#[test]
fn group_call_plain_messages_and_paid_star_reaction_batch() {
    let host_user = make_tl_user(301, "Host", "stream_host", true);
    let viewer_user = make_tl_user(302, "Viewer", "stream_viewer", true);
    let channel_tl = make_tl_channel(9001, "Broadcast Lab");
    let group_peer = PeerId::from_channel(9001);

    let server = FakeConferenceServer::new(888_001, 444_001, "stream-slug");
    let rpc_host = FakeConferenceRpc::register(&server, host_user.clone());
    let mut host = build_live_session(
        rpc_host,
        &host_user,
        std::slice::from_ref(&viewer_user),
        &[channel_tl],
    );

    let mut now_ms = 10_000_u64;
    host.join_group_call_with_id(
        group_peer,
        PeerId::from_user(301),
        CallId(888_001),
        444_001,
        true,
        now_ms,
    );

    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| matches!(g.state, GroupCallState::Connecting | GroupCallState::Joined))
    });

    // 1. Send a plain in-call message and confirm `phone.sendGroupCallMessage` + ID promotion.
    CallsBackend::send_group_call_message(&mut host, "Hello live stream!".to_owned(), 0, now_ms);
    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        ss[0].active_group_call().is_some_and(|g| {
            g.chat_messages
                .iter()
                .any(|m| m.text == "Hello live stream!" && m.id >= 500)
        })
    });

    // 2. Receive an incoming plain message from another viewer via `UpdateGroupCallMessage`.
    {
        let srv = server.lock().expect("lock server");
        srv.push_update_to(
            301,
            tl_enums::Update::GroupCallMessage(tl_types::UpdateGroupCallMessage {
                call: srv.input_call(),
                message: tl_enums::GroupCallMessage::GroupCallMessage(tl_types::GroupCallMessage {
                    from_admin: false,
                    id: 650,
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 302 }),
                    date: now_unix_i32(),
                    message: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Great audio quality!".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    paid_message_stars: Some(25),
                }),
            }),
        );
    }
    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        ss[0].active_group_call().is_some_and(|g| {
            g.chat_messages
                .iter()
                .any(|m| m.id == 650 && m.text == "Great audio quality!" && m.stars == 25)
        })
    });

    // 3. Batch multiple paid Star reactions locally, verify no RPC sent until flushed, then flush.
    let before_count = server
        .lock()
        .expect("lock server")
        .plain_messages_sent
        .len();
    CallsBackend::add_group_paid_stars_reaction(&mut host, 10);
    CallsBackend::add_group_paid_stars_reaction(&mut host, 50);
    {
        let snap = host.active_group_call().expect("snapshot");
        assert_eq!(snap.stars_scheduled, 60);
        assert_eq!(snap.stars_total, 25 + 60);
        assert_eq!(
            server
                .lock()
                .expect("lock server")
                .plain_messages_sent
                .len(),
            before_count,
            "batched Stars must not fire an RPC before flush"
        );
    }

    // Flush the 60-Star batch in one `phone.sendGroupCallMessage(allow_paid_stars = Some(60))`.
    CallsBackend::flush_group_paid_stars_reaction(&mut host, now_ms);
    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        ss[0]
            .active_group_call()
            .is_some_and(|g| g.stars_scheduled == 0)
            && server
                .lock()
                .expect("lock server")
                .plain_messages_sent
                .len()
                == before_count + 1
    });

    {
        let srv = server.lock().expect("lock server");
        let last = srv.plain_messages_sent.last().expect("paid reaction rpc");
        assert_eq!(last.sender_uid, 301);
        assert_ne!(last.random_id, 0);
        assert!(last.text.is_empty());
        assert_eq!(last.allow_paid_stars, Some(60));
    }

    // 4. Queue and cancel a batch without sending.
    CallsBackend::add_group_paid_stars_reaction(&mut host, 15);
    assert_eq!(host.active_group_call().expect("snap").stars_scheduled, 15);
    CallsBackend::cancel_group_paid_stars_reaction(&mut host);
    assert_eq!(host.active_group_call().expect("snap").stars_scheduled, 0);

    // 5. Refresh paid Stars leaderboard via `phone.getGroupCallStars`.
    CallsBackend::refresh_group_stars(&mut host);
    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        ss[0].active_group_call().is_some_and(|g| {
            g.stars_total == 60 && g.stars_top.iter().any(|d| d.is_my && d.stars == 60)
        })
    });

    // 6. Delete an in-call message via `phone.deleteGroupCallMessages`.
    CallsBackend::delete_group_call_message(&mut host, 650, None, false);
    pump_until(&mut [&mut host], &mut now_ms, |ss| {
        !ss[0]
            .active_group_call()
            .is_some_and(|g| g.chat_messages.iter().any(|m| m.id == 650))
            && server.lock().expect("lock server").deleted_message_ids == vec![vec![650]]
    });

    CallsBackend::leave_group_call(&mut host, false, now_ms);
}

#[test]
fn screen_share_sources_and_deep_link_join_conference_and_panel_render() {
    let user = make_tl_user(401, "Presenter", "presenter_lab", true);
    let server = FakeConferenceServer::new(999_001, 555_001, "deeplink-slug-7");
    let rpc = FakeConferenceRpc::register(&server, user.clone());
    let mut live = build_live_session(rpc, &user, &[], &[]);

    // 1. LiveSession overrides `CallsBackend::screen_share_sources` with real enumeration.
    let sources = CallsBackend::screen_share_sources(&live);
    assert!(
        sources.iter().any(|s| s.kind == ScreenSourceKind::Display),
        "screen_share_sources must enumerate at least one display"
    );

    // 2. `AppCallsHost::handle_join_call_deep_link` with `slug` joins the conference via
    //    `getLastChainBlock(InputGroupCallSlug)` -> `phone.joinGroupCall`.
    let mut host = AppCallsHost::with_backend(Box::new(live.clone()), false);
    host.handle_join_call_deep_link(None, Some("deeplink-slug-7".to_owned()));

    let mut now_ms = 20_000_u64;
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        now_ms += 25;
        host.backend_mut().tick(now_ms);
        if host
            .backend()
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    let snap = host
        .backend()
        .active_group_call()
        .expect("conference joined via deep link")
        .clone();
    assert!(snap.is_conference);
    assert_eq!(snap.e2e_verified_count, 1);

    {
        let srv = server.lock().expect("lock server");
        assert!(srv.chain_block_queries.iter().any(|(call, sc, off, lim)| {
            matches!(call, tl_enums::InputGroupCall::Slug(s) if s.slug == "deeplink-slug-7")
                && *sc == SUBCHAIN_STATE
                && *off == -1
                && *lim == 1
        }));
    }

    // 3. Smoke-render GroupCallPanelState with the E2E banner & Chat tab active in egui.
    let ctx = egui::Context::default();
    let palette = CallPalette::tdesktop();
    let mut panel = GroupCallPanelState {
        show_messages_tab: true,
        ..GroupCallPanelState::default()
    };
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(420.0, 560.0));
        panel.paint_panel_contents(ui, rect, &snap, host.backend_mut(), &palette, now_ms);
    });
    out.textures_delta.clear();

    // Verify helper conversion symbol remains exercised.
    let _ = from_calls_peer_id(tg_calls::types::PeerId::from_user(CallsUserId(401)));
    host.backend_mut().leave_group_call(false, now_ms);
    live.tick(now_ms + 50);
}

#[test]
fn multi_account_join_call_deep_link_isolated_per_account() {
    use tg_app::live::multi_account::MultiAccountWorkspaceHost;
    use tg_app::notifications::manager::NotificationSystem;
    use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
    use tg_app::stand_in::settings::ShellSettings;
    use tg_app::startup::ipc::IncomingActivation;
    use tg_app::tray::MockTrayPlatform;

    let acct0_user = make_tl_user(501, "AccountZero", "acct_zero", true);
    let acct1_user = make_tl_user(502, "AccountOne", "acct_one", true);

    let srv0 = FakeConferenceServer::new(910_001, 510_001, "conf-acct-0");
    let srv1 = FakeConferenceServer::new(910_002, 510_002, "conf-acct-1");
    let rpc0 = FakeConferenceRpc::register(&srv0, acct0_user.clone());
    let rpc1 = FakeConferenceRpc::register(&srv1, acct1_user.clone());

    let rt0 = Arc::new(
        SessionRuntime::start(
            DataUserId(acct0_user.id as u64),
            tg_app::live::ErasedRpcClient::new(rpc0),
            SessionRuntimeConfig::default(),
        )
        .expect("start rt0"),
    );
    let rt1 = Arc::new(
        SessionRuntime::start(
            DataUserId(acct1_user.id as u64),
            tg_app::live::ErasedRpcClient::new(rpc1),
            SessionRuntimeConfig::default(),
        )
        .expect("start rt1"),
    );

    let seed_self = |rt: &Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
                     user: &tl_types::User| {
        let session_arc = Arc::clone(rt.session());
        let u = tl_enums::User::User(user.clone());
        rt.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;
            tg_api::convert::apply_tl_users_and_chats(&mut s, &[u], &[]);
        });
    };
    seed_self(&rt0, &acct0_user);
    seed_self(&rt1, &acct1_user);

    let settings_dir = tempfile::tempdir().expect("tempdir for settings");
    let mut multi = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_dir.path().join("multi_calls_e2e.json"),
        NotificationSystem::new(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
        ),
        Box::new(MockTrayPlatform::default()),
    );
    multi.add_account(
        0,
        DataUserId(acct0_user.id as u64),
        false,
        false,
        Arc::clone(&rt0),
        None,
        true,
        false,
    );
    multi.add_account(
        1,
        DataUserId(acct1_user.id as u64),
        false,
        false,
        Arc::clone(&rt1),
        None,
        false,
        false,
    );

    let (mcfg, gcfg) = make_virtual_configs();
    for slot in [0, 1] {
        multi
            .account_mut(slot)
            .expect("account slot")
            .workspace
            .live_session_mut()
            .set_calls_media_config(mcfg.clone(), gcfg.clone());
    }

    let ctx = egui::Context::default();
    let run_frame = |host: &mut MultiAccountWorkspaceHost| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = host.show(&ctx, ui);
        });
        out.textures_delta.clear();
    };

    // 1. Account 0 receives `tg://call?slug=conf-acct-0` deep link.
    multi
        .active_workspace_mut()
        .expect("slot 0 active")
        .controller_mut()
        .handle_incoming_activation(
            IncomingActivation {
                urls: vec!["tg://call?slug=conf-acct-0".to_owned()],
                ..IncomingActivation::default()
            },
            &ctx,
        );

    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        run_frame(&mut multi);
        if multi
            .account(0)
            .expect("slot 0")
            .workspace
            .calls_host()
            .backend()
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    assert!(
        multi
            .account(0)
            .expect("slot 0")
            .workspace
            .calls_host()
            .backend()
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1),
        "account 0 must join conference via tg://call?slug=conf-acct-0"
    );
    assert!(
        multi
            .account(1)
            .expect("slot 1")
            .workspace
            .calls_host()
            .backend()
            .active_group_call()
            .is_none(),
        "account 1 must remain idle when account 0 handles JoinCall"
    );

    // 2. Switch to Account 1 and receive `https://t.me/call/conf-acct-1`.
    assert!(multi.switch_to(1));
    multi
        .active_workspace_mut()
        .expect("slot 1 active")
        .controller_mut()
        .handle_incoming_activation(
            IncomingActivation {
                urls: vec!["https://t.me/call/conf-acct-1".to_owned()],
                ..IncomingActivation::default()
            },
            &ctx,
        );

    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        run_frame(&mut multi);
        if multi
            .account(1)
            .expect("slot 1")
            .workspace
            .calls_host()
            .backend()
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    assert!(
        multi
            .account(1)
            .expect("slot 1")
            .workspace
            .calls_host()
            .backend()
            .active_group_call()
            .is_some_and(|g| g.is_conference && g.e2e_verified_count == 1),
        "account 1 must join its own conference via https://t.me/call/conf-acct-1"
    );
    assert!(
        srv0.lock()
            .expect("lock srv0")
            .chain_block_queries
            .iter()
            .any(|(c, _, _, _)| matches!(c, tl_enums::InputGroupCall::Slug(s) if s.slug == "conf-acct-0")),
        "account 0 server must receive conf-acct-0 slug query"
    );
    assert!(
        srv1.lock()
            .expect("lock srv1")
            .chain_block_queries
            .iter()
            .any(|(c, _, _, _)| matches!(c, tl_enums::InputGroupCall::Slug(s) if s.slug == "conf-acct-1")),
        "account 1 server must receive conf-acct-1 slug query"
    );
    assert!(
        srv0.lock()
            .expect("lock srv0")
            .chain_block_queries
            .iter()
            .all(|(c, _, _, _)| !matches!(c, tl_enums::InputGroupCall::Slug(s) if s.slug == "conf-acct-1")),
        "account 0 server must not receive account 1's JoinCall slug"
    );

    multi.on_exit();
}
