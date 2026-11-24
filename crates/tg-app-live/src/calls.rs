//! Live `CallsBackend` implementation for [`LiveSession`] connecting `tg-calls` state
//! machines (`CallsInstance`, `Call`, `GroupCallSession`) to Telegram MTProto RPCs via
//! `tg-session` and real `tg-voip` media engines (`MediaEngine`, `GroupMediaEngine`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/calls/calls_instance.cpp`
//! - `Telegram/SourceFiles/calls/calls_call.cpp`
//! - `Telegram/SourceFiles/calls/group/calls_group_call.cpp`

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_calls::call::{CallAction, CallTimerKind, ControllerState};
use tg_calls::group::{MAX_INVITE_PER_SLICE, MAX_VOLUME, MIN_VOLUME, extract_conference_slug};
use tg_calls::instance::{CallsInstance, InstanceAction};
use tg_calls::protocol::{CallProtocolConfig, ServerCallConfig};
use tg_calls::tl_conv::PhoneGroupCallResponse;
use tg_calls::types::{
    CallId, DhConfigResponse, GroupCallInfo, GroupCallObject, GroupCallParticipantData,
    InputGroupCall, PhoneCallUpdate, PhoneGroupCallStarsResponse, UpdateDeleteGroupCallMessages,
    UpdateGroupCallChainBlocks, UpdateGroupCallConnection, UpdateGroupCallEncryptedMessage,
    UpdateGroupCallMessage, UpdateGroupCallParticipants, UpdatePhoneCallSignalingData,
    UserId as CallsUserId,
};
use tg_calls::{
    ApplySliceSource, CallType, ConferenceEncryptDecrypt, DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
    E2eAction, E2eCall, E2eKeyPair, E2eTimerKind, GroupCallAction, GroupCallSession,
    GroupCallSound, GroupCallTimerKind, GroupMessageAction, InstanceMode, MessageDeleteRequest,
    MuteState, ParsedJoinResponse, ParticipantUpdate, PublicKey, RemoteBatteryState,
    SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST, STARS_STATS_SHORT_POLL_DELAY_MS, SUBCHAIN_STATE,
    VersionedGroupCallUpdate, VideoEndpoint, VideoQuality, VideoState,
    build_create_conference_call, build_delete_conference_participants,
    build_delete_group_call_messages, build_delete_group_call_participant_messages,
    build_get_chain_blocks, build_get_group_call_stars, build_get_last_chain_block,
    build_join_conference_call, build_send_conference_broadcast,
    build_send_group_call_encrypted_message, build_send_group_call_message, is_chain_invalid_error,
    is_permanent_broadcast_error, parse_join_response,
};
use tg_data::{Session, UserId as DataUserId};
use tg_session::SessionRuntime;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tg_voip::{
    BroadcastChunkLoader, BroadcastChunkRequest, BroadcastChunkResponse, BroadcastChunkStatus,
    CallMediaEngine, CameraCaptureConfig, CameraDeviceInfo, GroupMediaConfig, GroupMediaEngine,
    GroupMediaEvent, MediaEngine, MediaEngineConfig, MediaEngineEvent, RgbaVideoFrame,
    ScreenCaptureConfig, VirtualAudioHandle, VoipStatsSnapshot, list_cameras,
};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::bridge::{LiveBridgeState, LiveRuntimeBridge};
use crate::convert::{now_unix_i32, peer_to_call_info, to_data_peer, unix_to_local_datetime};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::calls::backend::{
    ActiveCallSnapshot, AudioDeviceOption, BackendSoundEvent, CallAudioSettings, CallPeerInfo,
    CallsBackend, GroupCallMessageView, GroupCallSnapshot, GroupMemberView, GroupStarsDonorView,
    JoinAsCandidate, RecentCallDirection, RecentCallEntry, RtmpStreamInfo, from_calls_peer_id,
    to_calls_peer_id,
};
use tg_app_core::core_types::misc::default_stars_colorings;
use tg_app_core::core_types::screen_picker::{ScreenShareSource, enumerate_screen_sources};
use tg_app_core::stand_in::types::{PeerId, PeerKind};

/// Maximum chunk size for `upload.getFile(inputGroupCallStream)` (`calls_group_call.cpp:3221`).
const STREAM_CHUNK_LIMIT_BYTES: i32 = 128 * 1024;

/// Default participant limit when loading `phone.getGroupCall`.
const GROUP_CALL_PARTICIPANTS_LIMIT: i32 = 100;

/// Timeout for asynchronous call RPCs so stalled connections fail cleanly.
const CALL_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for synchronous `join_as_candidates` lookups (`phone.getGroupCallJoinAs`).
const JOIN_AS_RPC_TIMEOUT: Duration = Duration::from_secs(5);

// =============================================================================
// Broadcast Stream Chunk Loader (`upload.getFile` + `inputGroupCallStream`)
// =============================================================================

/// [`BroadcastChunkLoader`] implementation pulling broadcast / RTMP livestream chunks from
/// Telegram via `upload.getFile(inputGroupCallStream)` on `stream_dc_id`.
///
/// Port of `GroupCall::LoadPartTask`, `GroupCall::broadcastPartStart`, and
/// `GroupCall::requestCurrentTimeStart` in `calls/group/calls_group_call.cpp`.
pub struct TelegramBroadcastChunkLoader<C: RpcClient + Clone + 'static = ErasedRpcClient> {
    rpc: C,
    call: Arc<Mutex<tl_enums::InputGroupCall>>,
    stream_dc_id: Arc<AtomicI32>,
}

impl<C: RpcClient + Clone + 'static> TelegramBroadcastChunkLoader<C> {
    /// Creates a new broadcast chunk loader bound to `call` and an optional `stream_dc_id`.
    #[must_use]
    pub fn new(rpc: C, call: tl_enums::InputGroupCall, stream_dc_id: Option<i32>) -> Self {
        Self {
            rpc,
            call: Arc::new(Mutex::new(call)),
            stream_dc_id: Arc::new(AtomicI32::new(stream_dc_id.unwrap_or(0))),
        }
    }

    /// Updates the `InputGroupCall` handle (e.g. after `phone.createGroupCall` assigns an ID).
    pub fn set_call(&self, call: tl_enums::InputGroupCall) {
        if let Ok(mut guard) = self.call.lock() {
            *guard = call;
        }
    }

    /// Updates the target stream DC ID (`GroupCall.stream_dc_id`).
    pub fn set_stream_dc_id(&self, dc_id: i32) {
        if dc_id > 0 {
            self.stream_dc_id.store(dc_id, Ordering::Release);
        }
    }

    /// Returns the currently configured stream DC ID (falling back to `rpc.main_dc_id()`).
    #[must_use]
    pub fn effective_dc_id(&self) -> DcId {
        let dc = self.stream_dc_id.load(Ordering::Acquire);
        if dc > 0 { dc } else { self.rpc.main_dc_id() }
    }

    fn current_call(&self) -> tl_enums::InputGroupCall {
        self.call.lock().map_or_else(
            |_| {
                tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                    id: 0,
                    access_hash: 0,
                })
            },
            |g| g.clone(),
        )
    }
}

impl<C: RpcClient + Clone + 'static> BroadcastChunkLoader for TelegramBroadcastChunkLoader<C> {
    fn fetch_chunk(
        &self,
        req: BroadcastChunkRequest,
    ) -> Pin<Box<dyn Future<Output = BroadcastChunkResponse> + Send + '_>> {
        let rpc = self.rpc.clone();
        let call = self.current_call();
        let dc_id = self.effective_dc_id();

        Box::pin(async move {
            let (video_channel, video_quality) = if req.video_channel != 0 {
                (
                    Some(req.video_channel),
                    Some(video_quality_to_tl(req.video_quality)),
                )
            } else {
                (None, None)
            };

            let tl_req = tl_fns::upload::GetFile {
                precise: false,
                cdn_supported: false,
                location: tl_enums::InputFileLocation::InputGroupCallStream(
                    tl_types::InputGroupCallStream {
                        call,
                        time_ms: req.time_ms,
                        scale: req.scale.tl_scale(),
                        video_channel,
                        video_quality,
                    },
                ),
                offset: 0,
                limit: STREAM_CHUNK_LIMIT_BYTES,
            };

            let resp_ts_sec = now_unix_ms() as f64 / 1000.0;
            match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke_in_dc(dc_id, &tl_req)).await {
                Ok(Ok(tl_enums::upload::File::File(file))) => BroadcastChunkResponse {
                    timestamp_ms: req.time_ms,
                    response_timestamp_sec: resp_ts_sec,
                    status: BroadcastChunkStatus::Success,
                    data: file.bytes,
                },
                Ok(Ok(tl_enums::upload::File::CdnRedirect(_))) => {
                    debug!("broadcast stream upload.getFile returned fileCdnRedirect");
                    BroadcastChunkResponse {
                        timestamp_ms: req.time_ms,
                        response_timestamp_sec: resp_ts_sec,
                        status: BroadcastChunkStatus::ResyncNeeded,
                        data: Vec::new(),
                    }
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err.into();
                    let name = invocation_error_name(&inv);
                    let status = if name == "TIME_TOO_BIG" || name.starts_with("FLOOD_WAIT") {
                        BroadcastChunkStatus::NotReady
                    } else {
                        BroadcastChunkStatus::ResyncNeeded
                    };
                    debug!(error = %inv, ?status, "broadcast stream chunk fetch failed");
                    BroadcastChunkResponse {
                        timestamp_ms: req.time_ms,
                        response_timestamp_sec: resp_ts_sec,
                        status,
                        data: Vec::new(),
                    }
                }
                Err(_) => BroadcastChunkResponse {
                    timestamp_ms: req.time_ms,
                    response_timestamp_sec: resp_ts_sec,
                    status: BroadcastChunkStatus::NotReady,
                    data: Vec::new(),
                },
            }
        })
    }

    fn request_current_time_ms(&self) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + '_>> {
        let rpc = self.rpc.clone();
        let call = self.current_call();
        let dc_id = self.effective_dc_id();

        Box::pin(async move {
            let req = tl_fns::phone::GetGroupCallStreamChannels { call };
            match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke_in_dc(dc_id, &req)).await {
                Ok(Ok(tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(resp))) => {
                    let ts = resp.channels.first().map(|ch| {
                        let tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(inner) = ch;
                        inner.last_timestamp_ms
                    });
                    ts.filter(|&v| v > 0).or_else(|| Some(now_unix_ms()))
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err.into();
                    debug!(error = %inv, "phone.getGroupCallStreamChannels failed; falling back to local clock");
                    Some(now_unix_ms())
                }
                Err(_) => Some(now_unix_ms()),
            }
        })
    }
}

const fn video_quality_to_tl(quality: VideoQuality) -> i32 {
    match quality {
        VideoQuality::Thumbnail => 0,
        VideoQuality::Medium => 1,
        VideoQuality::Full => 2,
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64)
}

fn invocation_error_name(err: &InvocationError) -> String {
    match err {
        InvocationError::Rpc(rpc) => rpc.name.clone(),
        other => other.to_string(),
    }
}

// =============================================================================
// Internal Inbound Events & Group Call State
// =============================================================================

#[derive(Debug, Clone, Copy)]
struct KnownGroupCall {
    call_id: CallId,
    access_hash: i64,
    can_manage: bool,
    stream_dc_id: Option<i32>,
}

enum LiveCallInboundEvent {
    CallConfigResult {
        ok: bool,
    },
    DhConfigResult(Result<DhConfigResponse, String>),
    RequestCallResult(Result<PhoneCallUpdate, String>),
    ReceivedCallResult(Result<(), String>),
    AcceptCallResult(Result<PhoneCallUpdate, String>),
    ConfirmCallResult(Result<PhoneCallUpdate, String>),
    SendSignalingResult(Result<bool, String>),
    DiscardCallResult {
        updates: Option<tl_enums::Updates>,
    },
    PhoneCallUpdate(tl_enums::PhoneCall),
    PhoneCallSignalingData(tl_types::UpdatePhoneCallSignalingData),
    GroupCallUpdate {
        peer: Option<tl_enums::Peer>,
        call: tl_enums::GroupCall,
    },
    GroupCallParticipantsUpdate(tl_types::UpdateGroupCallParticipants),
    GroupCallConnectionUpdate(tl_types::UpdateGroupCallConnection),
    GroupCallChainBlocksUpdate(tl_types::UpdateGroupCallChainBlocks),
    GroupCallMessageUpdate(tl_types::UpdateGroupCallMessage),
    GroupCallEncryptedMessageUpdate(tl_types::UpdateGroupCallEncryptedMessage),
    DeleteGroupCallMessagesUpdate(tl_types::UpdateDeleteGroupCallMessages),
    GroupLookupResolved {
        group_peer_id: PeerId,
        join_as: PeerId,
        found_call: Option<(CallId, i64)>,
        can_manage: bool,
    },
    GroupCreateResult {
        result: Result<tl_enums::Updates, String>,
    },
    GroupConferenceCreateResult {
        result: Result<tl_enums::Updates, String>,
        was_mute_state: MuteState,
        was_video_stopped: bool,
    },
    GroupLastChainBlockLoaded {
        result: Result<tl_enums::Updates, String>,
    },
    GroupChainBlocksResponse {
        subchain: i32,
        result: Result<tl_enums::Updates, String>,
    },
    GroupBroadcastSent {
        block: Vec<u8>,
        result: Result<tl_enums::Updates, String>,
    },
    GroupMessageSent {
        random_id: u64,
        paid_reaction: bool,
        result: Result<tl_enums::Updates, String>,
    },
    GroupEncryptedMessageSent {
        random_id: u64,
        result: Result<bool, String>,
    },
    GroupStarsLoaded(PhoneGroupCallStarsResponse),
    GroupFullCallLoaded {
        call_id: CallId,
        response: PhoneGroupCallResponse,
    },
    GroupJoinResult {
        result: Result<tl_enums::Updates, String>,
        was_mute_state: MuteState,
        was_video_stopped: bool,
    },
    GroupLeaveDone {
        updates: Option<tl_enums::Updates>,
    },
    GroupDiscardDone {
        updates: Option<tl_enums::Updates>,
    },
    RecentCallsLoaded(Vec<RecentCallEntry>),
    GroupCheckResult(Result<Vec<u32>, String>),
    GroupSelfUpdateDone {
        updates: Option<tl_enums::Updates>,
        error: Option<String>,
    },
    GroupScreenJoinDone {
        updates: Option<tl_enums::Updates>,
    },
    GroupScreenLeaveDone {
        updates: Option<tl_enums::Updates>,
    },
    GroupResolvedSsrcs {
        participants: Vec<GroupCallParticipantData>,
        resolved_ssrcs: Vec<u32>,
    },
    GroupParticipantsPage {
        participants: Vec<GroupCallParticipantData>,
    },
    GroupUpdatesOnly(tl_enums::Updates),
    Toast(String),
}

#[derive(Debug, Clone)]
struct PendingConferenceJoin {
    call: InputGroupCall,
    join_as: tg_calls::types::PeerId,
    muted: bool,
    video_stopped: bool,
    invite_hash: Option<String>,
    params_json: String,
    was_mute_state: MuteState,
}

struct LiveActiveGroup {
    session: GroupCallSession,
    engine: GroupMediaEngine,
    broadcast_loader: Arc<TelegramBroadcastChunkLoader<ErasedRpcClient>>,
    group_peer: CallPeerInfo,
    title: String,
    can_manage: bool,
    messages_enabled: bool,
    is_video_stream: bool,
    min_paid_stars: i64,
    participants_loaded: bool,
    pending_conference_create: bool,
    pending_conference_join: Option<PendingConferenceJoin>,
    pending_outbound_block: Option<Vec<u8>>,
    recording: bool,
    camera_active: bool,
    screen_share_active: bool,
    local_audio_level: f32,
    ssrc_levels: HashMap<u32, f32>,
    timers: HashMap<GroupCallTimerKind, (u64, Option<u64>)>,
    e2e_timers: HashMap<(i32, E2eTimerKind), u64>,
    last_messages_prune_ms: u64,
    stars_poll_due_ms: Option<u64>,
    last_stream_params_json: Option<String>,
    invite_link_speaker: String,
    invite_link_listener: String,
    snapshot: GroupCallSnapshot,
}

/// Shared live calls controller owned by [`LiveCallsHandle`] inside [`LiveSession`].
struct LiveCallsController {
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    bridge_state: Arc<LiveBridgeState>,
    inbound_tx: mpsc::UnboundedSender<LiveCallInboundEvent>,
    inbound_rx: mpsc::UnboundedReceiver<LiveCallInboundEvent>,
    updates_task: Option<JoinHandle<()>>,

    instance: CallsInstance,
    media_config: MediaEngineConfig,
    group_media_config: GroupMediaConfig,

    call_engine: Option<MediaEngine>,
    call_timers: HashMap<CallTimerKind, u64>,
    call_remote_audio_level: f32,
    call_screen_share_active: bool,
    call_submitted_rating: Option<u8>,
    call_latest_video_frame: Option<RgbaVideoFrame>,
    call_snapshot: Option<ActiveCallSnapshot>,

    active_group: Option<LiveActiveGroup>,
    known_group_calls: HashMap<PeerId, KnownGroupCall>,
    join_as_cache: HashMap<PeerId, Vec<JoinAsCandidate>>,

    audio_settings: CallAudioSettings,
    input_devices: Vec<AudioDeviceOption>,
    output_devices: Vec<AudioDeviceOption>,
    camera_devices: Vec<CameraDeviceInfo>,
    recent_calls: Vec<RecentCallEntry>,
    active_group_calls: Vec<CallPeerInfo>,

    sound_queue: Vec<BackendSoundEvent>,
    toast_queue: Vec<String>,
    next_random_id: i32,
    next_recent_msg_id: i32,
}

impl Drop for LiveCallsController {
    fn drop(&mut self) {
        if let Some(task) = self.updates_task.take() {
            task.abort();
        }
        if let Some(mut engine) = self.call_engine.take() {
            let _ = CallMediaEngine::stop(&mut engine);
        }
        if let Some(mut group) = self.active_group.take() {
            group.engine.stop();
        }
    }
}

impl LiveCallsController {
    fn new(runtime: Arc<SessionRuntime<ErasedRpcClient>>, bridge: &Arc<LiveRuntimeBridge>) -> Self {
        let self_uid = read_session_sync(&runtime, |s| s.self_peer_id().bare());
        let instance = CallsInstance::new(
            CallsUserId(self_uid),
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        let media_config = MediaEngineConfig::default();
        let group_media_config = GroupMediaConfig::default();

        let mut input_devices = vec![AudioDeviceOption {
            id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            name: "System Default Microphone".to_owned(),
        }];
        if let Ok(devs) = MediaEngine::list_audio_devices(tg_audio::DeviceType::Capture) {
            for d in devs {
                if !input_devices.iter().any(|x| x.id == d.id) {
                    input_devices.push(AudioDeviceOption {
                        id: d.id,
                        name: d.name,
                    });
                }
            }
        }

        let mut output_devices = vec![AudioDeviceOption {
            id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            name: "System Default Speakers".to_owned(),
        }];
        if let Ok(devs) = MediaEngine::list_audio_devices(tg_audio::DeviceType::Playback) {
            for d in devs {
                if !output_devices.iter().any(|x| x.id == d.id) {
                    output_devices.push(AudioDeviceOption {
                        id: d.id,
                        name: d.name,
                    });
                }
            }
        }

        let camera_devices = list_cameras().unwrap_or_default();
        let mut audio_settings = CallAudioSettings {
            output_device_id: media_config.output_device_id.clone(),
            input_device_id: media_config.input_device_id.clone(),
            noise_suppression: media_config.dsp.enable_ns,
            ..CallAudioSettings::default()
        };
        if let Some(first_cam) = camera_devices.first() {
            audio_settings.camera_device_id = first_cam.id.clone();
        }

        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let bridge_state = Arc::clone(bridge.state());
        let updates_task = spawn_updates_listener(&runtime, &bridge_state, inbound_tx.clone());

        Self {
            runtime,
            bridge_state,
            inbound_tx,
            inbound_rx,
            updates_task,
            instance,
            media_config,
            group_media_config,
            call_engine: None,
            call_timers: HashMap::new(),
            call_remote_audio_level: 0.0,
            call_screen_share_active: false,
            call_submitted_rating: None,
            call_latest_video_frame: None,
            call_snapshot: None,
            active_group: None,
            known_group_calls: HashMap::new(),
            join_as_cache: HashMap::new(),
            audio_settings,
            input_devices,
            output_devices,
            camera_devices,
            recent_calls: Vec::new(),
            active_group_calls: Vec::new(),
            sound_queue: Vec::new(),
            toast_queue: Vec::new(),
            next_random_id: 1001,
            next_recent_msg_id: 1,
        }
    }

    fn next_random_id(&mut self) -> i32 {
        self.next_random_id = self.next_random_id.wrapping_add(1);
        let now = now_unix_i32();
        (now ^ self.next_random_id).max(1)
    }

    fn next_random_u64(&mut self) -> u64 {
        let lo = self.next_random_id() as u32 as u64;
        let hi = (now_unix_ms() as u64) & 0xFFFF_FFFF;
        ((hi << 32) | lo).max(1)
    }

    fn peer_info(&self, peer_id: PeerId) -> CallPeerInfo {
        read_session_sync(&self.runtime, |s| {
            peer_to_call_info(s, to_data_peer(peer_id))
        })
    }

    fn drain_inbound_events(&mut self, now_ms: u64) {
        while let Ok(ev) = self.inbound_rx.try_recv() {
            self.handle_inbound_event(ev, now_ms);
        }
    }

    fn handle_inbound_event(&mut self, event: LiveCallInboundEvent, now_ms: u64) {
        match event {
            LiveCallInboundEvent::CallConfigResult { ok } => {
                self.instance.handle_call_config_response(ok, now_ms);
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::DhConfigResult(res) => match res {
                Ok(dh) => {
                    self.instance.handle_dh_config_response(Ok(dh));
                    self.process_instance_actions(now_ms);
                }
                Err(err) => {
                    self.toast_queue
                        .push(format!("Failed to fetch DH config for call: {err}"));
                    self.instance.handle_dh_config_response(Err(()));
                    self.process_instance_actions(now_ms);
                }
            },
            LiveCallInboundEvent::RequestCallResult(res) => {
                if let Some(call) = self.instance.current_call_mut() {
                    match res {
                        Ok(upd) => call.handle_request_call_result(Ok(upd), now_ms),
                        Err(err) => call.handle_request_call_result(Err(&err), now_ms),
                    }
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::ReceivedCallResult(res) => {
                if let Some(call) = self.instance.current_call_mut() {
                    match res {
                        Ok(()) => call.handle_received_call_result(Ok(()), now_ms),
                        Err(err) => call.handle_received_call_result(Err(&err), now_ms),
                    }
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::AcceptCallResult(res) => {
                if let Some(call) = self.instance.current_call_mut() {
                    match res {
                        Ok(upd) => call.handle_accept_call_result(Ok(upd), now_ms),
                        Err(err) => call.handle_accept_call_result(Err(&err), now_ms),
                    }
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::ConfirmCallResult(res) => {
                if let Some(call) = self.instance.current_call_mut() {
                    match res {
                        Ok(upd) => call.handle_confirm_call_result(Ok(upd), now_ms),
                        Err(err) => call.handle_confirm_call_result(Err(&err), now_ms),
                    }
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::SendSignalingResult(res) => {
                if let Some(call) = self.instance.current_call_mut() {
                    match res {
                        Ok(ok) => call.handle_send_signaling_data_result(Ok(ok), now_ms),
                        Err(err) => call.handle_send_signaling_data_result(Err(&err), now_ms),
                    }
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::DiscardCallResult { updates } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(call) = self.instance.current_call_mut() {
                    call.handle_discard_call_result(now_ms);
                }
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::PhoneCallUpdate(tl_call) => {
                self.handle_tl_phone_call(tl_call, now_ms);
            }
            LiveCallInboundEvent::PhoneCallSignalingData(tl_sig) => {
                let converted: UpdatePhoneCallSignalingData = tl_sig.into();
                self.instance.handle_signaling_data(converted);
                self.process_instance_actions(now_ms);
            }
            LiveCallInboundEvent::GroupCallUpdate { peer, call } => {
                self.handle_tl_group_call(peer, call, now_ms);
            }
            LiveCallInboundEvent::GroupCallParticipantsUpdate(upd) => {
                self.handle_tl_group_call_participants(upd, now_ms);
            }
            LiveCallInboundEvent::GroupCallConnectionUpdate(upd) => {
                self.handle_tl_group_call_connection(upd, now_ms);
            }
            LiveCallInboundEvent::GroupCallChainBlocksUpdate(upd) => {
                self.handle_tl_group_call_chain_blocks(upd, now_ms);
            }
            LiveCallInboundEvent::GroupCallMessageUpdate(upd) => {
                self.handle_tl_group_call_message(upd, now_ms);
            }
            LiveCallInboundEvent::GroupCallEncryptedMessageUpdate(upd) => {
                self.handle_tl_group_call_encrypted_message(upd, now_ms);
            }
            LiveCallInboundEvent::DeleteGroupCallMessagesUpdate(upd) => {
                self.handle_tl_delete_group_call_messages(upd, now_ms);
            }
            LiveCallInboundEvent::GroupLookupResolved {
                group_peer_id,
                join_as,
                found_call,
                can_manage,
            } => {
                if let Some((call_id, access_hash)) = found_call {
                    self.known_group_calls.insert(
                        group_peer_id,
                        KnownGroupCall {
                            call_id,
                            access_hash,
                            can_manage,
                            stream_dc_id: None,
                        },
                    );
                    self.refresh_active_group_peers();
                    if let Err(err) = self.start_group_call_session(
                        group_peer_id,
                        join_as,
                        call_id,
                        access_hash,
                        can_manage,
                        None,
                        now_ms,
                    ) {
                        self.toast_queue
                            .push(format!("Failed to start group call engine: {err}"));
                    }
                } else if let Err(err) =
                    self.start_new_group_call_session(group_peer_id, join_as, can_manage, now_ms)
                {
                    self.toast_queue
                        .push(format!("Failed to create group call: {err}"));
                }
            }
            LiveCallInboundEvent::GroupCreateResult { result } => match result {
                Ok(updates) => {
                    self.apply_inline_updates(updates, now_ms);
                    self.process_group_actions(now_ms);
                }
                Err(err) => {
                    self.toast_queue
                        .push(format!("Failed to create group call: {err}"));
                    if let Some(group) = self.active_group.as_mut() {
                        group.session.fail();
                    }
                    self.process_group_actions(now_ms);
                }
            },
            LiveCallInboundEvent::GroupConferenceCreateResult {
                result,
                was_mute_state,
                was_video_stopped,
            } => match result {
                Ok(updates) => {
                    self.complete_conference_or_group_join(
                        updates,
                        was_mute_state,
                        was_video_stopped,
                        now_ms,
                    );
                }
                Err(err) => {
                    self.toast_queue
                        .push(format!("Failed to create conference call: {err}"));
                    if let Some(group) = self.active_group.as_mut() {
                        group.session.handle_join_fail(&err);
                    }
                    self.process_group_actions(now_ms);
                }
            },
            LiveCallInboundEvent::GroupLastChainBlockLoaded { result } => match result {
                Ok(updates) => {
                    apply_updates_via_runtime(&self.runtime, updates.clone());
                    let (_, _, list) = unpack_tl_updates(updates);
                    let mut last_block: Option<Vec<u8>> = None;
                    for upd in list {
                        match upd {
                            tl_enums::Update::GroupCall(u) => {
                                self.handle_tl_group_call(u.peer, u.call, now_ms);
                            }
                            tl_enums::Update::GroupCallChainBlocks(u) => {
                                if let Some(b) = u.blocks.last() {
                                    last_block = Some(b.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(group) = self.active_group.as_mut()
                        && let Some(e2e) = group.session.e2e_mut()
                    {
                        e2e.refresh_last_block0(last_block);
                    }
                    self.send_pending_conference_join(now_ms);
                }
                Err(err) => {
                    self.toast_queue
                        .push(format!("Failed to load conference chain block: {err}"));
                    if let Some(group) = self.active_group.as_mut() {
                        group.pending_conference_join = None;
                        group.session.handle_join_fail(&err);
                    }
                    self.process_group_actions(now_ms);
                }
            },
            LiveCallInboundEvent::GroupChainBlocksResponse { subchain, result } => {
                match result {
                    Ok(updates) => {
                        apply_updates_via_runtime(&self.runtime, updates.clone());
                        let (_, _, list) = unpack_tl_updates(updates);
                        let mut chain_updates = Vec::new();
                        let mut others = Vec::new();
                        for upd in list {
                            if let tl_enums::Update::GroupCallChainBlocks(u) = upd {
                                if let Ok(conv) = UpdateGroupCallChainBlocks::try_from(u) {
                                    chain_updates.push(conv);
                                }
                            } else {
                                others.push(upd);
                            }
                        }
                        if let Some(group) = self.active_group.as_mut() {
                            if let Err(err) = group
                                .session
                                .finish_subchain_request(subchain, &chain_updates)
                            {
                                debug!(error = %err, subchain, "failed applying subchain response blocks");
                            }
                            if group.session.e2e().is_some_and(E2eCall::is_initialized) {
                                group.session.messages_mut().set_call_ready(true);
                            }
                        }
                        for upd in others {
                            self.apply_single_tl_update(upd, now_ms);
                        }
                    }
                    Err(err) => {
                        if let Some(group) = self.active_group.as_mut() {
                            let _ = group.session.finish_subchain_request(subchain, &[]);
                            if err == "GROUPCALL_FORBIDDEN" {
                                group.session.rejoin();
                            }
                        }
                    }
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupBroadcastSent { block, result } => {
                match result {
                    Ok(updates) => {
                        self.apply_inline_updates(updates, now_ms);
                    }
                    Err(err) => {
                        if err == "GROUPCALL_FORBIDDEN" {
                            if let Some(group) = self.active_group.as_mut() {
                                group.pending_outbound_block = Some(block);
                                group.session.rejoin();
                            }
                        } else if is_permanent_broadcast_error(&err) {
                            debug!(error = %err, "conference broadcast rejected permanently");
                        } else {
                            debug!(error = %err, "conference broadcast failed");
                        }
                    }
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupMessageSent {
                random_id,
                paid_reaction,
                result,
            } => {
                match result {
                    Ok(updates) => {
                        self.apply_inline_updates(updates, now_ms);
                        let next_rand = self.next_random_u64();
                        if paid_reaction && let Some(group) = self.active_group.as_mut() {
                            group.session.messages_mut().finish_paid_sending(
                                true,
                                next_rand,
                                now_unix_i32(),
                            );
                        }
                    }
                    Err(err) => {
                        let next_rand = self.next_random_u64();
                        if let Some(group) = self.active_group.as_mut() {
                            group
                                .session
                                .messages_mut()
                                .handle_send_failed(random_id, now_unix_i32());
                            if paid_reaction {
                                group.session.messages_mut().finish_paid_sending(
                                    false,
                                    next_rand,
                                    now_unix_i32(),
                                );
                            }
                        }
                        self.toast_queue
                            .push(format!("Failed to send in-call message: {err}"));
                    }
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupEncryptedMessageSent { random_id, result } => {
                match result {
                    Ok(true) => {
                        if let Some(group) = self.active_group.as_mut() {
                            group
                                .session
                                .messages_mut()
                                .handle_encrypted_message_sent(random_id, now_unix_i32());
                        }
                    }
                    Ok(false) => {
                        if let Some(group) = self.active_group.as_mut() {
                            group
                                .session
                                .messages_mut()
                                .handle_send_failed(random_id, now_unix_i32());
                        }
                    }
                    Err(err) => {
                        if let Some(group) = self.active_group.as_mut() {
                            group
                                .session
                                .messages_mut()
                                .handle_send_failed(random_id, now_unix_i32());
                        }
                        self.toast_queue
                            .push(format!("Failed to send encrypted call message: {err}"));
                    }
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupStarsLoaded(resp) => {
                if let Some(group) = self.active_group.as_mut() {
                    group.session.messages_mut().apply_stars_stats(&resp);
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupFullCallLoaded { call_id, response } => {
                let next_offset = response.participants_next_offset.clone();
                let mut stream_dc = None;
                let mut input_call_for_page = None;
                if let Some(group) = self.active_group.as_mut()
                    && (group.session.id() == call_id || group.session.id().is_zero())
                {
                    if let GroupCallObject::Active(ref info) = response.call {
                        if let Some(ref title) = info.title {
                            group.title = title.clone();
                        }
                        group.messages_enabled = info.messages_enabled;
                        group.recording = info.record_start_date.unwrap_or(0) != 0;
                        group.is_video_stream = info.rtmp_stream || group.group_peer.is_broadcast;
                        if let Some(min_stars) = info.send_paid_messages_stars {
                            group.min_paid_stars = min_stars;
                            group.session.messages_mut().configure_limits(
                                DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
                                min_stars,
                                default_stars_colorings(),
                            );
                        }
                        if let Some(ref link) = info.invite_link
                            && let Some(slug) = extract_conference_slug(link)
                        {
                            group.session.set_conference_slug(Some(slug));
                        }
                        group.session.messages_mut().set_call_ready(true);
                        group.participants_loaded = true;
                        stream_dc = info.stream_dc_id;
                        if let Some(dc) = stream_dc {
                            group.broadcast_loader.set_stream_dc_id(dc);
                        }
                        group
                            .broadcast_loader
                            .set_call(tl_enums::InputGroupCall::InputGroupCall(
                                tl_types::InputGroupCall {
                                    id: info.id.to_tl(),
                                    access_hash: info.access_hash,
                                },
                            ));
                        input_call_for_page = Some(InputGroupCall::Id {
                            id: info.id,
                            access_hash: info.access_hash,
                        });
                    }
                    group
                        .session
                        .process_full_call(response.call, &response.participants);
                    register_all_group_ssrcs(group);
                }
                if let (Some(call_handle), false) = (input_call_for_page, next_offset.is_empty()) {
                    self.spawn_get_group_participants_page(call_handle, next_offset);
                }
                let _ = stream_dc;
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupJoinResult {
                result,
                was_mute_state,
                was_video_stopped,
            } => match result {
                Ok(updates) => {
                    self.complete_conference_or_group_join(
                        updates,
                        was_mute_state,
                        was_video_stopped,
                        now_ms,
                    );
                }
                Err(err) => {
                    let retry_chain_call = self.active_group.as_mut().and_then(|group| {
                        if group.session.is_conference() && is_chain_invalid_error(&err) {
                            let call = group
                                .pending_conference_join
                                .as_ref()
                                .map(|p| p.call.clone())
                                .unwrap_or_else(|| group.session.input_call_handle());
                            if let Some(e2e) = group.session.e2e_mut() {
                                e2e.refresh_last_block0(None);
                            }
                            Some(call)
                        } else {
                            None
                        }
                    });
                    if let Some(call) = retry_chain_call {
                        self.spawn_get_last_chain_block(call);
                        return;
                    }
                    if let Some(group) = self.active_group.as_mut() {
                        group.pending_conference_join = None;
                        if err == "GROUPCALL_SSRC_DUPLICATE_MUCH" {
                            group.engine.regenerate_credentials();
                        }
                        group.session.handle_join_fail(&err);
                    }
                    self.toast_queue
                        .push(format!("Failed to join group call: {err}"));
                    self.process_group_actions(now_ms);
                }
            },
            LiveCallInboundEvent::GroupLeaveDone { updates } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_leave_done();
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupDiscardDone { updates } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_discard_done();
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::RecentCallsLoaded(entries) => {
                self.recent_calls = entries;
            }
            LiveCallInboundEvent::GroupCheckResult(res) => {
                if let Some(group) = self.active_group.as_mut() {
                    match res {
                        Ok(active) => group.session.handle_check_group_call_result(Ok(&active)),
                        Err(err) => group.session.handle_check_group_call_result(Err(&err)),
                    }
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupSelfUpdateDone { updates, error } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_self_update_done(error.as_deref());
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupScreenJoinDone { updates } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_screen_join_done();
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupScreenLeaveDone { updates } => {
                if let Some(upds) = updates {
                    self.apply_inline_updates(upds, now_ms);
                }
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_screen_leave_done();
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupResolvedSsrcs {
                participants,
                resolved_ssrcs,
            } => {
                if let Some(group) = self.active_group.as_mut() {
                    group.session.handle_resolved_unknown_participants(
                        &participants,
                        &resolved_ssrcs,
                        now_ms,
                    );
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupParticipantsPage { participants } => {
                if let Some(group) = self.active_group.as_mut() {
                    group
                        .session
                        .apply_participants_slice(&participants, ApplySliceSource::SliceLoaded);
                }
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::GroupUpdatesOnly(updates) => {
                self.apply_inline_updates(updates, now_ms);
                self.process_group_actions(now_ms);
            }
            LiveCallInboundEvent::Toast(msg) => {
                self.toast_queue.push(msg);
            }
        }
    }

    fn apply_inline_updates(&mut self, updates: tl_enums::Updates, now_ms: u64) {
        apply_updates_via_runtime(&self.runtime, updates.clone());
        let (_, _, list) = unpack_tl_updates(updates);
        // Apply `Update::GroupCall` first so newly assigned call IDs/access hashes are visible
        // before sibling updates (`GroupCallChainBlocks`, `GroupCallParticipants`, etc.).
        let mut deferred = Vec::with_capacity(list.len());
        for upd in list {
            if let tl_enums::Update::GroupCall(u) = upd {
                self.handle_tl_group_call(u.peer, u.call, now_ms);
            } else {
                deferred.push(upd);
            }
        }
        for upd in deferred {
            self.apply_single_tl_update(upd, now_ms);
        }
    }

    fn apply_single_tl_update(&mut self, upd: tl_enums::Update, now_ms: u64) {
        match upd {
            tl_enums::Update::PhoneCall(u) => {
                self.handle_tl_phone_call(u.phone_call, now_ms);
            }
            tl_enums::Update::PhoneCallSignalingData(u) => {
                let converted: UpdatePhoneCallSignalingData = u.into();
                self.instance.handle_signaling_data(converted);
                self.process_instance_actions(now_ms);
            }
            tl_enums::Update::GroupCall(u) => {
                self.handle_tl_group_call(u.peer, u.call, now_ms);
            }
            tl_enums::Update::GroupCallParticipants(u) => {
                self.handle_tl_group_call_participants(u, now_ms);
            }
            tl_enums::Update::GroupCallConnection(u) => {
                self.handle_tl_group_call_connection(u, now_ms);
            }
            tl_enums::Update::GroupCallChainBlocks(u) => {
                self.handle_tl_group_call_chain_blocks(u, now_ms);
            }
            tl_enums::Update::GroupCallMessage(u) => {
                self.handle_tl_group_call_message(u, now_ms);
            }
            tl_enums::Update::GroupCallEncryptedMessage(u) => {
                self.handle_tl_group_call_encrypted_message(u, now_ms);
            }
            tl_enums::Update::DeleteGroupCallMessages(u) => {
                self.handle_tl_delete_group_call_messages(u, now_ms);
            }
            tl_enums::Update::MessageID(u) => {
                if let Some(group) = self.active_group.as_mut() {
                    group.session.messages_mut().handle_message_sent(
                        u.random_id as u64,
                        u.id,
                        now_unix_i32(),
                    );
                }
            }
            _ => {}
        }
    }

    fn complete_conference_or_group_join(
        &mut self,
        updates: tl_enums::Updates,
        was_mute_state: MuteState,
        was_video_stopped: bool,
        now_ms: u64,
    ) {
        apply_updates_via_runtime(&self.runtime, updates.clone());
        let (_, _, list) = unpack_tl_updates(updates);
        if let Some(group) = self.active_group.as_mut() {
            group.session.messages_mut().set_applying_initial(true);
        }

        // Pass 1: `Update::GroupCall` (sets call_id, access_hash, invite_link/slug).
        let mut chain_blocks = Vec::new();
        let mut rest = Vec::new();
        for upd in list {
            match upd {
                tl_enums::Update::GroupCall(u) => {
                    self.handle_tl_group_call(u.peer, u.call, now_ms);
                }
                tl_enums::Update::GroupCallChainBlocks(u) => {
                    chain_blocks.push(u);
                }
                other => rest.push(other),
            }
        }

        // Pass 2: initial `Update::GroupCallChainBlocks` applied with `from_short_poll = true`
        // (matching `calls_group_call.cpp:1808-1818` so mid-call join snapshots initialize E2E).
        for upd in chain_blocks {
            if let Ok(converted) = UpdateGroupCallChainBlocks::try_from(upd)
                && let Some(group) = self.active_group.as_mut()
                && group.session.id() == converted.call_id
                && let Some(e2e) = group.session.e2e_mut()
                && let Err(err) = e2e.apply_blocks(
                    converted.sub_chain_id,
                    converted.next_offset,
                    &converted.blocks,
                    true,
                )
            {
                debug!(error = %err, "failed applying initial conference chain block");
            }
        }

        // Pass 3: remaining updates (`GroupCallConnection`, `GroupCallParticipants`, messages...).
        for upd in rest {
            self.apply_single_tl_update(upd, now_ms);
        }

        let mut pending_outbound = None;
        let mut need_initial_chain_fetch = None;
        if let Some(group) = self.active_group.as_mut() {
            group.session.messages_mut().set_applying_initial(false);
            if !group.session.id().is_zero() {
                group.session.messages_mut().set_call_ready(true);
            }
            group.pending_conference_join = None;
            group
                .session
                .handle_join_done(was_mute_state, was_video_stopped);
            if group.session.is_conference() {
                let initialized = group.session.e2e().is_some_and(E2eCall::is_initialized);
                if initialized {
                    if let Some(e2e) = group.session.e2e_mut() {
                        e2e.joined();
                    }
                    if let Some(block) = group.pending_outbound_block.take() {
                        pending_outbound = Some((group.session.input_call_handle(), block));
                    }
                } else if !group.session.id().is_zero() {
                    need_initial_chain_fetch = Some(group.session.input_call_handle());
                }
            }
        }
        if let Some((call, block)) = pending_outbound {
            self.spawn_send_conference_broadcast(call, block);
        }
        if let Some(call) = need_initial_chain_fetch {
            if let Some(group) = self.active_group.as_mut() {
                group.session.begin_subchain_request(SUBCHAIN_STATE);
            }
            self.spawn_get_chain_blocks(
                call,
                SUBCHAIN_STATE,
                0,
                SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST,
            );
        }
        self.process_group_actions(now_ms);
    }

    fn stop_call_engine_into_call(&mut self) -> i64 {
        let Some(mut engine) = self.call_engine.take() else {
            return 0;
        };
        let final_state = CallMediaEngine::stop(&mut engine);
        let relay_id = final_state.preferred_relay_id;
        if let Some(call) = self.instance.current_call_mut() {
            if !final_state.debug_log.is_empty() {
                call.set_debug_log(final_state.debug_log);
            }
            if relay_id != 0 {
                call.set_preferred_relay_id(relay_id);
            }
        }
        self.call_screen_share_active = false;
        self.call_latest_video_frame = None;
        relay_id
    }

    fn handle_tl_phone_call(&mut self, tl_call: tl_enums::PhoneCall, now_ms: u64) {
        // If remote party discarded with `need_debug`, capture the media engine debug log
        // BEFORE `Call::handle_update(Discarded)` inspects `self.debug_log`.
        if let tl_enums::PhoneCall::Discarded(ref disc) = tl_call {
            if disc.need_debug {
                let _ = self.stop_call_engine_into_call();
            }
            self.record_recent_call_from_discard(disc, now_ms);
        }

        match PhoneCallUpdate::try_from(tl_call) {
            Ok(update) => {
                self.instance
                    .handle_call_update(update, now_unix_i32(), now_ms);
                self.process_instance_actions(now_ms);
            }
            Err(err) => {
                warn!(error = %err, "failed to convert PhoneCall TL update");
            }
        }
    }

    fn record_recent_call_from_discard(
        &mut self,
        disc: &tl_types::PhoneCallDiscarded,
        now_ms: u64,
    ) {
        let Some(call) = self.instance.current_call() else {
            return;
        };
        if call.id() != CallId::from_tl(disc.id) {
            return;
        }
        let peer_id = PeerId::from_user(call.peer_user_id().raw());
        let peer = self.peer_info(peer_id);
        let direction = match call.call_type() {
            CallType::Outgoing => RecentCallDirection::Out,
            CallType::Incoming => {
                if matches!(
                    disc.reason,
                    Some(
                        tl_enums::PhoneCallDiscardReason::Missed
                            | tl_enums::PhoneCallDiscardReason::Busy
                    )
                ) && disc.duration.unwrap_or(0) == 0
                {
                    RecentCallDirection::Missed
                } else {
                    RecentCallDirection::In
                }
            }
        };
        let duration_secs = disc.duration.filter(|&d| d > 0).map(i64::from).or_else(|| {
            let ms = call.duration_ms(now_ms);
            (ms >= 1000).then_some((ms / 1000) as i64)
        });
        let dt = unix_to_local_datetime(now_unix_i32());
        let msg_id = self.next_recent_msg_id;
        self.next_recent_msg_id = self.next_recent_msg_id.saturating_add(1);
        self.recent_calls.insert(
            0,
            RecentCallEntry {
                msg_id,
                peer,
                direction,
                is_video: disc.video || call.is_sharing_video(),
                days_ago: 0,
                hour: dt.hour,
                minute: dt.minute,
                duration_secs,
            },
        );
    }

    fn handle_tl_group_call(
        &mut self,
        peer: Option<tl_enums::Peer>,
        call: tl_enums::GroupCall,
        now_ms: u64,
    ) {
        let obj: GroupCallObject = call.into();
        let peer_id = peer
            .as_ref()
            .map(tg_calls::tl_conv::peer_from_tl)
            .map(from_calls_peer_id);

        match &obj {
            GroupCallObject::Active(info) => {
                if let Some(pid) = peer_id {
                    self.known_group_calls.insert(
                        pid,
                        KnownGroupCall {
                            call_id: info.id,
                            access_hash: info.access_hash,
                            can_manage: info.creator,
                            stream_dc_id: info.stream_dc_id,
                        },
                    );
                    self.refresh_active_group_peers();
                }
                if let Some(group) = self.active_group.as_mut() {
                    if group.session.id().is_zero() {
                        let matches_peer = peer_id.is_none_or(|pid| pid == group.group_peer.id);
                        if matches_peer {
                            self.apply_created_group_call_to_active(info, now_ms);
                            return;
                        }
                    } else if group.session.id() == info.id {
                        if let Some(ref title) = info.title {
                            group.title = title.clone();
                        }
                        group.messages_enabled = info.messages_enabled;
                        group.recording = info.record_start_date.unwrap_or(0) != 0;
                        if let Some(dc) = info.stream_dc_id {
                            group.broadcast_loader.set_stream_dc_id(dc);
                        }
                        group
                            .session
                            .handle_versioned_update(VersionedGroupCallUpdate::Call(obj));
                    }
                }
            }
            GroupCallObject::Discarded(disc) => {
                if let Some(pid) = peer_id {
                    self.known_group_calls.remove(&pid);
                } else {
                    self.known_group_calls.retain(|_, v| v.call_id != disc.id);
                }
                self.refresh_active_group_peers();
                if let Some(group) = self.active_group.as_mut()
                    && group.session.id() == disc.id
                {
                    group
                        .session
                        .handle_versioned_update(VersionedGroupCallUpdate::Call(obj));
                }
            }
        }
        self.process_group_actions(now_ms);
    }

    fn apply_created_group_call_to_active(&mut self, info: &GroupCallInfo, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            if let Some(ref title) = info.title {
                group.title = title.clone();
            }
            group.messages_enabled = info.messages_enabled;
            group.recording = info.record_start_date.unwrap_or(0) != 0;
            group.is_video_stream = info.rtmp_stream || group.group_peer.is_broadcast;
            if let Some(min_stars) = info.send_paid_messages_stars {
                group.min_paid_stars = min_stars;
                group.session.messages_mut().configure_limits(
                    DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
                    min_stars,
                    default_stars_colorings(),
                );
            }
            if let Some(ref link) = info.invite_link
                && let Some(slug) = extract_conference_slug(link)
            {
                group.session.set_conference_slug(Some(slug));
            }
            group
                .broadcast_loader
                .set_call(tl_enums::InputGroupCall::InputGroupCall(
                    tl_types::InputGroupCall {
                        id: info.id.to_tl(),
                        access_hash: info.access_hash,
                    },
                ));
            if let Some(dc) = info.stream_dc_id {
                group.broadcast_loader.set_stream_dc_id(dc);
            }
            group.session.apply_created_call(info);
            group.session.messages_mut().set_call_ready(true);
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_group_call_chain_blocks(
        &mut self,
        upd: tl_types::UpdateGroupCallChainBlocks,
        now_ms: u64,
    ) {
        let converted = match UpdateGroupCallChainBlocks::try_from(upd) {
            Ok(c) => c,
            Err(err) => {
                debug!(error = %err, "ignoring updateGroupCallChainBlocks without numeric ID");
                return;
            }
        };
        if let Some(group) = self.active_group.as_mut() {
            if let Err(err) = group.session.handle_chain_blocks_update(&converted) {
                debug!(error = %err, "failed to apply updateGroupCallChainBlocks");
            }
            if group.session.e2e().is_some_and(E2eCall::is_initialized) {
                group.session.messages_mut().set_call_ready(true);
            }
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_group_call_message(&mut self, upd: tl_types::UpdateGroupCallMessage, now_ms: u64) {
        let converted = match UpdateGroupCallMessage::try_from(upd) {
            Ok(c) => c,
            Err(err) => {
                debug!(error = %err, "ignoring updateGroupCallMessage without numeric ID");
                return;
            }
        };
        if let Some(group) = self.active_group.as_mut()
            && group.session.id() == converted.call_id
        {
            group
                .session
                .messages_mut()
                .handle_incoming_message(&converted, now_unix_i32());
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_group_call_encrypted_message(
        &mut self,
        upd: tl_types::UpdateGroupCallEncryptedMessage,
        now_ms: u64,
    ) {
        let converted = match UpdateGroupCallEncryptedMessage::try_from(upd) {
            Ok(c) => c,
            Err(err) => {
                debug!(error = %err, "ignoring updateGroupCallEncryptedMessage without numeric ID");
                return;
            }
        };
        if let Some(group) = self.active_group.as_mut()
            && group.session.id() == converted.call_id
        {
            group
                .session
                .messages_mut()
                .handle_incoming_encrypted_message(&converted, now_unix_i32());
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_delete_group_call_messages(
        &mut self,
        upd: tl_types::UpdateDeleteGroupCallMessages,
        now_ms: u64,
    ) {
        let converted = match UpdateDeleteGroupCallMessages::try_from(upd) {
            Ok(c) => c,
            Err(err) => {
                debug!(error = %err, "ignoring updateDeleteGroupCallMessages without numeric ID");
                return;
            }
        };
        if let Some(group) = self.active_group.as_mut()
            && group.session.id() == converted.call_id
        {
            group
                .session
                .messages_mut()
                .handle_deleted_messages(&converted);
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_group_call_participants(
        &mut self,
        upd: tl_types::UpdateGroupCallParticipants,
        now_ms: u64,
    ) {
        let converted = match UpdateGroupCallParticipants::try_from(upd) {
            Ok(c) => c,
            Err(err) => {
                debug!(error = %err, "ignoring updateGroupCallParticipants without numeric ID");
                return;
            }
        };
        if let Some(group) = self.active_group.as_mut()
            && group.session.id() == converted.call_id
        {
            group
                .session
                .handle_versioned_update(VersionedGroupCallUpdate::Participants(converted));
        }
        self.process_group_actions(now_ms);
    }

    fn handle_tl_group_call_connection(
        &mut self,
        upd: tl_types::UpdateGroupCallConnection,
        now_ms: u64,
    ) {
        let converted: UpdateGroupCallConnection = upd.into();
        if let Some(group) = self.active_group.as_mut() {
            if !converted.presentation
                && matches!(
                    parse_join_response(&converted.params_json),
                    Ok(ParsedJoinResponse::BroadcastStream { .. })
                )
            {
                group.last_stream_params_json = Some(converted.params_json.clone());
            }
            group.session.handle_connection_update(&converted);
        }
        self.process_group_actions(now_ms);
    }

    fn refresh_active_group_peers(&mut self) {
        let ids: Vec<PeerId> = self.known_group_calls.keys().copied().collect();
        self.active_group_calls = ids.into_iter().map(|id| self.peer_info(id)).collect();
    }

    // -------------------------------------------------------------------------
    // Action execution: 1:1 Calls (`InstanceAction` / `CallAction`)
    // -------------------------------------------------------------------------

    fn process_instance_actions(&mut self, now_ms: u64) {
        for _ in 0..8 {
            let actions = self.instance.take_actions();
            if actions.is_empty() {
                break;
            }
            if actions
                .iter()
                .any(|a| matches!(a, InstanceAction::Call(CallAction::ShowRatingBox)))
                && let Some(call) = self.instance.current_call_mut()
            {
                call.take_rating_to_panel();
            }
            for action in actions {
                match action {
                    InstanceAction::GetCallConfig => {
                        self.spawn_get_call_config();
                    }
                    InstanceAction::GetDhConfig {
                        version,
                        random_length,
                    } => {
                        self.spawn_get_dh_config(version, random_length);
                    }
                    InstanceAction::DiscardIncomingBusy { video, peer } => {
                        self.spawn_discard_call(
                            video,
                            peer,
                            0,
                            tg_calls::types::PhoneCallDiscardReason::Busy,
                            0,
                            false,
                        );
                    }
                    InstanceAction::Call(call_action) => {
                        self.execute_call_action(call_action, now_ms);
                    }
                }
            }
        }
    }

    fn execute_call_action(&mut self, action: CallAction, now_ms: u64) {
        match action {
            CallAction::RequestCall {
                video,
                user_id,
                random_id,
                g_a_hash,
                protocol,
            } => {
                self.spawn_request_call(video, user_id, random_id, g_a_hash, protocol);
            }
            CallAction::ReceivedCall { peer } => {
                self.spawn_received_call(peer);
            }
            CallAction::AcceptCall {
                peer,
                g_b,
                protocol,
            } => {
                self.spawn_accept_call(peer, g_b, protocol);
            }
            CallAction::ConfirmCall {
                peer,
                g_a,
                key_fingerprint,
                protocol,
            } => {
                self.spawn_confirm_call(peer, g_a, key_fingerprint, protocol);
            }
            CallAction::DiscardCall {
                video,
                peer,
                duration,
                reason,
                connection_id,
            } => {
                let stopped_relay = self.stop_call_engine_into_call();
                let effective_conn_id = if connection_id != 0 {
                    connection_id
                } else {
                    stopped_relay
                };
                self.spawn_discard_call(video, peer, duration, reason, effective_conn_id, true);
            }
            CallAction::SetCallRating {
                user_initiative,
                peer,
                rating,
                comment,
            } => {
                self.spawn_set_call_rating(user_initiative, peer, rating, comment);
            }
            CallAction::SaveCallDebug { peer, debug_json } => {
                self.spawn_save_call_debug(peer, debug_json);
            }
            CallAction::SendSignalingData { peer, data } => {
                self.spawn_send_signaling_data(peer, data);
            }
            CallAction::GetGroupCall { call, limit } => {
                self.spawn_get_group_call(call, limit);
            }
            CallAction::DeclineIncomingConferenceInvites { .. } => {
                if let Some(call) = self.instance.current_call()
                    && let Some(msg_id) = call.conference_invite_msg_id().as_i32()
                    && msg_id > 0
                {
                    let rpc = self.runtime.rpc().clone();
                    self.runtime.tokio_handle().spawn(async move {
                        let _ = tokio::time::timeout(
                            CALL_RPC_TIMEOUT,
                            rpc.invoke(&tl_fns::phone::DeclineConferenceCallInvite { msg_id }),
                        )
                        .await;
                    });
                }
            }
            CallAction::StartController(desc) => {
                let mut cfg = self.media_config.clone();
                cfg.input_device_id = self.audio_settings.input_device_id.clone();
                cfg.output_device_id = self.audio_settings.output_device_id.clone();
                cfg.dsp.enable_ns = self.audio_settings.noise_suppression;

                let mut engine = MediaEngine::new(cfg, self.runtime.tokio_handle().clone());
                match CallMediaEngine::start(&mut engine, *desc) {
                    Ok(()) => {
                        if let Some(call) = self.instance.current_call()
                            && call.muted()
                        {
                            CallMediaEngine::set_muted(&mut engine, true);
                        }
                        self.call_engine = Some(engine);
                    }
                    Err(err) => {
                        self.toast_queue
                            .push(format!("VoIP media engine failed to start: {err}"));
                        if let Some(call) = self.instance.current_call_mut() {
                            call.handle_controller_state_change(
                                ControllerState::Failed("ERROR_AUDIO_IO".to_owned()),
                                now_ms,
                            );
                        }
                    }
                }
            }
            CallAction::DestroyController => {
                let _ = self.stop_call_engine_into_call();
            }
            CallAction::ReceiveSignalingData(bytes) => {
                if let Some(engine) = self.call_engine.as_mut() {
                    CallMediaEngine::receive_signaling_data(engine, bytes);
                }
            }
            CallAction::RequestPermissions { .. } => {
                if let Some(call) = self.instance.current_call_mut() {
                    call.actually_answer();
                }
            }
            CallAction::StartWaitingTrack { outgoing } => {
                self.sound_queue
                    .push(BackendSoundEvent::StartWaitingTrack { outgoing });
            }
            CallAction::StopWaitingTrack => {
                self.sound_queue.push(BackendSoundEvent::StopWaitingTrack);
            }
            CallAction::PlaySound(sound) => {
                self.sound_queue.push(BackendSoundEvent::CallSound(sound));
            }
            CallAction::SetTimer { kind, duration_ms } => {
                self.call_timers
                    .insert(kind, now_ms.saturating_add(duration_ms));
            }
            CallAction::CancelTimer(kind) => {
                self.call_timers.remove(&kind);
            }
            CallAction::ShowRatingBox => {
                if let Some(call) = self.instance.current_call_mut() {
                    call.take_rating_to_panel();
                }
            }
            CallAction::ShowError(err) => {
                self.toast_queue.push(err);
            }
            CallAction::NotifyFinished | CallAction::NotifyFailed => {
                let keep_rating = self
                    .instance
                    .current_call()
                    .is_some_and(|c| c.rating_in_panel());
                if !keep_rating {
                    let _ = self.stop_call_engine_into_call();
                    self.instance.destroy_current_call();
                    self.call_timers.clear();
                    self.call_snapshot = None;
                    self.call_submitted_rating = None;
                    self.call_latest_video_frame = None;
                }
            }
            CallAction::NotifyRedial
            | CallAction::StateChanged { .. }
            | CallAction::SignalBarCountChanged(_) => {}
        }
    }

    // -------------------------------------------------------------------------
    // Action execution: Group Calls (`GroupCallAction`)
    // -------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn start_group_call_session(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
        call_id: CallId,
        access_hash: i64,
        can_manage: bool,
        stream_dc_id: Option<i32>,
        now_ms: u64,
    ) -> anyhow::Result<()> {
        if let Some(mut old) = self.active_group.take() {
            old.engine.stop();
        }

        let mut cfg = self.group_media_config.clone();
        cfg.input_device_id = self.audio_settings.input_device_id.clone();
        cfg.output_device_id = self.audio_settings.output_device_id.clone();
        cfg.dsp.enable_ns = self.audio_settings.noise_suppression;

        let mut engine = GroupMediaEngine::new(cfg, self.runtime.tokio_handle().clone())?;
        let tl_input_call = tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: call_id.to_tl(),
            access_hash,
        });
        let broadcast_loader = Arc::new(TelegramBroadcastChunkLoader::new(
            self.runtime.rpc().clone(),
            tl_input_call,
            stream_dc_id,
        ));
        engine.set_broadcast_loader(Arc::clone(&broadcast_loader) as Arc<dyn BroadcastChunkLoader>);

        let group_peer = self.peer_info(group_peer_id);
        let is_video_stream = group_peer.is_broadcast;
        let mut session = GroupCallSession::new(
            to_calls_peer_id(group_peer.id),
            to_calls_peer_id(join_as),
            call_id,
            access_hash,
            can_manage,
            0,
            false,
        );
        session.messages_mut().configure_limits(
            DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
            0,
            default_stars_colorings(),
        );
        if !call_id.is_zero() {
            session.messages_mut().set_call_ready(true);
        }

        let title = group_peer.name.clone();
        let initial_snapshot = GroupCallSnapshot {
            group_peer: group_peer.clone(),
            title: title.clone(),
            join_as,
            state: session.state(),
            instance_state: session.instance_state(),
            mute_state: session.muted(),
            can_manage,
            join_muted: false,
            messages_enabled: true,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            members: Vec::new(),
            large_video: None,
            large_video_pinned: false,
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            is_conference: false,
            emoji_fingerprint: None,
            e2e_verified_count: 0,
            chat_messages: Vec::new(),
            stars_top: Vec::new(),
            stars_total: 0,
            stars_my_total: 0,
            stars_scheduled: 0,
            min_paid_stars: 0,
            is_video_stream,
        };

        self.active_group = Some(LiveActiveGroup {
            session,
            engine,
            broadcast_loader,
            group_peer,
            title,
            can_manage,
            messages_enabled: true,
            is_video_stream,
            min_paid_stars: 0,
            participants_loaded: false,
            pending_conference_create: false,
            pending_conference_join: None,
            pending_outbound_block: None,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            ssrc_levels: HashMap::new(),
            timers: HashMap::new(),
            e2e_timers: HashMap::new(),
            last_messages_prune_ms: now_ms,
            stars_poll_due_ms: is_video_stream
                .then_some(now_ms.saturating_add(STARS_STATS_SHORT_POLL_DELAY_MS)),
            last_stream_params_json: None,
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            snapshot: initial_snapshot,
        });
        self.instance.set_in_group_call(true);

        if !call_id.is_zero() {
            self.spawn_get_group_call(
                InputGroupCall::Id {
                    id: call_id,
                    access_hash,
                },
                GROUP_CALL_PARTICIPANTS_LIMIT,
            );
        }

        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
        Ok(())
    }

    fn start_conference_call_session(
        &mut self,
        slug: Option<String>,
        key_pair: Option<E2eKeyPair>,
        now_ms: u64,
    ) -> anyhow::Result<()> {
        if let Some(mut old) = self.active_group.take() {
            old.engine.stop();
        }

        let (self_peer_id, self_uid_raw) = read_session_sync(&self.runtime, |s| {
            let pid = s.self_peer_id();
            (PeerId::from_user(pid.bare()), pid.bare())
        });
        let self_calls_uid = CallsUserId(self_uid_raw);
        let self_calls_peer = to_calls_peer_id(self_peer_id);
        let group_peer = self.peer_info(self_peer_id);

        let mut cfg = self.group_media_config.clone();
        cfg.input_device_id = self.audio_settings.input_device_id.clone();
        cfg.output_device_id = self.audio_settings.output_device_id.clone();
        cfg.dsp.enable_ns = self.audio_settings.noise_suppression;
        cfg.e2e_frame_encryption = true;

        let mut engine = GroupMediaEngine::new(cfg, self.runtime.tokio_handle().clone())?;
        let tl_input_call = match slug {
            Some(ref s) if !s.is_empty() => {
                tl_enums::InputGroupCall::Slug(tl_types::InputGroupCallSlug { slug: s.clone() })
            }
            _ => tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                id: 0,
                access_hash: 0,
            }),
        };
        let broadcast_loader = Arc::new(TelegramBroadcastChunkLoader::new(
            self.runtime.rpc().clone(),
            tl_input_call,
            None,
        ));
        engine.set_broadcast_loader(Arc::clone(&broadcast_loader) as Arc<dyn BroadcastChunkLoader>);

        let is_creating = slug.as_deref().is_none_or(str::is_empty);
        let mut session = GroupCallSession::new(
            self_calls_peer,
            self_calls_peer,
            CallId::ZERO,
            0,
            is_creating,
            0,
            false,
        );
        if let Some(s) = slug.filter(|s| !s.is_empty()) {
            session.set_conference_slug(Some(s));
        }
        let kp = key_pair.unwrap_or_else(E2eKeyPair::generate);
        session.enable_conference_e2e_with_key_pair(self_calls_uid, kp);
        engine.set_e2e_encrypt_decrypt(session.e2e_encrypt_decrypt());
        session.messages_mut().configure_limits(
            DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
            0,
            default_stars_colorings(),
        );
        session.initial_join();

        let title = "Encrypted Call".to_owned();
        let initial_snapshot = GroupCallSnapshot {
            group_peer: group_peer.clone(),
            title: title.clone(),
            join_as: self_peer_id,
            state: session.state(),
            instance_state: session.instance_state(),
            mute_state: session.muted(),
            can_manage: is_creating,
            join_muted: false,
            messages_enabled: true,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            members: Vec::new(),
            large_video: None,
            large_video_pinned: false,
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            is_conference: true,
            emoji_fingerprint: None,
            e2e_verified_count: 0,
            chat_messages: Vec::new(),
            stars_top: Vec::new(),
            stars_total: 0,
            stars_my_total: 0,
            stars_scheduled: 0,
            min_paid_stars: 0,
            is_video_stream: false,
        };

        self.active_group = Some(LiveActiveGroup {
            session,
            engine,
            broadcast_loader,
            group_peer,
            title,
            can_manage: is_creating,
            messages_enabled: true,
            is_video_stream: false,
            min_paid_stars: 0,
            participants_loaded: false,
            pending_conference_create: is_creating,
            pending_conference_join: None,
            pending_outbound_block: None,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            ssrc_levels: HashMap::new(),
            timers: HashMap::new(),
            e2e_timers: HashMap::new(),
            last_messages_prune_ms: now_ms,
            stars_poll_due_ms: None,
            last_stream_params_json: None,
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            snapshot: initial_snapshot,
        });
        self.instance.set_in_group_call(true);
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
        Ok(())
    }

    fn send_pending_conference_join(&mut self, now_ms: u64) {
        let prepared = self.active_group.as_mut().and_then(|group| {
            let pending = group.pending_conference_join.clone()?;
            let e2e = group.session.e2e_mut()?;
            let my_key = e2e.my_key();
            match e2e.make_join_block() {
                Ok(block) => Some(Ok((pending, my_key, block))),
                Err(err) => Some(Err(err.to_string())),
            }
        });
        match prepared {
            Some(Ok((pending, my_key, join_block))) => {
                self.spawn_join_conference_call(pending, my_key, join_block);
            }
            Some(Err(err)) => {
                self.toast_queue
                    .push(format!("Could not generate conference join block: {err}"));
                if let Some(group) = self.active_group.as_mut() {
                    group.pending_conference_join = None;
                    group.session.handle_join_fail("E2E_JOIN_BLOCK_FAILED");
                }
                self.process_group_actions(now_ms);
            }
            None => {}
        }
    }

    fn remove_conference_participants(&mut self, user_ids: &[u64], kick: bool, now_ms: u64) {
        let ids_set: BTreeSet<CallsUserId> = user_ids.iter().copied().map(CallsUserId).collect();
        if ids_set.is_empty() {
            return;
        }
        let prepared = self.active_group.as_mut().and_then(|group| {
            if !group.session.is_conference() {
                return None;
            }
            let call = group.session.input_call_handle();
            match group.session.make_conference_remove_block(&ids_set) {
                Ok(Some(block)) => Some(Ok((call, block))),
                Ok(None) => None,
                Err(err) => Some(Err(err.to_string())),
            }
        });
        match prepared {
            Some(Ok((call, block))) => {
                let ids_vec: Vec<CallsUserId> = ids_set.into_iter().collect();
                self.spawn_delete_conference_participants(call, ids_vec, block, !kick);
            }
            Some(Err(err)) => {
                self.toast_queue
                    .push(format!("Failed to update conference participants: {err}"));
            }
            None => {}
        }
        self.process_group_actions(now_ms);
    }

    fn start_new_group_call_session(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
        can_manage: bool,
        now_ms: u64,
    ) -> anyhow::Result<()> {
        self.start_group_call_session(
            group_peer_id,
            join_as,
            CallId::ZERO,
            0,
            can_manage,
            None,
            now_ms,
        )?;
        let random_id = self.next_random_id();
        if let Some(group) = self.active_group.as_mut() {
            group.session.start_create(random_id);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
        Ok(())
    }

    fn process_group_actions(&mut self, now_ms: u64) {
        for _ in 0..8 {
            let Some(group) = self.active_group.as_mut() else {
                break;
            };
            let actions = group.session.take_actions();
            if actions.is_empty() {
                break;
            }
            let mut deferred_rpcs = Vec::new();
            let mut finished = false;
            for action in actions {
                match action {
                    GroupCallAction::EmitJoinPayload => {
                        match group.engine.emit_join_payload_json(Vec::new()) {
                            Ok((ssrc, json)) => {
                                group.session.on_join_payload_ready(ssrc, json);
                            }
                            Err(err) => {
                                self.toast_queue
                                    .push(format!("Failed to build group join payload: {err}"));
                            }
                        }
                    }
                    GroupCallAction::EmitScreenJoinPayload => {
                        match group.engine.emit_join_payload_json(Vec::new()) {
                            Ok((ssrc, json)) => {
                                group.session.on_screen_join_payload_ready(ssrc, json);
                            }
                            Err(err) => {
                                self.toast_queue
                                    .push(format!("Failed to build screen join payload: {err}"));
                            }
                        }
                    }
                    GroupCallAction::SetJoinResponsePayload(json) => {
                        if let Err(err) = group.engine.set_join_response_payload(&json) {
                            self.toast_queue
                                .push(format!("Group media engine error: {err}"));
                        }
                    }
                    GroupCallAction::SetScreenJoinResponsePayload(_) => {}
                    GroupCallAction::SetInstanceMuted(muted) => {
                        group.engine.set_muted(muted);
                    }
                    GroupCallAction::SetInstanceVolume { ssrc, volume } => {
                        group.engine.set_ssrc_volume(ssrc, volume);
                    }
                    GroupCallAction::RemoveInstanceSsrcs(ssrcs) => {
                        for s in &ssrcs {
                            group.ssrc_levels.remove(s);
                            group.engine.unregister_ssrc(*s);
                        }
                        group.engine.remove_ssrcs(ssrcs);
                    }
                    GroupCallAction::SetInstanceMode { mode, .. } => match mode {
                        InstanceMode::None => {
                            group.engine.stop();
                        }
                        InstanceMode::Rtc => {
                            if let Err(err) = group.engine.start() {
                                self.toast_queue
                                    .push(format!("Group media engine start failed: {err}"));
                            }
                        }
                        InstanceMode::Stream => {
                            let stream_json = group
                                .last_stream_params_json
                                .as_deref()
                                .unwrap_or(r#"{"stream":true}"#);
                            if let Err(err) = group.engine.set_join_response_payload(stream_json) {
                                self.toast_queue
                                    .push(format!("Group broadcast stream start failed: {err}"));
                            }
                        }
                    },
                    GroupCallAction::PlaySound(sound) => {
                        self.sound_queue
                            .push(BackendSoundEvent::GroupCallSound(sound));
                    }
                    GroupCallAction::AllowedToSpeak => {
                        self.sound_queue.push(BackendSoundEvent::GroupCallSound(
                            GroupCallSound::AllowedToSpeak,
                        ));
                    }
                    GroupCallAction::SetTimer {
                        kind,
                        duration_ms,
                        repeating,
                    } => {
                        let interval = repeating.then_some(duration_ms);
                        group
                            .timers
                            .insert(kind, (now_ms.saturating_add(duration_ms), interval));
                    }
                    GroupCallAction::CancelTimer(kind) => {
                        group.timers.remove(&kind);
                    }
                    GroupCallAction::NotifyFinished | GroupCallAction::NotifyFailed => {
                        group.engine.stop();
                        finished = true;
                    }
                    GroupCallAction::ParticipantUpdated(upd) => {
                        sync_group_ssrc_users(group, &upd);
                    }
                    GroupCallAction::SetRequestedVideoChannels(_)
                    | GroupCallAction::LevelUpdate(_)
                    | GroupCallAction::StateChanged { .. }
                    | GroupCallAction::MuteStateChanged { .. } => {}
                    rpc => {
                        deferred_rpcs.push(rpc);
                    }
                }
            }

            let (current_mute, current_join_as) = self
                .active_group
                .as_ref()
                .map_or((MuteState::Muted, tg_calls::types::PeerId::ZERO), |g| {
                    (g.session.muted(), g.session.join_as())
                });
            for rpc in deferred_rpcs {
                self.dispatch_group_rpc(rpc, current_mute, current_join_as, now_ms);
            }

            if finished {
                self.active_group = None;
                self.instance.set_in_group_call(false);
                break;
            }
        }
        self.process_group_e2e_actions(now_ms);
        self.process_group_message_actions(now_ms);
    }

    fn process_group_e2e_actions(&mut self, now_ms: u64) {
        for _ in 0..8 {
            let (call_handle, actions) = match self.active_group.as_mut() {
                Some(group) => (
                    group.session.input_call_handle(),
                    group.session.take_e2e_actions(),
                ),
                None => break,
            };
            if actions.is_empty() {
                break;
            }
            let mut subchain_fetches = Vec::new();
            let mut outbound_blocks = Vec::new();
            let mut stale_removals: Option<(InputGroupCall, Vec<CallsUserId>, Vec<u8>)> = None;
            let mut need_rejoin = false;

            if let Some(group) = self.active_group.as_mut() {
                for action in actions {
                    match action {
                        E2eAction::RequestSubchainBlocks {
                            subchain,
                            offset,
                            limit,
                        } => {
                            if !group.session.id().is_zero() {
                                group.session.begin_subchain_request(subchain);
                                subchain_fetches.push((
                                    call_handle.clone(),
                                    subchain,
                                    offset,
                                    limit,
                                ));
                            }
                        }
                        E2eAction::SendOutboundBlock(block) => {
                            if !group.session.id().is_zero() {
                                outbound_blocks.push((call_handle.clone(), block));
                            } else {
                                group.pending_outbound_block = Some(block);
                            }
                        }
                        E2eAction::ParticipantsSetChanged(_) => {
                            if group.participants_loaded
                                && !group.session.id().is_zero()
                                && matches!(
                                    group.session.state(),
                                    tg_calls::GroupCallState::Joined
                                        | tg_calls::GroupCallState::Connecting
                                )
                            {
                                let stale = group.session.find_stale_conference_participants();
                                if !stale.is_empty()
                                    && let Ok(Some(block)) =
                                        group.session.make_conference_remove_block(&stale)
                                {
                                    stale_removals = Some((
                                        call_handle.clone(),
                                        stale.into_iter().collect(),
                                        block,
                                    ));
                                }
                            }
                        }
                        E2eAction::EmojiHashChanged(_) => {}
                        E2eAction::SetTimer {
                            subchain,
                            kind,
                            duration_ms,
                        } => {
                            group
                                .e2e_timers
                                .insert((subchain, kind), now_ms.saturating_add(duration_ms));
                        }
                        E2eAction::CancelTimer { subchain, kind } => {
                            group.e2e_timers.remove(&(subchain, kind));
                        }
                        E2eAction::Failed(_) => {
                            need_rejoin = true;
                        }
                    }
                }
            }

            for (call, subchain, offset, limit) in subchain_fetches {
                self.spawn_get_chain_blocks(call, subchain, offset, limit);
            }
            for (call, block) in outbound_blocks {
                self.spawn_send_conference_broadcast(call, block);
            }
            if let Some((call, ids, block)) = stale_removals {
                self.spawn_delete_conference_participants(call, ids, block, true);
            }
            if need_rejoin {
                self.toast_queue
                    .push("End-to-end encryption verification failed; reconnecting...".to_owned());
                if let Some(group) = self.active_group.as_mut() {
                    group.session.rejoin();
                }
            }
        }
    }

    fn process_group_message_actions(&mut self, _now_ms: u64) {
        let (call_handle, actions) = match self.active_group.as_mut() {
            Some(group) => (
                group.session.input_call_handle(),
                group.session.take_message_actions(),
            ),
            None => return,
        };
        if actions.is_empty() {
            return;
        }
        for action in actions {
            match action {
                GroupMessageAction::SendPlainMessage {
                    random_id,
                    text,
                    entities,
                    allow_paid_stars,
                    send_as,
                } => {
                    let paid_reaction = text.is_empty() && allow_paid_stars > 0;
                    let send_as_input = resolve_input_peer_for_calls(&self.runtime, send_as);
                    self.spawn_send_group_call_message(
                        call_handle.clone(),
                        random_id,
                        text,
                        entities,
                        allow_paid_stars,
                        send_as_input,
                        paid_reaction,
                    );
                }
                GroupMessageAction::SendEncryptedMessage {
                    random_id,
                    encrypted_message,
                } => {
                    self.spawn_send_group_call_encrypted_message(
                        call_handle.clone(),
                        random_id,
                        encrypted_message,
                    );
                }
                GroupMessageAction::DeleteMessages {
                    message_ids,
                    report_spam,
                } => {
                    self.spawn_delete_group_call_messages(
                        call_handle.clone(),
                        message_ids,
                        report_spam,
                    );
                }
                GroupMessageAction::DeleteParticipantMessages {
                    participant,
                    report_spam,
                } => {
                    let participant_input =
                        resolve_input_peer_for_calls(&self.runtime, participant);
                    self.spawn_delete_group_call_participant_messages(
                        call_handle.clone(),
                        participant_input,
                        report_spam,
                    );
                }
                GroupMessageAction::RequestStarsStats => {
                    self.spawn_get_group_call_stars(call_handle.clone());
                }
                GroupMessageAction::MessageIdUpdated { .. }
                | GroupMessageAction::MessagesChanged
                | GroupMessageAction::StarsChanged { .. } => {}
            }
        }
    }

    fn dispatch_group_rpc(
        &mut self,
        action: GroupCallAction,
        current_mute: MuteState,
        current_join_as: tg_calls::types::PeerId,
        now_ms: u64,
    ) {
        match action {
            GroupCallAction::CreateGroupCall {
                peer,
                random_id,
                schedule_date,
                rtmp_stream,
            } => {
                let input_peer = resolve_input_peer_for_calls(&self.runtime, peer);
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::CreateGroupCall {
                        rtmp_stream,
                        peer: input_peer,
                        random_id,
                        title: None,
                        schedule_date,
                    };
                    let res = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await;
                    let ev = match res {
                        Ok(Ok(updates)) => LiveCallInboundEvent::GroupCreateResult {
                            result: Ok(updates),
                        },
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            LiveCallInboundEvent::GroupCreateResult {
                                result: Err(invocation_error_name(&inv)),
                            }
                        }
                        Err(_) => LiveCallInboundEvent::GroupCreateResult {
                            result: Err("TIMEOUT".to_owned()),
                        },
                    };
                    let _ = tx.send(ev);
                    bridge.notify_ui();
                });
            }
            GroupCallAction::JoinGroupCall {
                call,
                join_as,
                muted,
                video_stopped,
                invite_hash,
                params_json,
            } => {
                let is_conference = self
                    .active_group
                    .as_ref()
                    .is_some_and(|g| g.session.is_conference());
                if is_conference {
                    let is_create = self
                        .active_group
                        .as_mut()
                        .is_some_and(|g| std::mem::take(&mut g.pending_conference_create));
                    if is_create {
                        let random_id = self.next_random_id();
                        let prepared = self.active_group.as_mut().and_then(|g| {
                            let e2e = g.session.e2e_mut()?;
                            let pk = e2e.my_key();
                            Some(e2e.make_join_block().map(|blk| (pk, blk)))
                        });
                        match prepared {
                            Some(Ok((public_key, zero_block))) => {
                                self.spawn_create_conference_call(
                                    random_id,
                                    muted,
                                    video_stopped,
                                    public_key,
                                    zero_block,
                                    params_json,
                                    current_mute,
                                );
                            }
                            Some(Err(err)) => {
                                self.toast_queue.push(format!(
                                    "Could not generate conference zero block: {err}"
                                ));
                                if let Some(g) = self.active_group.as_mut() {
                                    g.session.handle_join_fail("E2E_ZERO_BLOCK_FAILED");
                                }
                            }
                            None => {}
                        }
                        return;
                    }

                    let has_last_block = self
                        .active_group
                        .as_ref()
                        .and_then(|g| g.session.e2e())
                        .is_some_and(E2eCall::has_last_block0);
                    let pending = PendingConferenceJoin {
                        call: call.clone(),
                        join_as,
                        muted,
                        video_stopped,
                        invite_hash,
                        params_json,
                        was_mute_state: current_mute,
                    };
                    if let Some(g) = self.active_group.as_mut() {
                        g.pending_conference_join = Some(pending);
                    }
                    if has_last_block {
                        self.send_pending_conference_join(now_ms);
                    } else {
                        self.spawn_get_last_chain_block(call);
                    }
                    return;
                }

                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let join_as_input = resolve_input_peer_for_calls(&self.runtime, join_as);
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::JoinGroupCall {
                        muted,
                        video_stopped,
                        call: tl_call,
                        join_as: join_as_input,
                        invite_hash,
                        public_key: None,
                        block: None,
                        params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: params_json,
                        }),
                    };
                    let res = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await;
                    let ev = match res {
                        Ok(Ok(updates)) => LiveCallInboundEvent::GroupJoinResult {
                            result: Ok(updates),
                            was_mute_state: current_mute,
                            was_video_stopped: video_stopped,
                        },
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            LiveCallInboundEvent::GroupJoinResult {
                                result: Err(invocation_error_name(&inv)),
                                was_mute_state: current_mute,
                                was_video_stopped: video_stopped,
                            }
                        }
                        Err(_) => LiveCallInboundEvent::GroupJoinResult {
                            result: Err("TIMEOUT".to_owned()),
                            was_mute_state: current_mute,
                            was_video_stopped: video_stopped,
                        },
                    };
                    let _ = tx.send(ev);
                    bridge.notify_ui();
                });
            }
            GroupCallAction::LeaveGroupCall { call, source } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::LeaveGroupCall {
                        call: tl_call,
                        source: source as i32,
                    };
                    let updates =
                        match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                            Ok(Ok(u)) => Some(u),
                            Ok(Err(err)) => {
                                let inv: InvocationError = err;
                                debug!(error = %inv, "phone.leaveGroupCall failed");
                                None
                            }
                            Err(_) => None,
                        };
                    let _ = tx.send(LiveCallInboundEvent::GroupLeaveDone { updates });
                    bridge.notify_ui();
                });
            }
            GroupCallAction::DiscardGroupCall { call } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::DiscardGroupCall { call: tl_call };
                    let updates =
                        match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                            Ok(Ok(u)) => Some(u),
                            Ok(Err(err)) => {
                                let inv: InvocationError = err;
                                debug!(error = %inv, "phone.discardGroupCall failed");
                                None
                            }
                            Err(_) => None,
                        };
                    let _ = tx.send(LiveCallInboundEvent::GroupDiscardDone { updates });
                    bridge.notify_ui();
                });
            }
            GroupCallAction::CheckGroupCall { call, sources } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::CheckGroupCall {
                        call: tl_call,
                        sources: sources.into_iter().map(|s| s as i32).collect(),
                    };
                    let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                        Ok(Ok(active)) => LiveCallInboundEvent::GroupCheckResult(Ok(active
                            .into_iter()
                            .map(|s| s as u32)
                            .collect())),
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            LiveCallInboundEvent::GroupCheckResult(Err(invocation_error_name(&inv)))
                        }
                        Err(_) => LiveCallInboundEvent::GroupCheckResult(Err("TIMEOUT".to_owned())),
                    };
                    let _ = tx.send(ev);
                    bridge.notify_ui();
                });
            }
            GroupCallAction::EditGroupCallParticipant {
                call,
                participant,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
            } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let is_self = participant == current_join_as;
                let part_input = resolve_input_peer_for_calls(&self.runtime, participant);
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::EditGroupCallParticipant {
                        call: tl_call,
                        participant: part_input,
                        muted,
                        volume,
                        raise_hand,
                        video_stopped,
                        video_paused,
                        presentation_paused,
                    };
                    match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                        Ok(Ok(updates)) => {
                            if is_self {
                                let _ = tx.send(LiveCallInboundEvent::GroupSelfUpdateDone {
                                    updates: Some(updates),
                                    error: None,
                                });
                            } else {
                                let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                            }
                        }
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            let name = invocation_error_name(&inv);
                            if is_self {
                                let _ = tx.send(LiveCallInboundEvent::GroupSelfUpdateDone {
                                    updates: None,
                                    error: Some(name),
                                });
                            } else {
                                debug!(error = %inv, "phone.editGroupCallParticipant failed");
                            }
                        }
                        Err(_) => {
                            if is_self {
                                let _ = tx.send(LiveCallInboundEvent::GroupSelfUpdateDone {
                                    updates: None,
                                    error: Some("TIMEOUT".to_owned()),
                                });
                            }
                        }
                    }
                    bridge.notify_ui();
                });
            }
            GroupCallAction::EditGroupCallTitle { call, title } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::EditGroupCallTitle {
                        call: tl_call,
                        title,
                    };
                    match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                        Ok(Ok(updates)) => {
                            let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                            bridge.notify_ui();
                        }
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            let _ = tx.send(LiveCallInboundEvent::Toast(format!(
                                "Title update failed: {}",
                                invocation_error_name(&inv)
                            )));
                            bridge.notify_ui();
                        }
                        Err(_) => {
                            let _ = tx.send(LiveCallInboundEvent::Toast(
                                "Title update timed out.".to_owned(),
                            ));
                            bridge.notify_ui();
                        }
                    }
                });
            }
            GroupCallAction::ToggleGroupCallRecord {
                call,
                start,
                title,
                video,
                video_portrait,
            } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::ToggleGroupCallRecord {
                        start,
                        video,
                        call: tl_call,
                        title,
                        video_portrait: video.then_some(video_portrait),
                    };
                    match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                        Ok(Ok(updates)) => {
                            let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                            bridge.notify_ui();
                        }
                        Ok(Err(err)) => {
                            let inv: InvocationError = err;
                            let _ = tx.send(LiveCallInboundEvent::Toast(format!(
                                "Recording update failed: {}",
                                invocation_error_name(&inv)
                            )));
                            bridge.notify_ui();
                        }
                        Err(_) => {
                            let _ = tx.send(LiveCallInboundEvent::Toast(
                                "Recording update timed out.".to_owned(),
                            ));
                            bridge.notify_ui();
                        }
                    }
                });
            }
            GroupCallAction::JoinGroupCallPresentation { call, params_json } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::JoinGroupCallPresentation {
                        call: tl_call,
                        params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: params_json,
                        }),
                    };
                    if let Ok(Ok(updates)) =
                        tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
                    {
                        let _ = tx.send(LiveCallInboundEvent::GroupScreenJoinDone {
                            updates: Some(updates),
                        });
                        bridge.notify_ui();
                    }
                });
            }
            GroupCallAction::LeaveGroupCallPresentation { call } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::LeaveGroupCallPresentation { call: tl_call };
                    let updates = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req))
                        .await
                        .ok()
                        .and_then(Result::ok);
                    let _ = tx.send(LiveCallInboundEvent::GroupScreenLeaveDone { updates });
                    bridge.notify_ui();
                });
            }
            GroupCallAction::ResolveUnknownSsrcs { call, ssrcs } => {
                let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
                    return;
                };
                let rpc = self.runtime.rpc().clone();
                let session_arc = Arc::clone(self.runtime.session());
                let tx = self.inbound_tx.clone();
                let bridge = Arc::clone(&self.bridge_state);
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::phone::GetGroupParticipants {
                        call: tl_call,
                        ids: Vec::new(),
                        sources: ssrcs.iter().map(|&s| s as i32).collect(),
                        offset: String::new(),
                        limit: ssrcs.len().max(1) as i32,
                    };
                    if let Ok(Ok(tl_enums::phone::GroupParticipants::GroupParticipants(resp))) =
                        tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
                    {
                        if !resp.users.is_empty() || !resp.chats.is_empty() {
                            let mut s = session_arc.write().await;
                            apply_tl_users_and_chats(&mut s, &resp.users, &resp.chats);
                        }
                        let participants: Vec<GroupCallParticipantData> =
                            resp.participants.into_iter().map(Into::into).collect();
                        let _ = tx.send(LiveCallInboundEvent::GroupResolvedSsrcs {
                            participants,
                            resolved_ssrcs: ssrcs,
                        });
                        bridge.notify_ui();
                    }
                });
            }
            GroupCallAction::SendSpeakingAction { peer } => {
                let input_peer = resolve_input_peer_for_calls(&self.runtime, peer);
                let rpc = self.runtime.rpc().clone();
                self.runtime.tokio_handle().spawn(async move {
                    let req = tl_fns::messages::SetTyping {
                        peer: input_peer,
                        top_msg_id: None,
                        action: tl_enums::SendMessageAction::SpeakingInGroupCallAction,
                    };
                    let _ = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await;
                });
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------------------
    // Async RPC spawners (1:1 and Group Call metadata)
    // -------------------------------------------------------------------------

    fn spawn_get_call_config(&self) {
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ok = tokio::time::timeout(
                CALL_RPC_TIMEOUT,
                rpc.invoke(&tl_fns::phone::GetCallConfig {}),
            )
            .await
            .is_ok_and(|r| r.is_ok());
            let _ = tx.send(LiveCallInboundEvent::CallConfigResult { ok });
            bridge.notify_ui();
        });
    }

    fn spawn_get_dh_config(&self, version: i32, random_length: i32) {
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::messages::GetDhConfig {
                version,
                random_length,
            };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(dh)) => LiveCallInboundEvent::DhConfigResult(Ok(dh.into())),
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::DhConfigResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::DhConfigResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_request_call(
        &self,
        video: bool,
        user_id: CallsUserId,
        random_id: i32,
        g_a_hash: [u8; 32],
        protocol: tg_calls::types::PhoneCallProtocol,
    ) {
        let input_user = resolve_input_user(&self.runtime, user_id);
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::RequestCall {
                video,
                user_id: input_user,
                random_id,
                g_a_hash: g_a_hash.to_vec(),
                protocol: protocol.into(),
            };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(tl_enums::phone::PhoneCall::PhoneCall(pc))) => {
                    if !pc.users.is_empty() {
                        let mut s = session_arc.write().await;
                        apply_tl_users_and_chats(&mut s, &pc.users, &[]);
                    }
                    match PhoneCallUpdate::try_from(pc.phone_call) {
                        Ok(upd) => LiveCallInboundEvent::RequestCallResult(Ok(upd)),
                        Err(e) => LiveCallInboundEvent::RequestCallResult(Err(e.to_string())),
                    }
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::RequestCallResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::RequestCallResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_received_call(&self, peer: tg_calls::types::InputPhoneCall) {
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::ReceivedCall { peer: peer.into() };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(true)) => LiveCallInboundEvent::ReceivedCallResult(Ok(())),
                Ok(Ok(false)) => {
                    LiveCallInboundEvent::ReceivedCallResult(Err("CALL_NOT_RECEIVED".to_owned()))
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::ReceivedCallResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::ReceivedCallResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_accept_call(
        &self,
        peer: tg_calls::types::InputPhoneCall,
        g_b: Vec<u8>,
        protocol: tg_calls::types::PhoneCallProtocol,
    ) {
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::AcceptCall {
                peer: peer.into(),
                g_b,
                protocol: protocol.into(),
            };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(tl_enums::phone::PhoneCall::PhoneCall(pc))) => {
                    if !pc.users.is_empty() {
                        let mut s = session_arc.write().await;
                        apply_tl_users_and_chats(&mut s, &pc.users, &[]);
                    }
                    match PhoneCallUpdate::try_from(pc.phone_call) {
                        Ok(upd) => LiveCallInboundEvent::AcceptCallResult(Ok(upd)),
                        Err(e) => LiveCallInboundEvent::AcceptCallResult(Err(e.to_string())),
                    }
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::AcceptCallResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::AcceptCallResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_confirm_call(
        &self,
        peer: tg_calls::types::InputPhoneCall,
        g_a: Vec<u8>,
        key_fingerprint: u64,
        protocol: tg_calls::types::PhoneCallProtocol,
    ) {
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::ConfirmCall {
                peer: peer.into(),
                g_a,
                key_fingerprint: key_fingerprint as i64,
                protocol: protocol.into(),
            };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(tl_enums::phone::PhoneCall::PhoneCall(pc))) => {
                    if !pc.users.is_empty() {
                        let mut s = session_arc.write().await;
                        apply_tl_users_and_chats(&mut s, &pc.users, &[]);
                    }
                    match PhoneCallUpdate::try_from(pc.phone_call) {
                        Ok(upd) => LiveCallInboundEvent::ConfirmCallResult(Ok(upd)),
                        Err(e) => LiveCallInboundEvent::ConfirmCallResult(Err(e.to_string())),
                    }
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::ConfirmCallResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::ConfirmCallResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_send_signaling_data(&self, peer: tg_calls::types::InputPhoneCall, data: Vec<u8>) {
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::SendSignalingData {
                peer: peer.into(),
                data,
            };
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(ok)) => LiveCallInboundEvent::SendSignalingResult(Ok(ok)),
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::SendSignalingResult(Err(invocation_error_name(&inv)))
                }
                Err(_) => LiveCallInboundEvent::SendSignalingResult(Err("TIMEOUT".to_owned())),
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_discard_call(
        &self,
        video: bool,
        peer: tg_calls::types::InputPhoneCall,
        duration: i32,
        reason: tg_calls::types::PhoneCallDiscardReason,
        connection_id: i64,
        notify_current_call: bool,
    ) {
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::DiscardCall {
                video,
                peer: peer.into(),
                duration,
                reason: reason.into(),
                connection_id,
            };
            let updates = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(u)) => Some(u),
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    debug!(error = %inv, "phone.discardCall failed");
                    None
                }
                Err(_) => None,
            };
            if notify_current_call {
                let _ = tx.send(LiveCallInboundEvent::DiscardCallResult { updates });
                bridge.notify_ui();
            }
        });
    }

    fn spawn_set_call_rating(
        &self,
        user_initiative: bool,
        peer: tg_calls::types::InputPhoneCall,
        rating: i32,
        comment: String,
    ) {
        let rpc = self.runtime.rpc().clone();
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::SetCallRating {
                user_initiative,
                peer: peer.into(),
                rating,
                comment,
            };
            if let Ok(Err(err)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                let inv: InvocationError = err;
                debug!(error = %inv, "phone.setCallRating failed");
            }
        });
    }

    fn spawn_save_call_debug(&self, peer: tg_calls::types::InputPhoneCall, debug_json: String) {
        let rpc = self.runtime.rpc().clone();
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::SaveCallDebug {
                peer: peer.into(),
                debug: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: debug_json }),
            };
            if let Ok(Err(err)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                let inv: InvocationError = err;
                debug!(error = %inv, "phone.saveCallDebug failed");
            }
        });
    }

    fn spawn_get_group_call(&self, call: InputGroupCall, limit: i32) {
        let call_id = match &call {
            InputGroupCall::Id { id, .. } => *id,
            _ => CallId::ZERO,
        };
        let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::GetGroupCall {
                call: tl_call,
                limit,
            };
            if let Ok(Ok(tl_enums::phone::GroupCall::GroupCall(gc))) =
                tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                if !gc.users.is_empty() || !gc.chats.is_empty() {
                    let mut s = session_arc.write().await;
                    apply_tl_users_and_chats(&mut s, &gc.users, &gc.chats);
                }
                let response = PhoneGroupCallResponse::from(gc);
                let _ = tx.send(LiveCallInboundEvent::GroupFullCallLoaded { call_id, response });
                bridge.notify_ui();
            }
        });
    }

    fn spawn_get_group_participants_page(&self, call: InputGroupCall, offset: String) {
        let Ok(tl_call) = tl_enums::InputGroupCall::try_from(call) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::GetGroupParticipants {
                call: tl_call,
                ids: Vec::new(),
                sources: Vec::new(),
                offset,
                limit: GROUP_CALL_PARTICIPANTS_LIMIT,
            };
            if let Ok(Ok(tl_enums::phone::GroupParticipants::GroupParticipants(resp))) =
                tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                if !resp.users.is_empty() || !resp.chats.is_empty() {
                    let mut s = session_arc.write().await;
                    apply_tl_users_and_chats(&mut s, &resp.users, &resp.chats);
                }
                let participants: Vec<GroupCallParticipantData> =
                    resp.participants.into_iter().map(Into::into).collect();
                let _ = tx.send(LiveCallInboundEvent::GroupParticipantsPage { participants });
                bridge.notify_ui();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_create_conference_call(
        &self,
        random_id: i32,
        muted: bool,
        video_stopped: bool,
        public_key: PublicKey,
        zero_block: Vec<u8>,
        params_json: String,
        was_mute_state: MuteState,
    ) {
        let req = build_create_conference_call(
            random_id,
            muted,
            video_stopped,
            public_key,
            zero_block,
            params_json,
        );
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupConferenceCreateResult {
                    result: Ok(updates),
                    was_mute_state,
                    was_video_stopped: video_stopped,
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupConferenceCreateResult {
                        result: Err(invocation_error_name(&inv)),
                        was_mute_state,
                        was_video_stopped: video_stopped,
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupConferenceCreateResult {
                    result: Err("TIMEOUT".to_owned()),
                    was_mute_state,
                    was_video_stopped: video_stopped,
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_join_conference_call(
        &self,
        pending: PendingConferenceJoin,
        public_key: PublicKey,
        join_block: Vec<u8>,
    ) {
        let join_as_input = resolve_input_peer_for_calls(&self.runtime, pending.join_as);
        let Ok(req) = build_join_conference_call(
            pending.call,
            join_as_input,
            pending.muted,
            pending.video_stopped,
            pending.invite_hash,
            public_key,
            join_block,
            pending.params_json,
        ) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupJoinResult {
                    result: Ok(updates),
                    was_mute_state: pending.was_mute_state,
                    was_video_stopped: pending.video_stopped,
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupJoinResult {
                        result: Err(invocation_error_name(&inv)),
                        was_mute_state: pending.was_mute_state,
                        was_video_stopped: pending.video_stopped,
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupJoinResult {
                    result: Err("TIMEOUT".to_owned()),
                    was_mute_state: pending.was_mute_state,
                    was_video_stopped: pending.video_stopped,
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_get_last_chain_block(&self, call: InputGroupCall) {
        let Ok(req) = build_get_last_chain_block(call) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupLastChainBlockLoaded {
                    result: Ok(updates),
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupLastChainBlockLoaded {
                        result: Err(invocation_error_name(&inv)),
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupLastChainBlockLoaded {
                    result: Err("TIMEOUT".to_owned()),
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_get_chain_blocks(&self, call: InputGroupCall, subchain: i32, offset: i32, limit: i32) {
        let Ok(req) = build_get_chain_blocks(call, subchain, offset, limit) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupChainBlocksResponse {
                    subchain,
                    result: Ok(updates),
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupChainBlocksResponse {
                        subchain,
                        result: Err(invocation_error_name(&inv)),
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupChainBlocksResponse {
                    subchain,
                    result: Err("TIMEOUT".to_owned()),
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_send_conference_broadcast(&self, call: InputGroupCall, block: Vec<u8>) {
        let Ok(req) = build_send_conference_broadcast(call, block.clone()) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupBroadcastSent {
                    block,
                    result: Ok(updates),
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupBroadcastSent {
                        block,
                        result: Err(invocation_error_name(&inv)),
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupBroadcastSent {
                    block,
                    result: Err("TIMEOUT".to_owned()),
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_delete_conference_participants(
        &self,
        call: InputGroupCall,
        user_ids: Vec<CallsUserId>,
        remove_block: Vec<u8>,
        removing_stale: bool,
    ) {
        let Ok(req) =
            build_delete_conference_participants(call, &user_ids, remove_block, removing_stale)
        else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => {
                    let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                    bridge.notify_ui();
                }
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    debug!(error = %inv, "phone.deleteConferenceCallParticipants failed");
                }
                Err(_) => {}
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_send_group_call_message(
        &self,
        call: InputGroupCall,
        random_id: u64,
        text: String,
        entities: Vec<tg_calls::types::GroupCallMessageEntity>,
        allow_paid_stars: i64,
        send_as: tl_enums::InputPeer,
        paid_reaction: bool,
    ) {
        let Ok(req) = build_send_group_call_message(
            call,
            random_id,
            text,
            &entities,
            (allow_paid_stars > 0).then_some(allow_paid_stars),
            Some(send_as),
        ) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(updates)) => LiveCallInboundEvent::GroupMessageSent {
                    random_id,
                    paid_reaction,
                    result: Ok(updates),
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupMessageSent {
                        random_id,
                        paid_reaction,
                        result: Err(invocation_error_name(&inv)),
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupMessageSent {
                    random_id,
                    paid_reaction,
                    result: Err("TIMEOUT".to_owned()),
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_send_group_call_encrypted_message(
        &self,
        call: InputGroupCall,
        random_id: u64,
        encrypted_message: Vec<u8>,
    ) {
        let Ok(req) = build_send_group_call_encrypted_message(call, encrypted_message) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let ev = match tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(ok)) => LiveCallInboundEvent::GroupEncryptedMessageSent {
                    random_id,
                    result: Ok(ok),
                },
                Ok(Err(err)) => {
                    let inv: InvocationError = err;
                    LiveCallInboundEvent::GroupEncryptedMessageSent {
                        random_id,
                        result: Err(invocation_error_name(&inv)),
                    }
                }
                Err(_) => LiveCallInboundEvent::GroupEncryptedMessageSent {
                    random_id,
                    result: Err("TIMEOUT".to_owned()),
                },
            };
            let _ = tx.send(ev);
            bridge.notify_ui();
        });
    }

    fn spawn_delete_group_call_messages(
        &self,
        call: InputGroupCall,
        message_ids: Vec<i32>,
        report_spam: bool,
    ) {
        let Ok(req) = build_delete_group_call_messages(call, message_ids, report_spam) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            if let Ok(Ok(updates)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                bridge.notify_ui();
            }
        });
    }

    fn spawn_delete_group_call_participant_messages(
        &self,
        call: InputGroupCall,
        participant: tl_enums::InputPeer,
        report_spam: bool,
    ) {
        let Ok(req) = build_delete_group_call_participant_messages(call, participant, report_spam)
        else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            if let Ok(Ok(updates)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                bridge.notify_ui();
            }
        });
    }

    fn spawn_get_group_call_stars(&self, call: InputGroupCall) {
        let Ok(req) = build_get_group_call_stars(call) else {
            return;
        };
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            if let Ok(Ok(tl_enums::phone::GroupCallStars::GroupCallStars(resp))) =
                tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                if !resp.users.is_empty() || !resp.chats.is_empty() {
                    let mut s = session_arc.write().await;
                    apply_tl_users_and_chats(&mut s, &resp.users, &resp.chats);
                }
                let converted = PhoneGroupCallStarsResponse::from(resp);
                let _ = tx.send(LiveCallInboundEvent::GroupStarsLoaded(converted));
                bridge.notify_ui();
            }
        });
    }

    fn spawn_lookup_and_join_group_call(&self, group_peer_id: PeerId, join_as: PeerId) {
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        let data_peer = to_data_peer(group_peer_id);

        self.runtime.tokio_handle().spawn(async move {
            let (kind, input_channel) = {
                let s = session_arc.read().await;
                let ch = s.peer(data_peer).and_then(|p| p.as_channel()).map(|c| {
                    tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                        channel_id: c.channel_id().0 as i64,
                        access_hash: c.access_hash as i64,
                    })
                });
                (group_peer_id.kind(), ch)
            };

            let full_res = match kind {
                PeerKind::Channel => {
                    let channel = input_channel.unwrap_or_else(|| {
                        tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                            channel_id: group_peer_id.bare() as i64,
                            access_hash: 0,
                        })
                    });
                    tokio::time::timeout(
                        CALL_RPC_TIMEOUT,
                        rpc.invoke(&tl_fns::channels::GetFullChannel { channel }),
                    )
                    .await
                }
                PeerKind::Group => {
                    tokio::time::timeout(
                        CALL_RPC_TIMEOUT,
                        rpc.invoke(&tl_fns::messages::GetFullChat {
                            chat_id: group_peer_id.bare() as i64,
                        }),
                    )
                    .await
                }
                PeerKind::User => {
                    let _ = tx.send(LiveCallInboundEvent::Toast(
                        "Group calls require a group or channel peer.".to_owned(),
                    ));
                    bridge.notify_ui();
                    return;
                }
            };

            let mut found_call = None;
            let mut can_manage = false;
            if let Ok(Ok(tl_enums::messages::ChatFull::ChatFull(full))) = full_res {
                if !full.users.is_empty() || !full.chats.is_empty() {
                    let mut s = session_arc.write().await;
                    apply_tl_users_and_chats(&mut s, &full.users, &full.chats);
                }
                let input_call = match full.full_chat {
                    tl_enums::ChatFull::ChatFull(cf) => cf.call,
                    tl_enums::ChatFull::ChannelFull(cf) => {
                        can_manage = cf.can_view_stats;
                        cf.call
                    }
                    _ => None,
                };
                if let Some(tl_enums::InputGroupCall::InputGroupCall(ic)) = input_call {
                    found_call = Some((CallId::from_tl(ic.id), ic.access_hash));
                }
            }

            let _ = tx.send(LiveCallInboundEvent::GroupLookupResolved {
                group_peer_id,
                join_as,
                found_call,
                can_manage,
            });
            bridge.notify_ui();
        });
    }

    fn spawn_toggle_group_settings(
        &self,
        join_muted: Option<bool>,
        messages_enabled: Option<bool>,
    ) {
        let Some(group) = self.active_group.as_ref() else {
            return;
        };
        let id = group.session.id();
        if id.is_zero() {
            return;
        }
        let tl_call = tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: id.to_tl(),
            access_hash: group.session.access_hash(),
        });
        let rpc = self.runtime.rpc().clone();
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::phone::ToggleGroupCallSettings {
                reset_invite_hash: false,
                call: tl_call,
                join_muted,
                messages_enabled,
                send_paid_messages_stars: None,
            };
            if let Ok(Ok(updates)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await
            {
                let _ = tx.send(LiveCallInboundEvent::GroupUpdatesOnly(updates));
                bridge.notify_ui();
            }
        });
    }

    fn spawn_fetch_recent_calls(&self) {
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let tx = self.inbound_tx.clone();
        let bridge = Arc::clone(&self.bridge_state);
        self.runtime.tokio_handle().spawn(async move {
            let req = tl_fns::messages::Search {
                peer: tl_enums::InputPeer::Empty,
                q: String::new(),
                from_id: None,
                saved_peer_id: None,
                saved_reaction: None,
                top_msg_id: None,
                filter: tl_enums::MessagesFilter::InputMessagesFilterPhoneCalls(
                    tl_types::InputMessagesFilterPhoneCalls { missed: false },
                ),
                min_date: 0,
                max_date: 0,
                offset_id: 0,
                add_offset: 0,
                limit: 50,
                max_id: 0,
                min_id: 0,
                hash: 0,
            };
            if let Ok(Ok(msgs)) = tokio::time::timeout(CALL_RPC_TIMEOUT, rpc.invoke(&req)).await {
                let entries = {
                    let mut s = session_arc.write().await;
                    parse_recent_calls_from_messages(&mut s, msgs, now_unix_i32())
                };
                let _ = tx.send(LiveCallInboundEvent::RecentCallsLoaded(entries));
                bridge.notify_ui();
            }
        });
    }

    // -------------------------------------------------------------------------
    // Media Engine Event Polling & Snapshot Refresh
    // -------------------------------------------------------------------------

    fn poll_call_media_events(&mut self, now_ms: u64) {
        let mut events = Vec::new();
        if let Some(engine) = self.call_engine.as_mut() {
            while let Some(ev) = CallMediaEngine::poll_event(engine) {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return;
        }
        for ev in events {
            match ev {
                MediaEngineEvent::StateChanged(st) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.handle_controller_state_change(st, now_ms);
                    }
                }
                MediaEngineEvent::SignalBarsChanged(bars) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.handle_controller_bar_count_change(bars);
                    }
                }
                MediaEngineEvent::SignalingData(bytes) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.emit_signaling_data(bytes);
                    }
                }
                MediaEngineEvent::RemoteMediaStateChanged { audio, video } => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.update_remote_media_state(audio, video);
                    }
                }
                MediaEngineEvent::RemoteBatteryChanged(battery) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.update_remote_battery_state(battery == RemoteBatteryState::Low);
                    }
                }
                MediaEngineEvent::AudioLevelChanged(level) => {
                    self.call_remote_audio_level = level.clamp(0.0, 1.0);
                }
                MediaEngineEvent::PreferredRelayChanged(relay_id) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.set_preferred_relay_id(relay_id);
                    }
                }
                MediaEngineEvent::RemoteVideoFrame(frame) => {
                    self.call_latest_video_frame = Some(frame);
                }
                MediaEngineEvent::RemoteVideoRotationChanged(_)
                | MediaEngineEvent::RemoteScreencastStateChanged(_)
                | MediaEngineEvent::KeyframeRequested
                | MediaEngineEvent::DataChannelMessage { .. } => {}
            }
        }
        self.process_instance_actions(now_ms);
    }

    fn poll_group_media_events(&mut self, now_ms: u64) {
        let mut events = Vec::new();
        if let Some(group) = self.active_group.as_mut() {
            while let Some(ev) = group.engine.poll_event() {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return;
        }
        if let Some(group) = self.active_group.as_mut() {
            for ev in events {
                match ev {
                    GroupMediaEvent::NetworkStateChanged {
                        connected,
                        transitioning_to_rtc,
                    } => {
                        group
                            .session
                            .set_instance_connected(connected, transitioning_to_rtc);
                    }
                    GroupMediaEvent::AudioLevelsUpdated(samples) => {
                        for s in &samples {
                            if s.ssrc == 0 {
                                group.local_audio_level = s.level.clamp(0.0, 1.0);
                            } else {
                                group.ssrc_levels.insert(s.ssrc, s.level.clamp(0.0, 1.0));
                            }
                        }
                        group.session.audio_levels_updated(&samples, now_ms);
                    }
                    GroupMediaEvent::ModeChanged(_) => {}
                    GroupMediaEvent::Error(msg) => {
                        self.toast_queue.push(msg);
                    }
                }
            }
        }
        self.process_group_actions(now_ms);
    }

    fn refresh_call_snapshot(&mut self, now_ms: u64) {
        let Some(call) = self.instance.current_call() else {
            self.call_snapshot = None;
            return;
        };
        let peer_id = PeerId::from_user(call.peer_user_id().raw());
        let peer = self.peer_info(peer_id);
        let duration_ms = call.duration_ms(now_ms);
        let ended_duration_secs = if call.rating_in_panel() || call.rating_requested() {
            let secs = i64::from(call.discarded_duration()).max((duration_ms / 1000) as i64);
            Some(secs.max(1))
        } else {
            None
        };
        let voip_stats = self
            .call_engine
            .as_ref()
            .map(MediaEngine::stats)
            .unwrap_or_default();

        self.call_snapshot = Some(ActiveCallSnapshot {
            peer,
            call_type: call.call_type(),
            state: call.state(),
            duration_ms,
            ended_duration_secs,
            muted: call.muted(),
            camera_state: call.outgoing_video_state(),
            screen_share_active: self.call_screen_share_active,
            remote_audio_state: call.remote_audio_state(),
            remote_video_state: call.remote_video_state(),
            remote_battery_state: call.remote_battery_state(),
            signal_bars: *call.signal_bars(),
            emoji_fingerprint: call.emoji_fingerprint(),
            is_conference_invite: call.is_conference_invite(),
            conference_supported: call.conference_supported(),
            rating_requested: call.rating_in_panel() || call.rating_requested(),
            submitted_rating: self.call_submitted_rating,
            voip_stats,
            remote_audio_level: self.call_remote_audio_level,
        });
    }

    fn refresh_group_snapshot(&mut self) {
        let Some(group) = self.active_group.as_mut() else {
            return;
        };
        let list = group.session.participants_list();
        let join_as = from_calls_peer_id(group.session.join_as());
        let member_ids: Vec<PeerId> = list
            .participants()
            .iter()
            .map(|p| from_calls_peer_id(p.peer_id))
            .collect();
        let peer_infos: Vec<CallPeerInfo> = read_session_sync(&self.runtime, |s| {
            member_ids
                .iter()
                .map(|&pid| peer_to_call_info(s, to_data_peer(pid)))
                .collect()
        });

        let mut members = Vec::with_capacity(list.participants().len());
        for (proto, peer) in list.participants().iter().zip(peer_infos) {
            let pid = from_calls_peer_id(proto.peer_id);
            let is_self = pid == join_as;
            let audio_level = if is_self {
                group.local_audio_level
            } else {
                group.ssrc_levels.get(&proto.ssrc).copied().unwrap_or(0.0)
            };
            let camera_frame_size = proto
                .video_params
                .as_ref()
                .filter(|v| !v.camera.is_empty())
                .map(|_| (1280, 720));
            let screen_frame_size = proto
                .video_params
                .as_ref()
                .filter(|v| !v.screen.is_empty())
                .map(|_| (1920, 1080));

            members.push(GroupMemberView {
                peer,
                participant: proto.clone(),
                is_self,
                audio_level,
                is_invited: false,
                is_calling: false,
                camera_frame_size,
                screen_frame_size,
            });
        }

        let large_ep = group.session.video_endpoint_large();
        let large_video = if large_ep.is_empty() {
            None
        } else {
            Some(large_ep.clone())
        };

        let is_conference = group.session.is_conference();
        let emoji_fingerprint = group.session.e2e().and_then(E2eCall::emoji_fingerprint);
        let e2e_verified_count = group
            .session
            .e2e()
            .map_or(0, |e| e.participants_set().len());

        let now_unix = now_unix_i32();
        let raw_messages = group.session.messages().messages();
        let raw_donors = &group.session.messages().stars_top().top_donors;
        let local_stars = group.session.messages().stars_local_state();
        let stars_scheduled = group.session.messages().reactions_paid_scheduled();

        let (chat_messages, stars_top) = read_session_sync(&self.runtime, |s| {
            let msgs: Vec<GroupCallMessageView> = raw_messages
                .iter()
                .map(|m| {
                    let app_peer = from_calls_peer_id(m.peer);
                    GroupCallMessageView {
                        id: m.id,
                        date: m.date,
                        peer: peer_to_call_info(s, to_data_peer(app_peer)),
                        text: m.text.clone(),
                        stars: m.stars,
                        failed: m.failed,
                        admin: m.admin,
                        mine: m.mine,
                        pinned: m.pin_finish_date > now_unix,
                    }
                })
                .collect();
            let donors: Vec<GroupStarsDonorView> = raw_donors
                .iter()
                .map(|d| GroupStarsDonorView {
                    peer: d.peer.map(|p| {
                        let app_peer = from_calls_peer_id(p);
                        peer_to_call_info(s, to_data_peer(app_peer))
                    }),
                    stars: d.stars,
                    is_my: d.my,
                })
                .collect();
            (msgs, donors)
        });

        group.snapshot = GroupCallSnapshot {
            group_peer: group.group_peer.clone(),
            title: group.title.clone(),
            join_as,
            state: group.session.state(),
            instance_state: group.session.instance_state(),
            mute_state: group.session.muted(),
            can_manage: group.can_manage,
            join_muted: list.join_muted(),
            messages_enabled: group.messages_enabled,
            recording: group.recording,
            camera_active: group.camera_active,
            screen_share_active: group.screen_share_active,
            local_audio_level: group.local_audio_level,
            members,
            large_video,
            large_video_pinned: group.session.video_endpoint_pinned(),
            invite_link_speaker: if !group.invite_link_speaker.is_empty() {
                group.invite_link_speaker.clone()
            } else {
                list.invite_link().to_owned()
            },
            invite_link_listener: group.invite_link_listener.clone(),
            is_conference,
            emoji_fingerprint,
            e2e_verified_count,
            chat_messages,
            stars_top,
            stars_total: local_stars.total,
            stars_my_total: local_stars.my,
            stars_scheduled,
            min_paid_stars: group.min_paid_stars,
            is_video_stream: group.is_video_stream,
        };
    }

    fn tick(&mut self, now_ms: u64) {
        // 1. Drain async RPC results and incoming MTProto call/group-call updates.
        self.drain_inbound_events(now_ms);

        // 2. Fire expired 1:1 call timers into `tg_calls::Call`.
        let due_call_timers: Vec<CallTimerKind> = self
            .call_timers
            .iter()
            .filter_map(|(&kind, &due)| (now_ms >= due).then_some(kind))
            .collect();
        for kind in due_call_timers {
            self.call_timers.remove(&kind);
            if let Some(call) = self.instance.current_call_mut() {
                call.on_timer(kind, now_ms);
            }
        }
        self.process_instance_actions(now_ms);

        // 3. Poll real `tg-voip::MediaEngine` events into `tg_calls::Call`.
        self.poll_call_media_events(now_ms);
        self.refresh_call_snapshot(now_ms);

        // 4. Fire expired group call, E2E subchain, and in-call message timers.
        if let Some(group) = self.active_group.as_mut() {
            let due_group_timers: Vec<(GroupCallTimerKind, Option<u64>)> = group
                .timers
                .iter()
                .filter_map(|(&kind, &(due, repeat))| (now_ms >= due).then_some((kind, repeat)))
                .collect();
            for (kind, repeat) in due_group_timers {
                if let Some(interval) = repeat {
                    group
                        .timers
                        .insert(kind, (now_ms.saturating_add(interval), Some(interval)));
                } else {
                    group.timers.remove(&kind);
                }
                group.session.on_timer(kind, now_ms);
            }

            let due_e2e_timers: Vec<(i32, E2eTimerKind)> = group
                .e2e_timers
                .iter()
                .filter_map(|(&k, &due)| (now_ms >= due).then_some(k))
                .collect();
            for (subchain, kind) in due_e2e_timers {
                group.e2e_timers.remove(&(subchain, kind));
                if let Some(e2e) = group.session.e2e_mut() {
                    let _ = e2e.on_timer(subchain, kind);
                }
            }

            if now_ms >= group.last_messages_prune_ms.saturating_add(1_000) {
                group.last_messages_prune_ms = now_ms;
                let _ = group
                    .session
                    .messages_mut()
                    .prune_expired(now_unix_i32(), false);
            }

            if let Some(due) = group.stars_poll_due_ms
                && now_ms >= due
            {
                group.stars_poll_due_ms =
                    Some(now_ms.saturating_add(STARS_STATS_SHORT_POLL_DELAY_MS));
                group.session.messages_mut().request_stars_stats();
            }
        }
        self.process_group_actions(now_ms);

        // 5. Poll real `tg-voip::GroupMediaEngine` events into `tg_calls::GroupCallSession`.
        self.poll_group_media_events(now_ms);
        self.refresh_group_snapshot();
    }

    fn join_as_candidates(&mut self, group_peer_id: PeerId) -> Vec<JoinAsCandidate> {
        if let Some(cached) = self.join_as_cache.get(&group_peer_id) {
            return cached.clone();
        }
        let default_self = read_session_sync(&self.runtime, |s| {
            let self_pid = s.self_peer_id();
            JoinAsCandidate {
                peer: peer_to_call_info(s, self_pid),
                is_personal: true,
                subtitle: "Personal account".to_owned(),
            }
        });

        let input_peer = read_session_sync(&self.runtime, |s| {
            input_peer_for(s, to_data_peer(group_peer_id))
        });
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return vec![default_self];
        }

        let rpc = self.runtime.rpc().clone();
        let res = block_on_runtime(self.runtime.tokio_handle(), async move {
            with_rpc_timeout(JOIN_AS_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::phone::GetGroupCallJoinAs { peer: input_peer })
                    .await
            })
            .await
        });

        let candidates = match res {
            Ok(tl_enums::phone::JoinAsPeers::JoinAsPeers(resp)) => {
                if !resp.users.is_empty() || !resp.chats.is_empty() {
                    write_session_sync(&self.runtime, |s| {
                        apply_tl_users_and_chats(s, &resp.users, &resp.chats);
                    });
                }
                read_session_sync(&self.runtime, |s| {
                    let self_pid = s.self_peer_id();
                    let mut out = Vec::with_capacity(resp.peers.len());
                    for p in &resp.peers {
                        let calls_pid = tg_calls::tl_conv::peer_from_tl(p);
                        let app_pid = from_calls_peer_id(calls_pid);
                        let data_pid = to_data_peer(app_pid);
                        let info = peer_to_call_info(s, data_pid);
                        let is_personal = data_pid == self_pid;
                        let subtitle = if is_personal {
                            "Personal account".to_owned()
                        } else if info.is_broadcast {
                            format!("{} subscribers", info.members_count)
                        } else {
                            format!("{} members", info.members_count)
                        };
                        out.push(JoinAsCandidate {
                            peer: info,
                            is_personal,
                            subtitle,
                        });
                    }
                    if out.is_empty() {
                        out.push(default_self.clone());
                    }
                    out
                })
            }
            Err(_) => vec![default_self],
        };
        self.join_as_cache.insert(group_peer_id, candidates.clone());
        candidates
    }
}

// =============================================================================
// Cloneable Handle Stored on `LiveSession`
// =============================================================================

/// Cheaply cloneable shared calls handle stored on [`LiveSession`], combining an `Arc<Mutex<...>>`
/// live controller with local borrowed view caches for [`CallsBackend`]'s `&T` / `&[T]` methods.
pub(crate) struct LiveCallsHandle {
    inner: Arc<Mutex<LiveCallsController>>,
    active_call: Option<ActiveCallSnapshot>,
    active_group: Option<GroupCallSnapshot>,
    audio_settings: CallAudioSettings,
    input_devices: Vec<AudioDeviceOption>,
    output_devices: Vec<AudioDeviceOption>,
    camera_devices: Vec<CameraDeviceInfo>,
    recent_calls: Vec<RecentCallEntry>,
    active_group_calls: Vec<CallPeerInfo>,
}

impl Clone for LiveCallsHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            active_call: self.active_call.clone(),
            active_group: self.active_group.clone(),
            audio_settings: self.audio_settings.clone(),
            input_devices: self.input_devices.clone(),
            output_devices: self.output_devices.clone(),
            camera_devices: self.camera_devices.clone(),
            recent_calls: self.recent_calls.clone(),
            active_group_calls: self.active_group_calls.clone(),
        }
    }
}

impl LiveCallsHandle {
    pub(crate) fn new(
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        bridge: &Arc<LiveRuntimeBridge>,
    ) -> Self {
        let ctrl = LiveCallsController::new(runtime, bridge);
        let audio_settings = ctrl.audio_settings.clone();
        let input_devices = ctrl.input_devices.clone();
        let output_devices = ctrl.output_devices.clone();
        let camera_devices = ctrl.camera_devices.clone();
        let recent_calls = ctrl.recent_calls.clone();
        let active_group_calls = ctrl.active_group_calls.clone();
        Self {
            inner: Arc::new(Mutex::new(ctrl)),
            active_call: None,
            active_group: None,
            audio_settings,
            input_devices,
            output_devices,
            camera_devices,
            recent_calls,
            active_group_calls,
        }
    }

    fn sync_from_controller(&mut self, ctrl: &LiveCallsController) {
        self.active_call = ctrl.call_snapshot.clone();
        self.active_group = ctrl.active_group.as_ref().map(|g| g.snapshot.clone());
        self.audio_settings = ctrl.audio_settings.clone();
        self.recent_calls = ctrl.recent_calls.clone();
        self.active_group_calls = ctrl.active_group_calls.clone();
    }

    fn with_ctrl<R>(&mut self, f: impl FnOnce(&mut LiveCallsController) -> R) -> R {
        let inner = Arc::clone(&self.inner);
        let mut guard = inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let res = f(&mut guard);
        self.sync_from_controller(&guard);
        res
    }
}

// =============================================================================
// Public Live Calls Extensions on `LiveSession`
// =============================================================================

impl LiveSession {
    /// Overrides the `tg-voip` [`MediaEngineConfig`] and [`GroupMediaConfig`] used when
    /// starting 1:1 or group call media engines (e.g. virtual audio backend in loopback tests).
    pub fn set_calls_media_config(
        &mut self,
        media_config: MediaEngineConfig,
        group_media_config: GroupMediaConfig,
    ) {
        self.calls.with_ctrl(|c| {
            c.audio_settings.input_device_id = media_config.input_device_id.clone();
            c.audio_settings.output_device_id = media_config.output_device_id.clone();
            c.audio_settings.noise_suppression = media_config.dsp.enable_ns;
            c.media_config = media_config;
            c.group_media_config = group_media_config;
        });
    }

    /// Returns the active 1:1 [`MediaEngine`]'s [`VirtualAudioHandle`] when running with
    /// [`tg_voip::AudioBackendKind::Virtual`].
    #[must_use]
    pub fn calls_virtual_audio(&self) -> Option<Arc<VirtualAudioHandle>> {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| c.call_engine.as_ref().and_then(MediaEngine::virtual_audio))
    }

    /// Returns the active [`GroupMediaEngine`]'s [`VirtualAudioHandle`] when running with
    /// [`tg_voip::AudioBackendKind::Virtual`].
    #[must_use]
    pub fn group_calls_virtual_audio(&self) -> Option<Arc<VirtualAudioHandle>> {
        self.calls.inner.lock().ok().and_then(|c| {
            c.active_group
                .as_ref()
                .and_then(|g| g.engine.virtual_audio())
        })
    }

    /// Returns live VoIP statistics from the active 1:1 [`MediaEngine`], if running.
    #[must_use]
    pub fn calls_voip_stats(&self) -> Option<VoipStatsSnapshot> {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| c.call_engine.as_ref().map(MediaEngine::stats))
    }

    /// Returns the most recently decoded remote 1:1 RGBA video frame, if any.
    #[must_use]
    pub fn calls_latest_remote_video_frame(&self) -> Option<RgbaVideoFrame> {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| c.call_latest_video_frame.clone())
    }

    /// Registers a known active group call for `group_peer_id` so [`CallsBackend::join_group_call`]
    /// can join it directly without an extra `getFullChat` / `getFullChannel` round-trip.
    pub fn register_known_group_call(
        &mut self,
        group_peer_id: PeerId,
        call_id: CallId,
        access_hash: i64,
        can_manage: bool,
        stream_dc_id: Option<i32>,
    ) {
        self.calls.with_ctrl(|c| {
            c.known_group_calls.insert(
                group_peer_id,
                KnownGroupCall {
                    call_id,
                    access_hash,
                    can_manage,
                    stream_dc_id,
                },
            );
            c.refresh_active_group_peers();
        });
    }

    /// Joins an existing group call with explicit server `call_id` and `access_hash`.
    pub fn join_group_call_with_id(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
        call_id: CallId,
        access_hash: i64,
        can_manage: bool,
        now_ms: u64,
    ) {
        self.calls.with_ctrl(|c| {
            if let Err(err) = c.start_group_call_session(
                group_peer_id,
                join_as,
                call_id,
                access_hash,
                can_manage,
                None,
                now_ms,
            ) {
                c.toast_queue
                    .push(format!("Failed to join group call: {err}"));
            }
        });
    }

    /// Returns the active group call's [`TelegramBroadcastChunkLoader`] (if a group call is active).
    #[must_use]
    pub fn calls_broadcast_loader(
        &self,
    ) -> Option<Arc<TelegramBroadcastChunkLoader<ErasedRpcClient>>> {
        self.calls.inner.lock().ok().and_then(|c| {
            c.active_group
                .as_ref()
                .map(|g| Arc::clone(&g.broadcast_loader))
        })
    }

    /// Creates and joins a new E2E conference call using a caller-provided [`E2eKeyPair`]
    /// (deterministic in tests).
    pub fn create_conference_call_with_key_pair(&mut self, key_pair: E2eKeyPair, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Err(err) = c.start_conference_call_session(None, Some(key_pair), now_ms) {
                c.toast_queue
                    .push(format!("Failed to create conference call: {err}"));
            }
        });
    }

    /// Joins an existing E2E conference call by invite `slug` using a caller-provided
    /// [`E2eKeyPair`] (deterministic in tests).
    pub fn join_conference_call_by_slug_with_key_pair(
        &mut self,
        slug: String,
        key_pair: E2eKeyPair,
        now_ms: u64,
    ) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Err(err) = c.start_conference_call_session(Some(slug), Some(key_pair), now_ms) {
                c.toast_queue
                    .push(format!("Failed to join conference call: {err}"));
            }
        });
    }

    /// Returns the active conference call's shared [`ConferenceEncryptDecrypt`] handle, if any.
    #[must_use]
    pub fn group_call_e2e_encrypt_decrypt(&self) -> Option<Arc<ConferenceEncryptDecrypt>> {
        self.calls.inner.lock().ok().and_then(|c| {
            c.active_group
                .as_ref()
                .and_then(|g| g.session.e2e_encrypt_decrypt())
        })
    }

    /// Returns `true` when the active [`GroupMediaEngine`] has E2E frame encryption enabled.
    #[must_use]
    pub fn group_call_e2e_frame_encryption_enabled(&self) -> bool {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| {
                c.active_group
                    .as_ref()
                    .map(|g| g.engine.e2e_frame_encryption_enabled())
            })
            .unwrap_or(false)
    }

    /// Returns the active conference E2E local public key, if a conference call is active.
    #[must_use]
    pub fn group_call_e2e_my_key(&self) -> Option<PublicKey> {
        self.calls.inner.lock().ok().and_then(|c| {
            c.active_group
                .as_ref()
                .and_then(|g| g.session.e2e().map(E2eCall::my_key))
        })
    }

    /// Returns the active conference E2E verified participant user IDs, if a conference is active.
    #[must_use]
    pub fn group_call_e2e_participants(&self) -> BTreeSet<CallsUserId> {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| {
                c.active_group
                    .as_ref()
                    .and_then(|g| g.session.e2e().map(|e| e.participants_set().clone()))
            })
            .unwrap_or_default()
    }

    /// Synchronously fetches recent 1:1 call history from the server via
    /// `messages.search(InputMessagesFilterPhoneCalls)` and updates `recent_calls()`.
    pub fn load_recent_calls_from_server(
        &mut self,
        offset_id: i32,
        limit: i32,
    ) -> Result<Vec<RecentCallEntry>, String> {
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        let req = tl_fns::messages::Search {
            peer: tl_enums::InputPeer::Empty,
            q: String::new(),
            from_id: None,
            saved_peer_id: None,
            saved_reaction: None,
            top_msg_id: None,
            filter: tl_enums::MessagesFilter::InputMessagesFilterPhoneCalls(
                tl_types::InputMessagesFilterPhoneCalls { missed: false },
            ),
            min_date: 0,
            max_date: 0,
            offset_id,
            add_offset: 0,
            limit: limit.clamp(1, 100),
            max_id: 0,
            min_id: 0,
            hash: 0,
        };
        let msgs = block_on_runtime(&handle, async move {
            with_rpc_timeout(CALL_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })?;
        let entries = write_session_sync(self.runtime(), |s| {
            parse_recent_calls_from_messages(s, msgs, now_unix_i32())
        });
        self.calls.with_ctrl(|c| {
            if offset_id == 0 {
                c.recent_calls = entries.clone();
            } else {
                for entry in &entries {
                    if !c.recent_calls.iter().any(|e| e.msg_id == entry.msg_id) {
                        c.recent_calls.push(entry.clone());
                    }
                }
            }
        });
        Ok(entries)
    }
}

// =============================================================================
// CallsBackend Implementation for LiveSession
// =============================================================================

impl CallsBackend for LiveSession {
    fn tick(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| c.tick(now_ms));
    }

    fn active_call(&self) -> Option<&ActiveCallSnapshot> {
        self.calls.active_call.as_ref()
    }

    fn start_outgoing_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            c.call_submitted_rating = None;
            c.call_screen_share_active = false;
            c.call_latest_video_frame = None;
            let _ =
                c.instance
                    .start_outgoing_call(CallsUserId(peer_id.bare()), video, false, now_ms);
            c.process_instance_actions(now_ms);
            c.refresh_call_snapshot(now_ms);
        });
    }

    fn confirm_outgoing_call(&mut self, _video: bool, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            c.instance.confirm_outgoing_call(now_ms);
            c.process_instance_actions(now_ms);
            c.refresh_call_snapshot(now_ms);
        });
    }

    fn simulate_incoming_call(&mut self, _peer_id: PeerId, _video: bool, _now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.toast_queue
                .push("Incoming calls in live mode arrive via MTProto updatePhoneCall.".to_owned());
        });
    }

    fn answer_call(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Some(call) = c.instance.current_call_mut() {
                call.answer();
            }
            c.process_instance_actions(now_ms);
            c.refresh_call_snapshot(now_ms);
        });
    }

    fn hangup_call(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Some(call) = c.instance.current_call_mut() {
                call.hangup(None, now_ms, false);
            }
            c.process_instance_actions(now_ms);
            c.refresh_call_snapshot(now_ms);
        });
    }

    fn redial_call(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            c.instance.redial_current_call();
            c.process_instance_actions(now_ms);
            c.refresh_call_snapshot(now_ms);
        });
    }

    fn set_call_muted(&mut self, muted: bool) {
        self.calls.with_ctrl(|c| {
            if let Some(call) = c.instance.current_call_mut() {
                call.set_muted(muted);
            }
            if let Some(engine) = c.call_engine.as_mut() {
                CallMediaEngine::set_muted(engine, muted);
            }
            if let Some(snap) = c.call_snapshot.as_mut() {
                snap.muted = muted;
            }
        });
    }

    fn toggle_call_camera(&mut self) {
        self.calls.with_ctrl(|c| {
            let currently_active = c
                .call_snapshot
                .as_ref()
                .is_some_and(|s| s.camera_state == VideoState::Active);
            if let Some(engine) = c.call_engine.as_ref() {
                if currently_active {
                    engine.stop_video();
                    if let Some(snap) = c.call_snapshot.as_mut() {
                        snap.camera_state = VideoState::Inactive;
                    }
                } else {
                    let cam_id = c.audio_settings.camera_device_id.clone();
                    match engine.start_camera_video(&cam_id, CameraCaptureConfig::default()) {
                        Ok(()) => {
                            c.call_screen_share_active = false;
                            if let Some(snap) = c.call_snapshot.as_mut() {
                                snap.camera_state = VideoState::Active;
                                snap.screen_share_active = false;
                            }
                        }
                        Err(err) => {
                            c.toast_queue.push(format!("Camera unavailable: {err}"));
                        }
                    }
                }
            }
        });
    }

    fn toggle_call_screen_share(&mut self) {
        self.calls.with_ctrl(|c| {
            if let Some(engine) = c.call_engine.as_ref() {
                if c.call_screen_share_active {
                    engine.stop_video();
                    c.call_screen_share_active = false;
                    if let Some(snap) = c.call_snapshot.as_mut() {
                        snap.screen_share_active = false;
                    }
                } else {
                    match engine.start_screen_share(ScreenCaptureConfig::default()) {
                        Ok(()) => {
                            c.call_screen_share_active = true;
                            if let Some(snap) = c.call_snapshot.as_mut() {
                                snap.screen_share_active = true;
                                snap.camera_state = VideoState::Inactive;
                            }
                        }
                        Err(err) => {
                            c.toast_queue
                                .push(format!("Screen capture unavailable: {err}"));
                        }
                    }
                }
            }
        });
    }

    fn submit_call_rating(&mut self, rating: u8, comment: String) {
        self.calls.with_ctrl(|c| {
            c.call_submitted_rating = Some(rating);
            if let Some(call) = c.instance.current_call_mut() {
                if comment.is_empty() {
                    call.set_rating(i32::from(rating));
                } else {
                    call.submit_rating(i32::from(rating), comment);
                }
            }
            c.process_instance_actions(0);
            if let Some(snap) = c.call_snapshot.as_mut() {
                snap.submitted_rating = Some(rating);
            }
        });
    }

    fn finish_call_rating(&mut self) {
        self.calls.with_ctrl(|c| {
            if let Some(call) = c.instance.current_call_mut() {
                call.finish_rating();
            }
            c.process_instance_actions(0);
            c.instance.destroy_current_call();
            c.call_snapshot = None;
            c.call_submitted_rating = None;
        });
    }

    fn active_group_call(&self) -> Option<&GroupCallSnapshot> {
        self.calls.active_group.as_ref()
    }

    fn join_group_call(&mut self, group_peer_id: PeerId, join_as: PeerId, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Some(known) = c.known_group_calls.get(&group_peer_id).copied() {
                if let Err(err) = c.start_group_call_session(
                    group_peer_id,
                    join_as,
                    known.call_id,
                    known.access_hash,
                    known.can_manage,
                    known.stream_dc_id,
                    now_ms,
                ) {
                    c.toast_queue
                        .push(format!("Failed to join group call: {err}"));
                }
            } else {
                c.spawn_lookup_and_join_group_call(group_peer_id, join_as);
            }
        });
    }

    fn switch_group_join_as(&mut self, join_as: PeerId, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_ref() {
                let gpeer = group.group_peer.id;
                let can_manage = group.can_manage;
                let id = group.session.id();
                let access_hash = group.session.access_hash();
                let stream_dc = group.session.stream_dc_id();
                if let Err(err) = c.start_group_call_session(
                    gpeer,
                    join_as,
                    id,
                    access_hash,
                    can_manage,
                    stream_dc,
                    now_ms,
                ) {
                    c.toast_queue
                        .push(format!("Failed to switch join-as identity: {err}"));
                }
            }
        });
    }

    fn leave_group_call(&mut self, discard: bool, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            let mut awaiting_discard = false;
            if let Some(group) = c.active_group.as_mut() {
                if discard && group.can_manage && !group.session.id().is_zero() {
                    group.session.discard();
                    group.engine.stop();
                    awaiting_discard = true;
                } else {
                    group.session.hangup();
                }
            }
            c.process_group_actions(now_ms);
            if awaiting_discard {
                c.refresh_group_snapshot();
            } else if let Some(mut group) = c.active_group.take() {
                group.engine.stop();
                c.instance.set_in_group_call(false);
            }
        });
    }

    fn set_group_mute_state(&mut self, state: MuteState, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.session.set_muted_and_update(state);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn set_group_push_to_talk(&mut self, pressed: bool, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            let delay = c.audio_settings.push_to_talk_delay_ms;
            if let Some(group) = c.active_group.as_mut() {
                group.session.push_to_talk(pressed, delay);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn toggle_group_camera(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.camera_active = !group.camera_active;
                group.session.toggle_camera(group.camera_active);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn toggle_group_screen_share(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.screen_share_active = !group.screen_share_active;
                group
                    .session
                    .toggle_screen_sharing(group.screen_share_active);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn set_participant_volume(&mut self, peer_id: PeerId, volume: i32) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                let clamped = volume.clamp(MIN_VOLUME, MAX_VOLUME);
                group
                    .session
                    .change_participant_volume(to_calls_peer_id(peer_id), clamped, false);
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn set_participant_muted(&mut self, peer_id: PeerId, muted: bool) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .toggle_participant_mute(to_calls_peer_id(peer_id), muted, false);
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn allow_participant_to_speak(&mut self, peer_id: PeerId) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .toggle_participant_mute(to_calls_peer_id(peer_id), false, false);
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn select_large_video(&mut self, endpoint: Option<VideoEndpoint>, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .show_video_endpoint_large(endpoint.unwrap_or_default(), now_ms);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn toggle_pin_large_video(&mut self) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                if group.session.video_endpoint_pinned() {
                    group.session.pin_video_endpoint(VideoEndpoint::default());
                } else {
                    let current = group.session.video_endpoint_large().clone();
                    if !current.is_empty() {
                        group.session.pin_video_endpoint(current);
                    }
                }
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn set_group_join_muted(&mut self, join_muted: bool) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.snapshot.join_muted = join_muted;
            }
            c.spawn_toggle_group_settings(Some(join_muted), None);
        });
    }

    fn set_group_messages_enabled(&mut self, enabled: bool) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.messages_enabled = enabled;
                group.snapshot.messages_enabled = enabled;
            }
            c.spawn_toggle_group_settings(None, Some(enabled));
        });
    }

    fn toggle_group_recording(
        &mut self,
        start: bool,
        title: Option<String>,
        video: bool,
        video_portrait: bool,
    ) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .toggle_recording(start, title, video, video_portrait);
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn request_group_rtmp_url(
        &mut self,
        peer_id: PeerId,
        revoke: bool,
    ) -> Result<RtmpStreamInfo, String> {
        let input_peer =
            read_session_sync(self.runtime(), |s| input_peer_for(s, to_data_peer(peer_id)));
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Unknown group or channel peer for RTMP stream.".to_owned());
        }
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        let tl_enums::phone::GroupCallStreamRtmpUrl::GroupCallStreamRtmpUrl(resp) =
            block_on_runtime(&handle, async move {
                with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                    rpc.invoke(&tl_fns::phone::GetGroupCallStreamRtmpUrl {
                        live_story: false,
                        peer: input_peer,
                        revoke,
                    })
                    .await
                })
                .await
            })?;
        Ok(RtmpStreamInfo {
            url: resp.url,
            key: resp.key,
        })
    }

    fn screen_share_sources(&self) -> Vec<ScreenShareSource> {
        enumerate_screen_sources()
    }

    fn start_call_screen_share_with_source(&mut self, source: &ScreenShareSource) {
        self.calls.with_ctrl(|c| {
            if let Some(engine) = c.call_engine.as_ref() {
                let cfg = ScreenCaptureConfig {
                    display_id: source.display_id.clone(),
                    fps: 15,
                };
                match engine.start_screen_share(cfg) {
                    Ok(()) => {
                        c.call_screen_share_active = true;
                        if let Some(snap) = c.call_snapshot.as_mut() {
                            snap.screen_share_active = true;
                            snap.camera_state = VideoState::Inactive;
                        }
                    }
                    Err(err) => {
                        c.toast_queue
                            .push(format!("Screen capture unavailable: {err}"));
                    }
                }
            }
        });
    }

    fn start_group_screen_share_with_source(&mut self, _source: &ScreenShareSource, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut()
                && !group.screen_share_active
            {
                group.screen_share_active = true;
                group.session.toggle_screen_sharing(true);
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn create_conference_call(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Err(err) = c.start_conference_call_session(None, None, now_ms) {
                c.toast_queue
                    .push(format!("Failed to create conference call: {err}"));
            }
        });
    }

    fn join_conference_call_by_slug(&mut self, slug: String, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.drain_inbound_events(now_ms);
            if let Err(err) = c.start_conference_call_session(Some(slug), None, now_ms) {
                c.toast_queue
                    .push(format!("Failed to join conference call: {err}"));
            }
        });
    }

    fn remove_conference_participants(&mut self, user_ids: &[u64], kick: bool, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.remove_conference_participants(user_ids, kick, now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn send_group_call_message(&mut self, text: String, paid_stars: i64, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            let random_id = c.next_random_u64();
            if let Some(group) = c.active_group.as_mut() {
                group.session.messages_mut().send(
                    text,
                    Vec::new(),
                    paid_stars,
                    random_id,
                    now_unix_i32(),
                );
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn delete_group_call_message(
        &mut self,
        msg_id: i32,
        delete_all_from: Option<PeerId>,
        report_spam: bool,
    ) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .messages_mut()
                    .delete_confirmed(MessageDeleteRequest {
                        id: msg_id,
                        delete_all_from: delete_all_from.map(to_calls_peer_id),
                        report_spam,
                    });
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn add_group_paid_stars_reaction(&mut self, count: i64) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.session.messages_mut().reactions_paid_add(count);
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn flush_group_paid_stars_reaction(&mut self, now_ms: u64) {
        self.calls.with_ctrl(|c| {
            let random_id = c.next_random_u64();
            if let Some(group) = c.active_group.as_mut() {
                group
                    .session
                    .messages_mut()
                    .reactions_paid_send(random_id, now_unix_i32());
            }
            c.process_group_actions(now_ms);
            c.refresh_group_snapshot();
        });
    }

    fn cancel_group_paid_stars_reaction(&mut self) {
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                group.session.messages_mut().reactions_paid_cancel();
            }
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
    }

    fn refresh_group_stars(&mut self) {
        self.calls.with_ctrl(|c| {
            let call = c
                .active_group
                .as_ref()
                .and_then(|g| (!g.session.id().is_zero()).then(|| g.session.input_call_handle()));
            if let Some(call) = call {
                c.spawn_get_group_call_stars(call);
            }
        });
    }

    fn refresh_recent_calls(&mut self) {
        self.calls.with_ctrl(|c| {
            c.spawn_fetch_recent_calls();
        });
    }

    fn export_group_call_invite(&mut self, can_self_unmute: bool) -> Result<String, String> {
        // Port of `calls_group_settings.cpp:938-974` (`phone.exportGroupCallInvite`).
        let input_call = self.calls.with_ctrl(|c| {
            c.active_group.as_ref().and_then(|g| {
                (!g.session.id().is_zero()).then(|| {
                    tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                        id: g.session.id().to_tl(),
                        access_hash: g.session.access_hash(),
                    })
                })
            })
        });
        let Some(call) = input_call else {
            return Err("No active group call to export an invite link for.".to_owned());
        };
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        let tl_enums::phone::ExportedGroupCallInvite::ExportedGroupCallInvite(resp) =
            block_on_runtime(&handle, async move {
                with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                    rpc.invoke(&tl_fns::phone::ExportGroupCallInvite {
                        can_self_unmute,
                        call,
                    })
                    .await
                })
                .await
            })?;
        let link = resp.link;
        self.calls.with_ctrl(|c| {
            if let Some(group) = c.active_group.as_mut() {
                if can_self_unmute {
                    group.invite_link_speaker = link.clone();
                } else {
                    group.invite_link_listener = link.clone();
                }
            }
            c.refresh_group_snapshot();
        });
        Ok(link)
    }

    fn invite_to_group_call(&mut self, user_ids: &[PeerId]) -> Result<Vec<PeerId>, String> {
        // Port of `GroupCall::inviteUsers` (`calls_group_call.cpp:53, 4086-4155`),
        // chunking requests into slices of `MAX_INVITE_PER_SLICE` (10) `InputUser`s.
        let input_call = self.calls.with_ctrl(|c| {
            c.active_group.as_ref().and_then(|g| {
                (!g.session.id().is_zero()).then(|| {
                    tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                        id: g.session.id().to_tl(),
                        access_hash: g.session.access_hash(),
                    })
                })
            })
        });
        let Some(call) = input_call else {
            return Err("No active group call to invite users to.".to_owned());
        };
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let resolved: Vec<(PeerId, tl_enums::InputUser)> = read_session_sync(self.runtime(), |s| {
            user_ids
                .iter()
                .filter_map(|&pid| {
                    let uid = to_data_peer(pid).to_user()?;
                    let input = input_user_for(s, uid);
                    if matches!(input, tl_enums::InputUser::Empty) {
                        None
                    } else {
                        Some((pid, input))
                    }
                })
                .collect()
        });
        if resolved.is_empty() {
            return Err("No resolvable user peers to invite to the group call.".to_owned());
        }

        let mut invited = Vec::new();
        let mut last_error: Option<String> = None;
        let handle = self.runtime().tokio_handle().clone();

        for chunk in resolved.chunks(MAX_INVITE_PER_SLICE) {
            let users_tl: Vec<tl_enums::InputUser> = chunk.iter().map(|(_, u)| u.clone()).collect();
            let rpc = self.runtime().rpc().clone();
            let call_clone = call.clone();
            let res = block_on_runtime(&handle, async move {
                with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                    rpc.invoke(&tl_fns::phone::InviteToGroupCall {
                        call: call_clone,
                        users: users_tl,
                    })
                    .await
                })
                .await
            });
            match res {
                Ok(updates) => {
                    self.calls.with_ctrl(|c| {
                        c.apply_inline_updates(updates, 0);
                        c.process_group_actions(0);
                        c.refresh_group_snapshot();
                    });
                    invited.extend(chunk.iter().map(|(pid, _)| *pid));
                }
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }

        if invited.is_empty()
            && let Some(err) = last_error
        {
            return Err(err);
        }
        Ok(invited)
    }

    fn save_default_group_call_join_as(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
    ) -> Result<bool, String> {
        // Port of `GroupCall::saveDefaultJoinAs` (`calls_group_call.cpp:1586-1592`).
        let (peer_tl, join_as_tl) = read_session_sync(self.runtime(), |s| {
            (
                input_peer_for(s, to_data_peer(group_peer_id)),
                input_peer_for(s, to_data_peer(join_as)),
            )
        });
        if matches!(peer_tl, tl_enums::InputPeer::Empty) {
            return Err("Unknown group or channel peer for default join-as.".to_owned());
        }
        if matches!(join_as_tl, tl_enums::InputPeer::Empty) {
            return Err("Unknown join-as peer identity.".to_owned());
        }
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        block_on_runtime(&handle, async move {
            with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::phone::SaveDefaultGroupCallJoinAs {
                    peer: peer_tl,
                    join_as: join_as_tl,
                })
                .await
            })
            .await
        })
    }

    fn start_scheduled_group_call(&mut self) -> Result<(), String> {
        // Port of `GroupCall::startScheduledNow` (`calls_group_call.cpp:2262-2271`).
        let input_call = self.calls.with_ctrl(|c| {
            c.active_group.as_ref().and_then(|g| {
                (!g.session.id().is_zero()).then(|| {
                    tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                        id: g.session.id().to_tl(),
                        access_hash: g.session.access_hash(),
                    })
                })
            })
        });
        let Some(call) = input_call else {
            return Err("No active scheduled group call to start.".to_owned());
        };
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        let updates = block_on_runtime(&handle, async move {
            with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::phone::StartScheduledGroupCall { call })
                    .await
            })
            .await
        })?;
        self.calls.with_ctrl(|c| {
            c.apply_inline_updates(updates, 0);
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
        Ok(())
    }

    fn toggle_group_call_start_subscription(&mut self, subscribed: bool) -> Result<(), String> {
        // Port of `GroupCall::toggleScheduleStartSubscribed` (`calls_group_call.cpp:2273-2283`).
        let input_call = self.calls.with_ctrl(|c| {
            c.active_group.as_ref().and_then(|g| {
                (!g.session.id().is_zero()).then(|| {
                    tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                        id: g.session.id().to_tl(),
                        access_hash: g.session.access_hash(),
                    })
                })
            })
        });
        let Some(call) = input_call else {
            return Err("No active scheduled group call to toggle subscription for.".to_owned());
        };
        let rpc = self.runtime().rpc().clone();
        let handle = self.runtime().tokio_handle().clone();
        let updates = block_on_runtime(&handle, async move {
            with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::phone::ToggleGroupCallStartSubscription { call, subscribed })
                    .await
            })
            .await
        })?;
        self.calls.with_ctrl(|c| {
            c.apply_inline_updates(updates, 0);
            c.process_group_actions(0);
            c.refresh_group_snapshot();
        });
        Ok(())
    }

    fn delete_phone_call_history(&mut self, revoke: bool) -> Result<usize, String> {
        // Port of `calls_box_controller.cpp:756-785`: loop `messages.deletePhoneCallHistory`
        // while `offset > 0`, deleting returned message IDs from session histories and clearing
        // all local recent call items once `offset == 0`.
        const MAX_DELETE_PAGES: usize = 100;
        let handle = self.runtime().tokio_handle().clone();
        let mut total_deleted = 0usize;

        for _ in 0..MAX_DELETE_PAGES {
            let rpc = self.runtime().rpc().clone();
            let tl_enums::messages::AffectedFoundMessages::AffectedFoundMessages(resp) =
                block_on_runtime(&handle, async move {
                    with_rpc_timeout(CALL_RPC_TIMEOUT, async move {
                        rpc.invoke(&tl_fns::messages::DeletePhoneCallHistory { revoke })
                            .await
                    })
                    .await
                })?;
            let deleted_ids: Vec<tg_data::MsgId> = resp
                .messages
                .iter()
                .map(|&id| tg_data::MsgId(i64::from(id)))
                .collect();
            total_deleted += deleted_ids.len();
            if !deleted_ids.is_empty() {
                write_session_sync(self.runtime(), |s| {
                    let _ = s.delete_non_channel_messages(&deleted_ids);
                });
                let raw_ids = resp.messages;
                self.calls.with_ctrl(|c| {
                    c.recent_calls
                        .retain(|entry| !raw_ids.contains(&entry.msg_id));
                });
            }
            if resp.offset <= 0 {
                break;
            }
        }

        self.calls.with_ctrl(|c| {
            c.recent_calls.clear();
        });
        self.bridge_state().notify_ui();
        Ok(total_deleted)
    }

    fn active_group_call_schedule_date(&self) -> Option<i32> {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| c.active_group.as_ref().map(|g| g.session.schedule_date()))
            .filter(|&d| d > 0)
    }

    fn active_group_call_schedule_subscribed(&self) -> bool {
        self.calls
            .inner
            .lock()
            .ok()
            .and_then(|c| {
                c.active_group
                    .as_ref()
                    .map(|g| g.session.schedule_start_subscribed())
            })
            .unwrap_or(false)
    }

    fn group_invite_candidates(&self) -> Vec<CallPeerInfo> {
        // Port of `Calls::Group::PrepareInviteBox` (`calls/group/calls_group_invite_controller.cpp:824-905`):
        // collect non-joined chat members first, followed by live contacts and other known
        // invitable users from `Session`, excluding self, already-joined participants, bots,
        // support/deleted accounts, chats, and channels.
        let (active_group_peer, mut already_in) = self
            .calls
            .inner
            .lock()
            .ok()
            .map(|c| {
                let mut joined = BTreeSet::new();
                let group_peer = c.active_group.as_ref().map(|g| {
                    for p in g.session.participants_list().participants() {
                        joined.insert(to_data_peer(from_calls_peer_id(p.peer_id)));
                    }
                    for m in &g.snapshot.members {
                        joined.insert(to_data_peer(m.peer.id));
                    }
                    to_data_peer(g.group_peer.id)
                });
                (group_peer, joined)
            })
            .unwrap_or_default();

        read_session_sync(self.runtime(), move |s| {
            already_in.insert(s.self_peer_id());

            let is_invitable = |pid: tg_data::PeerId| -> bool {
                !already_in.contains(&pid)
                    && s.peer(pid)
                        .and_then(tg_data::Peer::as_user)
                        .is_some_and(|u| {
                            !u.is_self() && !u.is_bot() && !u.is_inaccessible() && !u.is_support()
                        })
            };

            let mut seen = BTreeSet::new();
            let mut candidates = Vec::new();

            // 1. Non-joined members of the active group chat (`InviteController`).
            if let Some(group_pid) = active_group_peer
                && let Some(chat) = s.peer(group_pid).and_then(tg_data::Peer::as_chat)
            {
                for &uid in &chat.participants {
                    let pid = tg_data::PeerId::from_user(uid);
                    if is_invitable(pid) && seen.insert(pid) {
                        candidates.push(peer_to_call_info(s, pid));
                    }
                }
            }

            // 2. Non-joined contacts from `Session` (`InviteContactsController`).
            for uid in s.contact_user_ids() {
                let pid = tg_data::PeerId::from_user(DataUserId(uid));
                if is_invitable(pid) && seen.insert(pid) {
                    candidates.push(peer_to_call_info(s, pid));
                }
            }

            // 3. Any remaining loaded invitable users (e.g. supergroup/dialog members).
            let mut extra_users: Vec<tg_data::PeerId> = crate::convert::known_peer_ids(s)
                .into_iter()
                .filter(|&pid| is_invitable(pid) && !seen.contains(&pid))
                .collect();
            extra_users.sort_by_key(|p| p.0);
            for pid in extra_users {
                if seen.insert(pid) {
                    candidates.push(peer_to_call_info(s, pid));
                }
            }

            candidates
        })
    }

    fn set_noise_suppression(&mut self, enabled: bool) {
        self.calls.with_ctrl(|c| {
            c.audio_settings.noise_suppression = enabled;
            c.media_config.dsp.enable_ns = enabled;
            c.group_media_config.dsp.enable_ns = enabled;
        });
    }

    fn set_audio_devices(&mut self, input_id: String, output_id: String) {
        self.calls.with_ctrl(|c| {
            c.audio_settings.input_device_id = input_id.clone();
            c.audio_settings.output_device_id = output_id.clone();
            c.media_config.input_device_id = input_id.clone();
            c.media_config.output_device_id = output_id.clone();
            c.group_media_config.input_device_id = input_id.clone();
            c.group_media_config.output_device_id = output_id.clone();
            if let Some(engine) = c.call_engine.as_mut() {
                CallMediaEngine::set_audio_devices(engine, &input_id, &output_id);
            }
            if let Some(group) = c.active_group.as_ref() {
                group.engine.set_audio_devices(&input_id, &output_id);
            }
        });
    }

    fn set_camera_device(&mut self, camera_id: String) {
        self.calls.with_ctrl(|c| {
            c.audio_settings.camera_device_id = camera_id;
        });
    }

    fn set_push_to_talk_settings(&mut self, enabled: bool, shortcut: String, delay_ms: u64) {
        self.calls.with_ctrl(|c| {
            c.audio_settings.push_to_talk_enabled = enabled;
            c.audio_settings.push_to_talk_shortcut = shortcut;
            c.audio_settings.push_to_talk_delay_ms = delay_ms.min(2_000);
        });
    }

    fn audio_settings(&self) -> &CallAudioSettings {
        &self.calls.audio_settings
    }

    fn input_devices(&self) -> &[AudioDeviceOption] {
        &self.calls.input_devices
    }

    fn output_devices(&self) -> &[AudioDeviceOption] {
        &self.calls.output_devices
    }

    fn camera_devices(&self) -> &[CameraDeviceInfo] {
        &self.calls.camera_devices
    }

    fn recent_calls(&self) -> &[RecentCallEntry] {
        &self.calls.recent_calls
    }

    fn delete_recent_calls(&mut self, msg_ids: &[i32]) {
        let valid_ids: Vec<i32> = msg_ids.iter().copied().filter(|&id| id > 0).collect();
        self.calls.with_ctrl(|c| {
            c.recent_calls
                .retain(|entry| !msg_ids.contains(&entry.msg_id));
            if !valid_ids.is_empty() {
                let rpc = c.runtime.rpc().clone();
                let runtime = Arc::clone(&c.runtime);
                c.runtime.tokio_handle().spawn(async move {
                    if let Ok(Ok(tl_enums::messages::AffectedMessages::AffectedMessages(aff))) =
                        tokio::time::timeout(
                            CALL_RPC_TIMEOUT,
                            rpc.invoke(&tl_fns::messages::DeleteMessages {
                                revoke: false,
                                id: valid_ids,
                            }),
                        )
                        .await
                    {
                        let _ = runtime
                            .apply_affected_history(None, aff.pts, aff.pts_count)
                            .await;
                    }
                });
            }
        });
    }

    fn active_chat_group_calls(&self) -> &[CallPeerInfo] {
        &self.calls.active_group_calls
    }

    fn join_as_candidates(&self, group_peer_id: PeerId) -> Vec<JoinAsCandidate> {
        self.calls
            .inner
            .lock()
            .map(|mut c| c.join_as_candidates(group_peer_id))
            .unwrap_or_default()
    }

    fn drain_sound_events(&mut self) -> Vec<BackendSoundEvent> {
        self.calls.with_ctrl(|c| std::mem::take(&mut c.sound_queue))
    }

    fn drain_toasts(&mut self) -> Vec<String> {
        self.calls.with_ctrl(|c| std::mem::take(&mut c.toast_queue))
    }
}

// =============================================================================
// Updates Subscription & Session Helpers
// =============================================================================

/// Parses `messages.search(InputMessagesFilterPhoneCalls)` results into [`RecentCallEntry`] rows.
pub(crate) fn parse_recent_calls_from_messages(
    session: &mut Session,
    msgs: tl_enums::messages::Messages,
    now_unix: i32,
) -> Vec<RecentCallEntry> {
    let (users, chats, messages) = match msgs {
        tl_enums::messages::Messages::Messages(m) => (m.users, m.chats, m.messages),
        tl_enums::messages::Messages::Slice(m) => (m.users, m.chats, m.messages),
        tl_enums::messages::Messages::ChannelMessages(m) => (m.users, m.chats, m.messages),
        tl_enums::messages::Messages::NotModified(_) => return Vec::new(),
    };
    if !users.is_empty() || !chats.is_empty() {
        apply_tl_users_and_chats(session, &users, &chats);
    }

    let mut out = Vec::new();
    for msg in messages {
        let tl_enums::Message::Service(svc) = msg else {
            continue;
        };
        let tl_enums::MessageAction::PhoneCall(pc) = svc.action else {
            continue;
        };
        let peer_calls_id = tg_calls::tl_conv::peer_from_tl(&svc.peer_id);
        let peer_app_id = from_calls_peer_id(peer_calls_id);
        let peer = peer_to_call_info(session, to_data_peer(peer_app_id));

        let direction = if svc.out {
            RecentCallDirection::Out
        } else if matches!(
            pc.reason,
            Some(tl_enums::PhoneCallDiscardReason::Missed | tl_enums::PhoneCallDiscardReason::Busy)
        ) && pc.duration.unwrap_or(0) == 0
        {
            RecentCallDirection::Missed
        } else {
            RecentCallDirection::In
        };

        let dt = unix_to_local_datetime(svc.date);
        let days_ago = (now_unix.saturating_sub(svc.date)).max(0) as u32 / 86_400;
        let duration_secs = pc.duration.filter(|&d| d > 0).map(i64::from);

        out.push(RecentCallEntry {
            msg_id: svc.id,
            peer,
            direction,
            is_video: pc.video,
            days_ago,
            hour: dt.hour,
            minute: dt.minute,
            duration_secs,
        });
    }
    out
}

fn spawn_updates_listener(
    runtime: &Arc<SessionRuntime<ErasedRpcClient>>,
    bridge_state: &Arc<LiveBridgeState>,
    tx: mpsc::UnboundedSender<LiveCallInboundEvent>,
) -> Option<JoinHandle<()>> {
    let mut updates_rx = {
        let _guard = runtime.tokio_handle().enter();
        runtime.rpc().subscribe_updates()?
    };
    let session_arc = Arc::clone(runtime.session());
    let bridge = Arc::clone(bridge_state);

    Some(runtime.tokio_handle().spawn(async move {
        loop {
            match updates_rx.recv().await {
                Ok(updates) => {
                    forward_call_updates(&session_arc, &tx, &bridge, updates).await;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "live calls updates receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}

async fn forward_call_updates(
    session_arc: &Arc<RwLock<Session>>,
    tx: &mpsc::UnboundedSender<LiveCallInboundEvent>,
    bridge: &Arc<LiveBridgeState>,
    updates: tl_enums::Updates,
) {
    let (_, _, list) = unpack_tl_updates(updates);
    let mut relevant = Vec::new();
    for upd in list {
        match upd {
            tl_enums::Update::PhoneCall(u) => {
                relevant.push(LiveCallInboundEvent::PhoneCallUpdate(u.phone_call));
            }
            tl_enums::Update::PhoneCallSignalingData(u) => {
                relevant.push(LiveCallInboundEvent::PhoneCallSignalingData(u));
            }
            tl_enums::Update::GroupCall(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallUpdate {
                    peer: u.peer,
                    call: u.call,
                });
            }
            tl_enums::Update::GroupCallParticipants(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallParticipantsUpdate(u));
            }
            tl_enums::Update::GroupCallConnection(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallConnectionUpdate(u));
            }
            tl_enums::Update::GroupCallChainBlocks(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallChainBlocksUpdate(u));
            }
            tl_enums::Update::GroupCallMessage(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallMessageUpdate(u));
            }
            tl_enums::Update::GroupCallEncryptedMessage(u) => {
                relevant.push(LiveCallInboundEvent::GroupCallEncryptedMessageUpdate(u));
            }
            tl_enums::Update::DeleteGroupCallMessages(u) => {
                relevant.push(LiveCallInboundEvent::DeleteGroupCallMessagesUpdate(u));
            }
            _ => {}
        }
    }
    if relevant.is_empty() {
        return;
    }
    let _ = session_arc;
    for ev in relevant {
        let _ = tx.send(ev);
    }
    bridge.notify_ui();
}

fn sync_group_ssrc_users(group: &mut LiveActiveGroup, update: &ParticipantUpdate) {
    if let Some(ref was) = update.was {
        let same_ssrc = update.now.as_ref().is_some_and(|n| n.ssrc == was.ssrc);
        if was.ssrc != 0 && !same_ssrc {
            group.engine.unregister_ssrc(was.ssrc);
        }
        let was_add = was.additional_audio_ssrc();
        let same_add = update
            .now
            .as_ref()
            .is_some_and(|n| n.additional_audio_ssrc() == was_add);
        if was_add != 0 && !same_add {
            group.engine.unregister_ssrc(was_add);
        }
        if update.now.is_none()
            && let Some(ref vp) = was.video_params
        {
            for grp in vp
                .camera
                .ssrc_groups
                .iter()
                .chain(vp.screen.ssrc_groups.iter())
            {
                for &s in &grp.ssrcs {
                    if s != 0 {
                        group.engine.unregister_ssrc(s);
                    }
                }
            }
        }
    }
    if let Some(ref now) = update.now
        && let Some(uid) = now.peer_id.to_user()
    {
        register_participant_ssrcs(&group.engine, now, uid);
    }
}

fn register_all_group_ssrcs(group: &mut LiveActiveGroup) {
    for p in group.session.participants_list().participants() {
        if let Some(uid) = p.peer_id.to_user() {
            register_participant_ssrcs(&group.engine, p, uid);
        }
    }
}

fn register_participant_ssrcs(
    engine: &GroupMediaEngine,
    p: &tg_calls::GroupCallParticipant,
    uid: CallsUserId,
) {
    if p.ssrc != 0 {
        engine.register_ssrc_user(p.ssrc, uid);
    }
    let add = p.additional_audio_ssrc();
    if add != 0 {
        engine.register_ssrc_user(add, uid);
    }
    if let Some(ref vp) = p.video_params {
        for grp in vp
            .camera
            .ssrc_groups
            .iter()
            .chain(vp.screen.ssrc_groups.iter())
        {
            for &s in &grp.ssrcs {
                if s != 0 {
                    engine.register_ssrc_user(s, uid);
                }
            }
        }
    }
}

fn apply_updates_via_runtime(
    runtime: &Arc<SessionRuntime<ErasedRpcClient>>,
    updates: tl_enums::Updates,
) {
    let rt = Arc::clone(runtime);
    let _ = match tokio::runtime::Handle::try_current() {
        Ok(current) => match current.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(move || rt.apply_updates_blocking(updates))
            }
            _ => std::thread::scope(|scope| {
                scope
                    .spawn(move || rt.apply_updates_blocking(updates))
                    .join()
                    .unwrap_or_else(|_| panic!("calls apply_updates helper thread panicked"))
            }),
        },
        Err(_) => rt.apply_updates_blocking(updates),
    };
}

fn unpack_tl_updates(
    updates: tl_enums::Updates,
) -> (
    Vec<tl_enums::User>,
    Vec<tl_enums::Chat>,
    Vec<tl_enums::Update>,
) {
    match updates {
        tl_enums::Updates::Updates(u) => (u.users, u.chats, u.updates),
        tl_enums::Updates::Combined(u) => (u.users, u.chats, u.updates),
        tl_enums::Updates::UpdateShort(u) => (Vec::new(), Vec::new(), vec![u.update]),
        _ => (Vec::new(), Vec::new(), Vec::new()),
    }
}

fn read_session_sync<R: Send>(
    runtime: &SessionRuntime<ErasedRpcClient>,
    f: impl FnOnce(&Session) -> R + Send,
) -> R {
    let lock = runtime.session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(runtime.tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session_sync<R: Send>(
    runtime: &SessionRuntime<ErasedRpcClient>,
    f: impl FnOnce(&mut Session) -> R + Send,
) -> R {
    let lock = runtime.session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(runtime.tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn resolve_input_user(
    runtime: &SessionRuntime<ErasedRpcClient>,
    user_id: CallsUserId,
) -> tl_enums::InputUser {
    let data_uid = DataUserId::from_tl(user_id.to_tl());
    let resolved = read_session_sync(runtime, |s| input_user_for(s, data_uid));
    if matches!(resolved, tl_enums::InputUser::Empty) {
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: user_id.to_tl(),
            access_hash: 0,
        })
    } else {
        resolved
    }
}

fn resolve_input_peer_for_calls(
    runtime: &SessionRuntime<ErasedRpcClient>,
    peer: tg_calls::types::PeerId,
) -> tl_enums::InputPeer {
    let app_peer = from_calls_peer_id(peer);
    let data_peer = to_data_peer(app_peer);
    let resolved = read_session_sync(runtime, |s| input_peer_for(s, data_peer));
    if !matches!(resolved, tl_enums::InputPeer::Empty) {
        return resolved;
    }
    if let Some(u) = peer.to_user() {
        tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id: u.to_tl(),
            access_hash: 0,
        })
    } else if let Some(c) = peer.to_chat() {
        tl_enums::InputPeer::Chat(tl_types::InputPeerChat { chat_id: c.to_tl() })
    } else if let Some(ch) = peer.to_channel() {
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: ch.to_tl(),
            access_hash: 0,
        })
    } else {
        tl_enums::InputPeer::Empty
    }
}

// =============================================================================
// Unit & End-to-End Loopback Tests (FakeRpcClient + LoopbackReflector)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use tg_calls::dh::DH_GOOD_PRIME;
    use tg_calls::{CallState, GroupCallState, InstanceState};
    use tg_session::SessionRuntimeConfig;
    use tg_tl::{Deserializable, Identifiable, RemoteCall, Serializable};
    use tg_voip::{
        AudioBackendKind, BroadcastChunkScale, DEFAULT_VOICE_BITRATE_BPS, JitterBufferConfig,
        LoopbackReflector, NetworkType, ReflectorImpairment, TransportWireMode, VOIP_FRAME_SAMPLES,
        VOIP_SAMPLE_RATE, VoiceDspConfig, VoiceOpusEncoder, encode_ogg_opus_chunk,
    };

    const TEST_TIMEOUT: Duration = Duration::from_secs(15);

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
            access_hash: Some(id * 1000 + 7),
            first_name: Some(first_name.to_owned()),
            last_name: Some("Test".to_owned()),
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
            broadcast: false,
            verified: false,
            megagroup: true,
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
            broadcast_messages_allowed: false,
            monoforum: false,
            forum_tabs: false,
            in_news: false,
            forum_sidebar: false,
            id,
            access_hash: Some(id * 13 + 5),
            title: title.to_owned(),
            username: Some("testgroup".to_owned()),
            photo: tl_enums::ChatPhoto::Empty,
            date: 1_750_000_000,
            restriction_reason: None,
            admin_rights: None,
            banned_rights: None,
            default_banned_rights: None,
            participants_count: Some(42),
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

    fn make_virtual_media_config() -> MediaEngineConfig {
        MediaEngineConfig {
            audio_backend: AudioBackendKind::Virtual,
            virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 6,
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
        }
    }

    fn make_virtual_group_config() -> GroupMediaConfig {
        GroupMediaConfig {
            audio_backend: AudioBackendKind::Virtual,
            virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 6,
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
        }
    }

    fn generate_sine(freq_hz: f32, duration_ms: usize, amplitude: f32) -> Vec<f32> {
        let total = (VOIP_SAMPLE_RATE as usize * duration_ms) / 1000;
        (0..total)
            .map(|i| {
                let t = i as f32 / VOIP_SAMPLE_RATE as f32;
                (2.0 * std::f32::consts::PI * freq_hz * t).sin() * amplitude
            })
            .collect()
    }

    fn goertzel_magnitude(samples: &[f32], target_hz: f32) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let n = samples.len() as f64;
        let omega = 2.0 * std::f64::consts::PI * f64::from(target_hz) / f64::from(VOIP_SAMPLE_RATE);
        let coeff = 2.0 * omega.cos();
        let mut s1 = 0.0f64;
        let mut s2 = 0.0f64;
        for &x in samples {
            let s = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s2 * s2 + s1 * s1 - coeff * s1 * s2).max(0.0).sqrt() / n
    }

    // -------------------------------------------------------------------------
    // Shared Fake Telegram Call Server + Per-Account FakeRpcClient
    // -------------------------------------------------------------------------

    #[derive(Debug, Clone)]
    struct RecordedDiscard {
        caller_uid: i64,
        duration: i32,
        reason: tl_enums::PhoneCallDiscardReason,
        connection_id: i64,
    }

    #[derive(Debug, Clone)]
    struct RecordedRating {
        rating: i32,
        comment: String,
    }

    #[derive(Debug, Clone)]
    struct RecordedDebug {
        debug_json: String,
    }

    #[derive(Debug, Clone)]
    struct ServerCallRecord {
        id: i64,
        access_hash: i64,
        admin_id: i64,
        participant_id: i64,
        video: bool,
        g_b: Option<Vec<u8>>,
        protocol: tl_enums::PhoneCallProtocol,
        date: i32,
    }

    type GroupEditRecord = (i64, Option<bool>, Option<i32>, Option<bool>);

    struct FakeCallServer {
        reflector_addr: Option<SocketAddr>,
        need_rating_on_discard: bool,
        need_debug_on_discard: bool,
        next_call_id: i64,
        calls: HashMap<i64, ServerCallRecord>,
        updates_tx_by_user: HashMap<i64, broadcast::Sender<tl_enums::Updates>>,
        users: HashMap<i64, tl_types::User>,
        discards: Vec<RecordedDiscard>,
        ratings: Vec<RecordedRating>,
        debugs: Vec<RecordedDebug>,
        signaling_count: usize,
        // Group call state
        group_join_payloads: Vec<String>,
        group_join_response_json: String,
        group_stream_dc_id: Option<i32>,
        group_edits: Vec<GroupEditRecord>,
        group_leaves: Vec<(i64, i32)>,
        group_settings_toggles: Vec<(Option<bool>, Option<bool>)>,
        group_participants_queries: Vec<Vec<i32>>,
        group_resolved_participant: Option<tl_types::GroupCallParticipant>,
        broadcast_chunk_bytes: Vec<u8>,
        broadcast_fetches: Vec<(DcId, i64, i32)>,
        broadcast_fail_first_time_too_big: bool,
    }

    impl FakeCallServer {
        fn new(reflector_addr: Option<SocketAddr>) -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Self {
                reflector_addr,
                need_rating_on_discard: true,
                need_debug_on_discard: true,
                next_call_id: 900_001,
                calls: HashMap::new(),
                updates_tx_by_user: HashMap::new(),
                users: HashMap::new(),
                discards: Vec::new(),
                ratings: Vec::new(),
                debugs: Vec::new(),
                signaling_count: 0,
                group_join_payloads: Vec::new(),
                group_join_response_json: String::new(),
                group_stream_dc_id: None,
                group_edits: Vec::new(),
                group_leaves: Vec::new(),
                group_settings_toggles: Vec::new(),
                group_participants_queries: Vec::new(),
                group_resolved_participant: None,
                broadcast_chunk_bytes: Vec::new(),
                broadcast_fetches: Vec::new(),
                broadcast_fail_first_time_too_big: false,
            }))
        }

        fn push_update_to(&self, uid: i64, update: tl_enums::Update) {
            if let Some(tx) = self.updates_tx_by_user.get(&uid) {
                let _ = tx.send(tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![update],
                    users: self
                        .users
                        .values()
                        .cloned()
                        .map(tl_enums::User::User)
                        .collect(),
                    chats: Vec::new(),
                    date: now_unix_i32(),
                    seq: 1,
                }));
            }
        }

        fn make_connections(&self) -> Vec<tl_enums::PhoneConnection> {
            let Some(addr) = self.reflector_addr else {
                return Vec::new();
            };
            let peer_tag = vec![
                0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0x01, 0x02,
                0x03, 0x04,
            ];
            vec![tl_enums::PhoneConnection::PhoneConnection(
                tl_types::PhoneConnection {
                    tcp: false,
                    id: 777,
                    ip: addr.ip().to_string(),
                    ipv6: String::new(),
                    port: i32::from(addr.port()),
                    peer_tag,
                },
            )]
        }
    }

    #[derive(Clone)]
    struct AccountFakeRpc {
        uid: i64,
        main_dc: Arc<Mutex<DcId>>,
        updates_tx: broadcast::Sender<tl_enums::Updates>,
        server: Arc<Mutex<FakeCallServer>>,
        recorded_ctors: Arc<Mutex<Vec<u32>>>,
    }

    impl AccountFakeRpc {
        fn register(server: &Arc<Mutex<FakeCallServer>>, user: tl_types::User) -> Self {
            let uid = user.id;
            let (updates_tx, _) = broadcast::channel(256);
            {
                let mut srv = server.lock().expect("lock server");
                srv.users.insert(uid, user);
                srv.updates_tx_by_user.insert(uid, updates_tx.clone());
            }
            Self {
                uid,
                main_dc: Arc::new(Mutex::new(2)),
                updates_tx,
                server: Arc::clone(server),
                recorded_ctors: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_ctors(&self) -> Vec<u32> {
            self.recorded_ctors.lock().expect("lock ctors").clone()
        }
    }

    impl RpcClient for AccountFakeRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            let dc = self.main_dc_id();
            self.invoke_in_dc(dc, req).await
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            let bytes = req.to_bytes();
            let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            self.recorded_ctors.lock().expect("lock ctors").push(ctor);

            let resp_bytes = handle_fake_rpc(self.uid, dc_id, ctor, &bytes, &self.server)?;
            R::Return::from_bytes(&resp_bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        fn main_dc_id(&self) -> DcId {
            *self.main_dc.lock().expect("lock main_dc")
        }

        fn set_main_dc_id(&self, dc_id: DcId) {
            *self.main_dc.lock().expect("lock main_dc") = dc_id;
        }

        fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
            Some(self.updates_tx.subscribe())
        }
    }

    fn handle_fake_rpc(
        uid: i64,
        dc_id: DcId,
        ctor: u32,
        bytes: &[u8],
        server: &Arc<Mutex<FakeCallServer>>,
    ) -> Result<Vec<u8>, InvocationError> {
        let mut srv = server.lock().expect("lock server");

        // Startup SessionRuntime RPCs
        if ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
            let self_user = srv
                .users
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| make_tl_user(uid, "Self", "self_u", true));
            return Ok(vec![tl_enums::User::User(self_user)].to_bytes());
        }
        if ctor == tl_fns::updates::GetState::CONSTRUCTOR_ID {
            return Ok(tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: now_unix_i32(),
                seq: 1,
                unread_count: 0,
            })
            .to_bytes());
        }
        if ctor == tl_fns::updates::GetDifference::CONSTRUCTOR_ID {
            return Ok(
                tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                    date: now_unix_i32(),
                    seq: 1,
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
            let users = srv
                .users
                .values()
                .cloned()
                .map(tl_enums::User::User)
                .collect();
            return Ok(
                tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                    dialogs: Vec::new(),
                    messages: Vec::new(),
                    chats: Vec::new(),
                    users,
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID {
            return Ok(tl_enums::messages::PeerDialogs::PeerDialogs(
                tl_types::messages::PeerDialogs {
                    dialogs: Vec::new(),
                    messages: Vec::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                    state: tl_enums::updates::State::State(tl_types::updates::State {
                        pts: 10,
                        qts: 0,
                        date: now_unix_i32(),
                        seq: 1,
                        unread_count: 0,
                    }),
                },
            )
            .to_bytes());
        }

        // 1:1 Call RPCs
        if ctor == tl_fns::phone::GetCallConfig::CONSTRUCTOR_ID {
            return Ok(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: r#"{"audio_max_bitrate":32000}"#.to_owned(),
            })
            .to_bytes());
        }
        if ctor == tl_fns::messages::GetDhConfig::CONSTRUCTOR_ID {
            let req = tl_fns::messages::GetDhConfig::from_bytes(bytes).expect("decode getDhConfig");
            let mut random = vec![0u8; 256];
            for (i, b) in random.iter_mut().enumerate() {
                *b = ((i as u8).wrapping_mul(29)) ^ (uid as u8);
            }
            if req.version == 1 {
                return Ok(tl_enums::messages::DhConfig::NotModified(
                    tl_types::messages::DhConfigNotModified { random },
                )
                .to_bytes());
            }
            return Ok(
                tl_enums::messages::DhConfig::DhConfig(tl_types::messages::DhConfig {
                    g: 3,
                    p: DH_GOOD_PRIME.to_vec(),
                    version: 1,
                    random,
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::phone::RequestCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::RequestCall::from_bytes(bytes).expect("decode requestCall");
            let participant_id = match req.user_id {
                tl_enums::InputUser::InputUser(u) => u.user_id,
                _ => 0,
            };
            let call_id = srv.next_call_id;
            srv.next_call_id += 1;
            let access_hash = call_id * 31 + 17;
            let now = now_unix_i32();
            let rec = ServerCallRecord {
                id: call_id,
                access_hash,
                admin_id: uid,
                participant_id,
                video: req.video,
                g_b: None,
                protocol: req.protocol.clone(),
                date: now,
            };
            srv.calls.insert(call_id, rec);

            // Push PhoneCallRequested to callee.
            srv.push_update_to(
                participant_id,
                tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                    phone_call: tl_enums::PhoneCall::Requested(tl_types::PhoneCallRequested {
                        video: req.video,
                        id: call_id,
                        access_hash,
                        date: now,
                        admin_id: uid,
                        participant_id,
                        g_a_hash: req.g_a_hash,
                        protocol: req.protocol.clone(),
                    }),
                }),
            );

            return Ok(
                tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                        video: req.video,
                        id: call_id,
                        access_hash,
                        date: now,
                        admin_id: uid,
                        participant_id,
                        protocol: req.protocol,
                        receive_date: None,
                    }),
                    users: Vec::new(),
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::phone::ReceivedCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::ReceivedCall::from_bytes(bytes).expect("decode receivedCall");
            let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
            if let Some(rec) = srv.calls.get(&ipc.id).cloned() {
                srv.push_update_to(
                    rec.admin_id,
                    tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                        phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                            video: rec.video,
                            id: rec.id,
                            access_hash: rec.access_hash,
                            date: rec.date,
                            admin_id: rec.admin_id,
                            participant_id: rec.participant_id,
                            protocol: rec.protocol.clone(),
                            receive_date: Some(now_unix_i32()),
                        }),
                    }),
                );
            }
            return Ok(true.to_bytes());
        }
        if ctor == tl_fns::phone::AcceptCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::AcceptCall::from_bytes(bytes).expect("decode acceptCall");
            let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
            let rec = srv.calls.get_mut(&ipc.id).expect("call exists").clone();
            if let Some(entry) = srv.calls.get_mut(&ipc.id) {
                entry.g_b = Some(req.g_b.clone());
            }
            // Push PhoneCallAccepted to caller.
            srv.push_update_to(
                rec.admin_id,
                tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                    phone_call: tl_enums::PhoneCall::Accepted(tl_types::PhoneCallAccepted {
                        video: rec.video,
                        id: rec.id,
                        access_hash: rec.access_hash,
                        date: rec.date,
                        admin_id: rec.admin_id,
                        participant_id: rec.participant_id,
                        g_b: req.g_b,
                        protocol: req.protocol.clone(),
                    }),
                }),
            );
            return Ok(
                tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                        video: rec.video,
                        id: rec.id,
                        access_hash: rec.access_hash,
                        date: rec.date,
                        admin_id: rec.admin_id,
                        participant_id: rec.participant_id,
                        protocol: req.protocol,
                        receive_date: Some(now_unix_i32()),
                    }),
                    users: Vec::new(),
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::phone::ConfirmCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::ConfirmCall::from_bytes(bytes).expect("decode confirmCall");
            let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
            let rec = srv.calls.get(&ipc.id).expect("call exists").clone();
            let connections = srv.make_connections();
            let now = now_unix_i32();

            // Push PhoneCall (Active) with g_a to callee.
            srv.push_update_to(
                rec.participant_id,
                tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                    phone_call: tl_enums::PhoneCall::PhoneCall(tl_types::PhoneCall {
                        p2p_allowed: false,
                        video: rec.video,
                        conference_supported: true,
                        id: rec.id,
                        access_hash: rec.access_hash,
                        date: rec.date,
                        admin_id: rec.admin_id,
                        participant_id: rec.participant_id,
                        g_a_or_b: req.g_a.clone(),
                        key_fingerprint: req.key_fingerprint,
                        protocol: req.protocol.clone(),
                        connections: connections.clone(),
                        start_date: now,
                        custom_parameters: None,
                    }),
                }),
            );

            // Return PhoneCall (Active) with g_b to caller.
            return Ok(
                tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::PhoneCall(tl_types::PhoneCall {
                        p2p_allowed: false,
                        video: rec.video,
                        conference_supported: true,
                        id: rec.id,
                        access_hash: rec.access_hash,
                        date: rec.date,
                        admin_id: rec.admin_id,
                        participant_id: rec.participant_id,
                        g_a_or_b: rec.g_b.unwrap_or_default(),
                        key_fingerprint: req.key_fingerprint,
                        protocol: req.protocol,
                        connections,
                        start_date: now,
                        custom_parameters: None,
                    }),
                    users: Vec::new(),
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::phone::SendSignalingData::CONSTRUCTOR_ID {
            let req =
                tl_fns::phone::SendSignalingData::from_bytes(bytes).expect("decode sendSignaling");
            let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
            srv.signaling_count += 1;
            if let Some(rec) = srv.calls.get(&ipc.id).cloned() {
                let target = if uid == rec.admin_id {
                    rec.participant_id
                } else {
                    rec.admin_id
                };
                srv.push_update_to(
                    target,
                    tl_enums::Update::PhoneCallSignalingData(
                        tl_types::UpdatePhoneCallSignalingData {
                            phone_call_id: rec.id,
                            data: req.data,
                        },
                    ),
                );
            }
            return Ok(true.to_bytes());
        }
        if ctor == tl_fns::phone::DiscardCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::DiscardCall::from_bytes(bytes).expect("decode discardCall");
            let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
            srv.discards.push(RecordedDiscard {
                caller_uid: uid,
                duration: req.duration,
                reason: req.reason.clone(),
                connection_id: req.connection_id,
            });
            let disc = tl_types::PhoneCallDiscarded {
                need_rating: srv.need_rating_on_discard,
                need_debug: srv.need_debug_on_discard,
                video: req.video,
                id: ipc.id,
                reason: Some(req.reason),
                duration: Some(req.duration),
            };
            if let Some(rec) = srv.calls.get(&ipc.id).cloned() {
                let other = if uid == rec.admin_id {
                    rec.participant_id
                } else {
                    rec.admin_id
                };
                srv.push_update_to(
                    other,
                    tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                        phone_call: tl_enums::PhoneCall::Discarded(disc.clone()),
                    }),
                );
            }
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
                    phone_call: tl_enums::PhoneCall::Discarded(disc),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 2,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::SetCallRating::CONSTRUCTOR_ID {
            let req =
                tl_fns::phone::SetCallRating::from_bytes(bytes).expect("decode setCallRating");
            srv.ratings.push(RecordedRating {
                rating: req.rating,
                comment: req.comment,
            });
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 3,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::SaveCallDebug::CONSTRUCTOR_ID {
            let req =
                tl_fns::phone::SaveCallDebug::from_bytes(bytes).expect("decode saveCallDebug");
            let tl_enums::DataJSON::DataJSON(dj) = req.debug;
            srv.debugs.push(RecordedDebug {
                debug_json: dj.data,
            });
            return Ok(true.to_bytes());
        }

        // Group Call RPCs
        if ctor == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID {
            let req = tl_fns::phone::GetGroupCall::from_bytes(bytes).expect("decode getGroupCall");
            let (id, access_hash) = match req.call {
                tl_enums::InputGroupCall::InputGroupCall(c) => (c.id, c.access_hash),
                _ => (1, 1),
            };
            let gc = tl_types::GroupCall {
                join_muted: false,
                can_change_join_muted: true,
                join_date_asc: false,
                schedule_start_subscribed: false,
                can_start_video: true,
                record_video_active: false,
                rtmp_stream: false,
                listeners_hidden: false,
                conference: false,
                creator: true,
                messages_enabled: true,
                can_change_messages_enabled: true,
                min: false,
                id,
                access_hash,
                participants_count: 1,
                title: Some("Voice Lounge".to_owned()),
                stream_dc_id: srv.group_stream_dc_id,
                record_start_date: None,
                schedule_date: None,
                unmuted_video_count: Some(0),
                unmuted_video_limit: 30,
                version: 1,
                invite_link: Some("https://t.me/+groupcall".to_owned()),
                send_paid_messages_stars: None,
                default_send_as: None,
            };
            return Ok(
                tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
                    call: tl_enums::GroupCall::GroupCall(gc),
                    participants: Vec::new(),
                    participants_next_offset: String::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                })
                .to_bytes(),
            );
        }
        if ctor == tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID {
            let req =
                tl_fns::phone::JoinGroupCall::from_bytes(bytes).expect("decode joinGroupCall");
            let tl_enums::DataJSON::DataJSON(dj) = req.params;
            srv.group_join_payloads.push(dj.data.clone());
            let parsed_ssrc = serde_json::from_str::<serde_json::Value>(&dj.data)
                .ok()
                .and_then(|v| v["ssrc"].as_i64())
                .unwrap_or(12345) as i32;
            let conn_json = srv.group_join_response_json.clone();
            let is_broadcast_stream = conn_json.contains("\"stream\"");
            let self_part = tl_enums::GroupCallParticipant::GroupCallParticipant(
                tl_types::GroupCallParticipant {
                    muted: req.muted || is_broadcast_stream,
                    left: false,
                    can_self_unmute: !is_broadcast_stream,
                    just_joined: true,
                    versioned: false,
                    min: false,
                    muted_by_you: false,
                    volume_by_admin: false,
                    self_: true,
                    video_joined: true,
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid }),
                    date: now_unix_i32(),
                    active_date: None,
                    source: parsed_ssrc,
                    volume: Some(10_000),
                    about: None,
                    raise_hand_rating: None,
                    video: None,
                    presentation: None,
                    paid_stars_total: None,
                },
            );
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::GroupCallConnection(tl_types::UpdateGroupCallConnection {
                        presentation: false,
                        params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: conn_json,
                        }),
                    }),
                    tl_enums::Update::GroupCallParticipants(
                        tl_types::UpdateGroupCallParticipants {
                            call: req.call,
                            participants: vec![self_part],
                            version: 1,
                        },
                    ),
                ],
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 4,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::EditGroupCallParticipant::CONSTRUCTOR_ID {
            let req = tl_fns::phone::EditGroupCallParticipant::from_bytes(bytes)
                .expect("decode editGroupCallParticipant");
            let pid = match req.participant {
                tl_enums::InputPeer::Self_ => uid,
                tl_enums::InputPeer::User(u) => u.user_id,
                _ => 0,
            };
            srv.group_edits
                .push((pid, req.muted, req.volume, req.raise_hand));
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 5,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::GetGroupParticipants::CONSTRUCTOR_ID {
            let req = tl_fns::phone::GetGroupParticipants::from_bytes(bytes)
                .expect("decode getGroupParticipants");
            srv.group_participants_queries.push(req.sources);
            let participants = srv
                .group_resolved_participant
                .clone()
                .map(|p| vec![tl_enums::GroupCallParticipant::GroupCallParticipant(p)])
                .unwrap_or_default();
            return Ok(tl_enums::phone::GroupParticipants::GroupParticipants(
                tl_types::phone::GroupParticipants {
                    count: participants.len() as i32,
                    participants,
                    next_offset: String::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                    version: 1,
                },
            )
            .to_bytes());
        }
        if ctor == tl_fns::phone::LeaveGroupCall::CONSTRUCTOR_ID {
            let req =
                tl_fns::phone::LeaveGroupCall::from_bytes(bytes).expect("decode leaveGroupCall");
            let id = match req.call {
                tl_enums::InputGroupCall::InputGroupCall(c) => c.id,
                _ => 0,
            };
            srv.group_leaves.push((id, req.source));
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 6,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::ToggleGroupCallSettings::CONSTRUCTOR_ID {
            let req = tl_fns::phone::ToggleGroupCallSettings::from_bytes(bytes)
                .expect("decode toggleGroupCallSettings");
            srv.group_settings_toggles
                .push((req.join_muted, req.messages_enabled));
            return Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 7,
            })
            .to_bytes());
        }
        if ctor == tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID {
            return Ok(
                tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(
                    tl_types::phone::GroupCallStreamChannels {
                        channels: vec![tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(
                            tl_types::GroupCallStreamChannel {
                                channel: 1,
                                scale: 0,
                                last_timestamp_ms: 20_000,
                            },
                        )],
                    },
                )
                .to_bytes(),
            );
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let req = tl_fns::upload::GetFile::from_bytes(bytes).expect("decode upload.getFile");
            if let tl_enums::InputFileLocation::InputGroupCallStream(stream) = req.location {
                srv.broadcast_fetches
                    .push((dc_id, stream.time_ms, stream.scale));
                if srv.broadcast_fail_first_time_too_big {
                    srv.broadcast_fail_first_time_too_big = false;
                    return Err(InvocationError::Rpc(tg_api::RpcError {
                        code: 400,
                        name: "TIME_TOO_BIG".to_owned(),
                        value: None,
                    }));
                }
                return Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileUnknown,
                    mtime: now_unix_i32(),
                    bytes: srv.broadcast_chunk_bytes.clone(),
                })
                .to_bytes());
            }
        }

        Err(InvocationError::Dropped)
    }

    fn start_test_account(
        server: &Arc<Mutex<FakeCallServer>>,
        user: tl_types::User,
        peer_users: &[tl_types::User],
        extra_chats: &[tl_enums::Chat],
    ) -> (
        AccountFakeRpc,
        Arc<SessionRuntime<crate::ErasedRpcClient>>,
        LiveSession,
    ) {
        let uid = user.id;
        let rpc = AccountFakeRpc::register(server, user.clone());
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(uid),
                crate::ErasedRpcClient::new(rpc.clone()),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("wait_ready ok");

        runtime.tokio_handle().block_on(async {
            let mut s = runtime.session().write().await;
            let mut users = vec![tl_enums::User::User(user)];
            users.extend(peer_users.iter().cloned().map(tl_enums::User::User));
            apply_tl_users_and_chats(&mut s, &users, extra_chats);
        });

        let mut live = LiveSession::new(Arc::clone(&runtime), uid as u64);
        live.set_calls_media_config(make_virtual_media_config(), make_virtual_group_config());
        (rpc, runtime, live)
    }

    fn shutdown_account(runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>, live: LiveSession) {
        drop(live);
        if let Ok(rt) = Arc::try_unwrap(runtime) {
            rt.shutdown();
        }
    }

    // =========================================================================
    // Test 1: Full 2-account 1:1 call over FakeRpcClient + LoopbackReflector
    // =========================================================================

    #[test]
    fn live_calls_two_party_outgoing_incoming_established_audio_and_discard_rating_debug() {
        let reflector_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("reflector runtime");
        let reflector = reflector_rt
            .block_on(LoopbackReflector::start(ReflectorImpairment::default()))
            .expect("start LoopbackReflector");

        let server = FakeCallServer::new(Some(reflector.addr()));
        let alice_user = make_tl_user(700_001, "Alice", "alice_call", true);
        let bob_user = make_tl_user(700_002, "Bob", "bob_call", true);
        let bob_peer = PeerId::from_user(bob_user.id as u64);

        let (alice_rpc, alice_rt, mut alice) = start_test_account(
            &server,
            alice_user.clone(),
            std::slice::from_ref(&bob_user),
            &[],
        );
        let (bob_rpc, bob_rt, mut bob) = start_test_account(
            &server,
            bob_user.clone(),
            std::slice::from_ref(&alice_user),
            &[],
        );

        // 1. Alice places outgoing call -> confirms -> DH + phone.requestCall.
        let mut now_ms = 1_000u64;
        alice.start_outgoing_call(bob_peer, false, now_ms);
        assert_eq!(
            alice.active_call().expect("alice snap").state,
            CallState::WaitingUserConfirmation
        );
        alice.confirm_outgoing_call(false, now_ms);

        // 2. Pump until Bob sees WaitingIncoming (after phone.receivedCall) and Alice sees Ringing.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            let a_ringing = alice
                .active_call()
                .is_some_and(|c| c.state == CallState::Ringing);
            let b_waiting = bob
                .active_call()
                .is_some_and(|c| c.state == CallState::WaitingIncoming);
            if a_ringing && b_waiting {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(
            alice.active_call().expect("alice active").state,
            CallState::Ringing
        );
        assert_eq!(
            bob.active_call().expect("bob active").state,
            CallState::WaitingIncoming
        );

        // 3. Bob answers -> phone.acceptCall -> Alice phone.confirmCall -> both start MediaEngines!
        bob.answer_call(now_ms);

        let mut audio_pushed = false;
        let mut bob_received_pcm = Vec::new();
        let mut alice_received_pcm = Vec::new();
        let mut both_established_at = None;

        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            now_ms += 25;
            alice.tick(now_ms);
            bob.tick(now_ms);

            if !audio_pushed
                && let (Some(a_va), Some(b_va)) =
                    (alice.calls_virtual_audio(), bob.calls_virtual_audio())
            {
                a_va.push_capture_pcm(&generate_sine(440.0, 1800, 0.5));
                b_va.push_capture_pcm(&generate_sine(880.0, 1800, 0.4));
                audio_pushed = true;
            }
            if let Some(a_va) = alice.calls_virtual_audio() {
                alice_received_pcm.extend_from_slice(&a_va.drain_playback_pcm());
            }
            if let Some(b_va) = bob.calls_virtual_audio() {
                bob_received_pcm.extend_from_slice(&b_va.drain_playback_pcm());
            }

            let a_est = alice
                .active_call()
                .is_some_and(|c| c.state == CallState::Established);
            let b_est = bob
                .active_call()
                .is_some_and(|c| c.state == CallState::Established);
            if a_est && b_est {
                let start = *both_established_at.get_or_insert_with(Instant::now);
                if start.elapsed() >= Duration::from_millis(1_100)
                    && bob_received_pcm.len() >= VOIP_SAMPLE_RATE as usize / 2
                    && alice_received_pcm.len() >= VOIP_SAMPLE_RATE as usize / 2
                {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Verify both sides reached Established with identical 4-emoji fingerprints!
        let alice_snap = alice.active_call().expect("alice established snap").clone();
        let bob_snap = bob.active_call().expect("bob established snap").clone();
        assert_eq!(alice_snap.state, CallState::Established);
        assert_eq!(bob_snap.state, CallState::Established);
        assert!(
            alice_snap.emoji_fingerprint.is_some(),
            "emoji fingerprint must be computed from real DH key"
        );
        assert_eq!(
            alice_snap.emoji_fingerprint, bob_snap.emoji_fingerprint,
            "both sides must derive the exact same 4-emoji fingerprint"
        );

        // Verify real audio frames flowed in both directions over LoopbackReflector!
        let refl_stats = reflector.stats();
        assert!(
            refl_stats.packets_forwarded >= 20,
            "LoopbackReflector must forward VoIP packets, got {}",
            refl_stats.packets_forwarded
        );
        let skip = (VOIP_SAMPLE_RATE as usize) / 4;
        assert!(
            bob_received_pcm.len() > skip + 4800,
            "Bob must receive decoded audio samples, got {}",
            bob_received_pcm.len()
        );
        let bob_mag_440 = goertzel_magnitude(&bob_received_pcm[skip..], 440.0);
        let bob_mag_1000 = goertzel_magnitude(&bob_received_pcm[skip..], 1000.0);
        assert!(
            bob_mag_440 > 0.08 && bob_mag_440 > bob_mag_1000 * 6.0,
            "Bob must decode Alice's 440 Hz audio (mag_440={bob_mag_440:.4}, mag_1000={bob_mag_1000:.4})"
        );
        let alice_mag_880 = goertzel_magnitude(&alice_received_pcm[skip..], 880.0);
        let alice_mag_300 = goertzel_magnitude(&alice_received_pcm[skip..], 300.0);
        assert!(
            alice_mag_880 > 0.06 && alice_mag_880 > alice_mag_300 * 5.0,
            "Alice must decode Bob's 880 Hz audio (mag_880={alice_mag_880:.4}, mag_300={alice_mag_300:.4})"
        );

        // 4. Alice hangs up -> phone.discardCall(Hangup, duration, relay_id=777) ->
        //    phone.saveCallDebug + rating UI -> submit_call_rating -> phone.setCallRating!
        now_ms += 2_000;
        alice.hangup_call(now_ms);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            let ready = alice.active_call().is_some_and(|c| c.rating_requested)
                && !server.lock().expect("srv").debugs.is_empty();
            if ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let alice_after = alice.active_call().expect("alice rating snap");
        assert!(alice_after.rating_requested, "rating card must be shown");

        alice.submit_call_rating(5, "Great loopback call!".to_owned());
        alice.finish_call_rating();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            alice.tick(now_ms);
            if !server.lock().expect("srv").ratings.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        {
            let srv = server.lock().expect("srv");
            assert!(
                srv.signaling_count >= 2,
                "phone.sendSignalingData must be invoked both ways"
            );
            assert!(
                srv.discards.iter().any(|d| {
                    d.caller_uid == alice_user.id
                        && d.reason == tl_enums::PhoneCallDiscardReason::Hangup
                        && d.duration >= 1
                        && d.connection_id == 777
                }),
                "expected Hangup discard with duration >= 1 and relay 777, got: {:?}",
                srv.discards
            );
            assert!(
                !srv.debugs.is_empty(),
                "expected phone.saveCallDebug to upload debug JSON"
            );
            let parsed_dbg: serde_json::Value =
                serde_json::from_str(&srv.debugs[0].debug_json).expect("valid debug JSON");
            assert_eq!(parsed_dbg["lib"], "tg-voip");
            assert_eq!(srv.ratings.len(), 1);
            assert_eq!(srv.ratings[0].rating, 5);
            assert_eq!(srv.ratings[0].comment, "Great loopback call!");
        }

        // Verify expected TL constructors fired.
        let a_ctors = alice_rpc.recorded_ctors();
        let b_ctors = bob_rpc.recorded_ctors();
        assert!(a_ctors.contains(&tl_fns::messages::GetDhConfig::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::RequestCall::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::ConfirmCall::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::SendSignalingData::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::DiscardCall::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::SaveCallDebug::CONSTRUCTOR_ID));
        assert!(a_ctors.contains(&tl_fns::phone::SetCallRating::CONSTRUCTOR_ID));
        assert!(b_ctors.contains(&tl_fns::phone::ReceivedCall::CONSTRUCTOR_ID));
        assert!(b_ctors.contains(&tl_fns::phone::AcceptCall::CONSTRUCTOR_ID));
        assert!(b_ctors.contains(&tl_fns::phone::SendSignalingData::CONSTRUCTOR_ID));

        shutdown_account(alice_rt, alice);
        shutdown_account(bob_rt, bob);
        reflector_rt.block_on(reflector.stop());
        reflector_rt.shutdown_timeout(Duration::from_secs(2));
    }

    // =========================================================================
    // Test 2: Discard reasons (Missed on outgoing ring, Busy on incoming decline)
    // =========================================================================

    #[test]
    fn live_calls_discard_sends_missed_and_busy_reasons() {
        let server = FakeCallServer::new(None);
        server.lock().expect("srv").need_rating_on_discard = false;
        server.lock().expect("srv").need_debug_on_discard = false;

        let alice_user = make_tl_user(710_001, "Alice", "alice_disc", true);
        let bob_user = make_tl_user(710_002, "Bob", "bob_disc", true);
        let bob_peer = PeerId::from_user(bob_user.id as u64);

        let (_alice_rpc, alice_rt, mut alice) = start_test_account(
            &server,
            alice_user.clone(),
            std::slice::from_ref(&bob_user),
            &[],
        );
        let (_bob_rpc, bob_rt, mut bob) = start_test_account(
            &server,
            bob_user.clone(),
            std::slice::from_ref(&alice_user),
            &[],
        );

        // Case A: Outgoing ringing call cancelled by caller -> DiscardReason::Missed.
        let mut now_ms = 5_000u64;
        alice.start_outgoing_call(bob_peer, false, now_ms);
        alice.confirm_outgoing_call(false, now_ms);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            if alice
                .active_call()
                .is_some_and(|c| c.state == CallState::Ringing)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        alice.hangup_call(now_ms);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            if !server.lock().expect("srv").discards.is_empty() && bob.active_call().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(
            server.lock().expect("srv").discards[0].reason,
            tl_enums::PhoneCallDiscardReason::Missed
        );

        // Case B: Incoming ringing call declined by callee -> DiscardReason::Busy.
        server.lock().expect("srv").discards.clear();
        alice.start_outgoing_call(bob_peer, false, now_ms);
        alice.confirm_outgoing_call(false, now_ms);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            if bob
                .active_call()
                .is_some_and(|c| c.state == CallState::WaitingIncoming)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        bob.hangup_call(now_ms);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            now_ms += 20;
            alice.tick(now_ms);
            bob.tick(now_ms);
            if !server.lock().expect("srv").discards.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(
            server.lock().expect("srv").discards[0].reason,
            tl_enums::PhoneCallDiscardReason::Busy
        );

        shutdown_account(alice_rt, alice);
        shutdown_account(bob_rt, bob);
    }

    // =========================================================================
    // Test 3: Group call join payload, fake join response, participants, edit, leave
    // =========================================================================

    #[test]
    fn live_group_call_join_payload_response_edit_and_leave() {
        let server = FakeCallServer::new(None);
        {
            let mut srv = server.lock().expect("srv");
            // Minimal valid SFU RTC join response JSON (`parse_join_response`).
            srv.group_join_response_json = serde_json::json!({
                "transport": {
                    "ufrag": "sfuUfrag",
                    "pwd": "sfuPassword12345678901234",
                    "fingerprints": [{
                        "hash": "sha-256",
                        "setup": "passive",
                        "fingerprint": "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
                    }],
                    "candidates": [{
                        "generation": "0",
                        "component": "1",
                        "protocol": "udp",
                        "port": "14000",
                        "ip": "127.0.0.1",
                        "foundation": "1",
                        "id": "1",
                        "priority": "2122260223",
                        "type": "host",
                        "network": "1"
                    }]
                },
                "audio": {
                    "payload-types": [{
                        "id": 111,
                        "name": "opus",
                        "clockrate": 48000,
                        "channels": 2
                    }],
                    "rtp-hdrexts": []
                }
            })
            .to_string();
        }

        let self_user = make_tl_user(720_001, "Alice", "alice_gc", true);
        let peer_user = make_tl_user(720_002, "Charlie", "charlie_gc", false);
        let channel_id = 880_001i64;
        let group_peer = PeerId::from_channel(channel_id as u64);
        let self_peer = PeerId::from_user(self_user.id as u64);
        let charlie_peer = PeerId::from_user(peer_user.id as u64);
        let call_id = CallId(55_555);
        let access_hash = 999_888i64;

        let (rpc, rt, mut live) = start_test_account(
            &server,
            self_user.clone(),
            std::slice::from_ref(&peer_user),
            &[make_tl_channel(channel_id, "Rust Lounge")],
        );

        live.register_known_group_call(group_peer, call_id, access_hash, true, None);
        let mut now_ms = 10_000u64;
        live.join_group_call(group_peer, self_peer, now_ms);

        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            now_ms += 20;
            live.tick(now_ms);
            let joined_or_connecting = live.active_group_call().is_some_and(|g| {
                matches!(g.state, GroupCallState::Connecting | GroupCallState::Joined)
                    && !g.members.is_empty()
            });
            if joined_or_connecting {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        // Verify phone.joinGroupCall sent authentic GroupMediaEngine payload JSON.
        {
            let srv = server.lock().expect("srv");
            assert_eq!(srv.group_join_payloads.len(), 1);
            let payload: serde_json::Value =
                serde_json::from_str(&srv.group_join_payloads[0]).expect("valid join payload JSON");
            assert!(payload["ssrc"].as_i64().unwrap_or(0) != 0);
            assert!(
                !payload["ufrag"].as_str().unwrap_or("").is_empty(),
                "join payload must include ICE ufrag"
            );
            assert!(
                payload["fingerprints"]
                    .as_array()
                    .is_some_and(|f| !f.is_empty()),
                "join payload must include DTLS fingerprint"
            );
        }

        let snap = live.active_group_call().expect("group snapshot");
        assert_eq!(snap.title, "Voice Lounge");
        assert_eq!(snap.members.len(), 1);
        assert!(snap.members[0].is_self);

        // Push a remote participant via updateGroupCallParticipants.
        server.lock().expect("srv").push_update_to(
            self_user.id,
            tl_enums::Update::GroupCallParticipants(tl_types::UpdateGroupCallParticipants {
                call: tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                    id: call_id.to_tl(),
                    access_hash,
                }),
                participants: vec![tl_enums::GroupCallParticipant::GroupCallParticipant(
                    tl_types::GroupCallParticipant {
                        muted: false,
                        left: false,
                        can_self_unmute: true,
                        just_joined: true,
                        versioned: false,
                        min: false,
                        muted_by_you: false,
                        volume_by_admin: false,
                        self_: false,
                        video_joined: false,
                        peer: tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: peer_user.id,
                        }),
                        date: now_unix_i32(),
                        active_date: Some(now_unix_i32()),
                        source: 77_001,
                        volume: Some(10_000),
                        about: None,
                        raise_hand_rating: None,
                        video: None,
                        presentation: None,
                        paid_stars_total: None,
                    },
                )],
                version: 1,
            }),
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            now_ms += 20;
            live.tick(now_ms);
            if live
                .active_group_call()
                .is_some_and(|g| g.members.len() == 2)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(live.active_group_call().expect("group").members.len(), 2);

        // Exercise self unmute, remote participant volume, admin settings, and leave.
        live.set_group_mute_state(MuteState::Active, now_ms);
        live.set_participant_volume(charlie_peer, 15_000);
        live.set_group_join_muted(true);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            now_ms += 20;
            live.tick(now_ms);
            let srv = server.lock().expect("srv");
            if srv.group_edits.len() >= 2 && !srv.group_settings_toggles.is_empty() {
                break;
            }
            drop(srv);
            std::thread::sleep(Duration::from_millis(15));
        }

        live.leave_group_call(false, now_ms);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            now_ms += 20;
            live.tick(now_ms);
            if !server.lock().expect("srv").group_leaves.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        assert!(live.active_group_call().is_none());
        {
            let srv = server.lock().expect("srv");
            assert!(
                srv.group_edits
                    .iter()
                    .any(|&(pid, muted, _, _)| pid == self_user.id && muted == Some(false)),
                "expected self unmute editGroupCallParticipant, got {:?}",
                srv.group_edits
            );
            assert!(
                srv.group_edits
                    .iter()
                    .any(|&(pid, _, vol, _)| pid == peer_user.id && vol == Some(15_000)),
                "expected Charlie volume=15000 editGroupCallParticipant, got {:?}",
                srv.group_edits
            );
            assert_eq!(srv.group_settings_toggles, vec![(Some(true), None)]);
            assert_eq!(srv.group_leaves.len(), 1);
            assert_eq!(srv.group_leaves[0].0, call_id.to_tl());
        }
        let ctors = rpc.recorded_ctors();
        assert!(ctors.contains(&tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::phone::EditGroupCallParticipant::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::phone::ToggleGroupCallSettings::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::phone::LeaveGroupCall::CONSTRUCTOR_ID));

        shutdown_account(rt, live);
    }

    // =========================================================================
    // Test 4: Broadcast stream path via upload.getFile(inputGroupCallStream)
    // =========================================================================

    #[test]
    fn live_group_call_broadcast_stream_fetches_chunks_via_upload_get_file() {
        let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).expect("opus encoder");
        let mut packets = Vec::new();
        for f in 0..10usize {
            let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
            for (i, s) in pcm.iter_mut().enumerate() {
                let t = (f * VOIP_FRAME_SAMPLES + i) as f32 / VOIP_SAMPLE_RATE as f32;
                *s = (2.0 * std::f32::consts::PI * 500.0 * t).sin() * 0.45;
            }
            packets.push(enc.encode_frame(&pcm).expect("encode opus"));
        }
        let ogg_chunk = encode_ogg_opus_chunk(&packets);

        let server = FakeCallServer::new(None);
        {
            let mut srv = server.lock().expect("srv");
            srv.group_join_response_json = r#"{"stream":true}"#.to_owned();
            srv.group_stream_dc_id = Some(4);
            srv.broadcast_chunk_bytes = ogg_chunk;
            srv.broadcast_fail_first_time_too_big = true;
        }

        let self_user = make_tl_user(730_001, "Listener", "listener_bc", true);
        let channel_id = 890_001i64;
        let group_peer = PeerId::from_channel(channel_id as u64);
        let self_peer = PeerId::from_user(self_user.id as u64);
        let call_id = CallId(66_666);
        let access_hash = 123_456i64;

        let (_rpc, rt, mut live) = start_test_account(
            &server,
            self_user,
            &[],
            &[make_tl_channel(channel_id, "Livestream Channel")],
        );
        live.register_known_group_call(group_peer, call_id, access_hash, false, Some(4));

        let mut now_ms = 20_000u64;
        live.join_group_call(group_peer, self_peer, now_ms);

        let mut received_pcm = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            now_ms += 25;
            live.tick(now_ms);
            if let Some(va) = live.group_calls_virtual_audio() {
                received_pcm.extend_from_slice(&va.drain_playback_pcm());
            }
            let connected = live.active_group_call().is_some_and(|g| {
                g.state == GroupCallState::Joined && g.instance_state == InstanceState::Connected
            });
            if connected && received_pcm.len() >= VOIP_FRAME_SAMPLES * 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let snap = live.active_group_call().expect("broadcast group snapshot");
        assert_eq!(snap.state, GroupCallState::Joined);
        assert_eq!(snap.instance_state, InstanceState::Connected);

        let fetches = server.lock().expect("srv").broadcast_fetches.clone();
        assert!(
            fetches.len() >= 2,
            "expected TIME_TOO_BIG retry + successful upload.getFile(inputGroupCallStream), got {fetches:?}"
        );
        assert!(
            fetches.iter().all(|&(dc, _, _)| dc == 4),
            "broadcast stream chunks must be fetched on stream_dc_id=4, got {fetches:?}"
        );

        let mag_500 = goertzel_magnitude(&received_pcm, 500.0);
        assert!(
            mag_500 > 0.05,
            "broadcast stream playback must decode 500 Hz Opus audio (mag_500={mag_500:.4})"
        );

        live.leave_group_call(false, now_ms);
        shutdown_account(rt, live);
    }
}
