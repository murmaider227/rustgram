//! Top-level 1:1 call coordinator (`Calls::Instance`).
//!
//! Port of `calls/calls_instance.h` and `calls/calls_instance.cpp` (active call
//! management, DH config caching/refresh, 24h call config refresh, incoming
//! `phoneCallRequested` gating, busy rejection, and stale-call filtering).

use crate::call::{Call, CallAction, CallState, CallType};
use crate::dh::{DhConfigCache, RANDOM_POWER_SIZE};
use crate::protocol::{CallProtocolConfig, SERVER_CONFIG_UPDATE_TIMEOUT_MS, ServerCallConfig};
use crate::types::{
    DhConfigResponse, InputPhoneCall, PhoneCallUpdate, UpdatePhoneCallSignalingData, UserId,
};

/// Actions emitted by [`CallsInstance`] for the surrounding API/UI layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceAction {
    /// Request `messages.getDhConfig(version, random_length = 256)`.
    GetDhConfig {
        /// Current cached DH config version.
        version: i32,
        /// Always 256 (`RANDOM_POWER_SIZE`).
        random_length: i32,
    },
    /// Request `phone.getCallConfig` (refreshed at most once every 24 hours).
    GetCallConfig,
    /// Reject an incoming call immediately with `phone.discardCall(Busy)` because the user
    /// is already in a call or group call.
    DiscardIncomingBusy {
        /// Whether the incoming call was a video call.
        video: bool,
        /// Incoming call handle.
        peer: InputPhoneCall,
    },
    /// An action forwarded from the active 1:1 [`Call`].
    Call(CallAction),
}

/// Coordinates active 1:1 calls, cached DH parameters, and incoming call filtering.
///
/// Port of the protocol logic in `Calls::Instance` (`calls_instance.cpp`).
#[derive(Debug, Clone)]
pub struct CallsInstance {
    self_user_id: UserId,
    server_config: ServerCallConfig,
    protocol_config: CallProtocolConfig,
    dh_cache: DhConfigCache,
    last_server_config_update_ms: Option<u64>,
    server_config_request_in_flight: bool,
    in_group_call: bool,
    current_call: Option<Call>,
    actions: Vec<InstanceAction>,
}

impl CallsInstance {
    /// Create a new call coordinator for the logged-in `self_user_id`.
    pub fn new(
        self_user_id: UserId,
        server_config: ServerCallConfig,
        protocol_config: CallProtocolConfig,
    ) -> Self {
        Self {
            self_user_id,
            server_config,
            protocol_config,
            dh_cache: DhConfigCache::new(),
            last_server_config_update_ms: None,
            server_config_request_in_flight: false,
            in_group_call: false,
            current_call: None,
            actions: Vec::new(),
        }
    }

    /// Drain all queued [`InstanceAction`]s.
    pub fn take_actions(&mut self) -> Vec<InstanceAction> {
        self.flush_call_actions();
        std::mem::take(&mut self.actions)
    }

    /// Inspect the active 1:1 call, if any.
    pub fn current_call(&self) -> Option<&Call> {
        self.current_call.as_ref()
    }

    /// Mutably access the active 1:1 call, if any.
    pub fn current_call_mut(&mut self) -> Option<&mut Call> {
        self.current_call.as_mut()
    }

    /// Update whether a group call is currently active (used to reject incoming 1:1 calls
    /// with `Busy`, matching `calls_instance.cpp:705`).
    pub fn set_in_group_call(&mut self, in_group_call: bool) {
        self.in_group_call = in_group_call;
    }

    /// Update server call timeout configuration from `help.getConfig`.
    pub fn set_server_config(&mut self, config: ServerCallConfig) {
        self.server_config = config;
    }

    /// Returns `true` if a 1:1 call is currently active.
    ///
    /// Exact port of `Instance::inCall` (`calls_instance.cpp:842-849`):
    /// excludes `rating_in_panel`, `Busy`, and `WaitingUserConfirmation`.
    pub fn in_call(&self) -> bool {
        let Some(call) = &self.current_call else {
            return false;
        };
        if call.rating_in_panel() {
            return false;
        }
        !matches!(
            call.state(),
            CallState::Busy | CallState::WaitingUserConfirmation
        )
    }

    /// Start an outgoing 1:1 call.
    ///
    /// If `is_confirmed` is `true`, transitions immediately to `Requesting` and emits
    /// `GetCallConfig` (if stale) and `GetDhConfig`.
    /// Port of `Instance::createCall` (`calls_instance.cpp:424-474`).
    pub fn start_outgoing_call(
        &mut self,
        peer_user_id: UserId,
        video: bool,
        is_confirmed: bool,
        now_ms: u64,
    ) -> bool {
        if self.in_call() {
            return false;
        }
        self.replace_current_call(
            Call::new_outgoing(
                self.self_user_id,
                peer_user_id,
                video,
                self.server_config,
                self.protocol_config.clone(),
            ),
            is_confirmed,
            now_ms,
        );
        true
    }

    /// Confirm an outgoing call that was created in `WaitingUserConfirmation`, triggering
    /// `phone.getCallConfig` and `messages.getDhConfig`.
    pub fn confirm_outgoing_call(&mut self, now_ms: u64) {
        let should_refresh = if let Some(call) = &mut self.current_call {
            if call.state() == CallState::WaitingUserConfirmation {
                call.apply_user_confirmation();
                true
            } else {
                false
            }
        } else {
            false
        };
        self.flush_call_actions();
        if should_refresh {
            self.refresh_server_config(now_ms);
            self.refresh_dh_config();
        }
    }

    /// Handle an incoming `updatePhoneCall`.
    ///
    /// Port of `Instance::handleCallUpdate` (`calls_instance.cpp:680-728`).
    pub fn handle_call_update(&mut self, update: PhoneCallUpdate, now_unix_sec: i32, now_ms: u64) {
        if let PhoneCallUpdate::Requested(ref req) = update {
            // Ignore repeated phoneCallRequested (e.g. from updates.getDifference).
            if let Some(current) = &self.current_call
                && current.peer_user_id() == req.admin_id
                && current.id() == req.id
            {
                return;
            }

            // Busy if already in another call/group call, or invalid self caller.
            if self.in_call() || self.in_group_call || req.admin_id == self.self_user_id {
                self.actions.push(InstanceAction::DiscardIncomingBusy {
                    video: req.video,
                    peer: InputPhoneCall {
                        id: req.id,
                        access_hash: req.access_hash,
                    },
                });
                return;
            }

            // Ignore calls older than callRingTimeoutMs.
            let ring_timeout_sec = (self.server_config.call_ring_timeout_ms / 1000) as i32;
            if req.date.saturating_add(ring_timeout_sec) < now_unix_sec {
                return;
            }

            let incoming = Call::new_incoming(
                self.self_user_id,
                req.admin_id,
                req.video,
                self.server_config,
                self.protocol_config.clone(),
            );
            self.replace_current_call(incoming, false, now_ms);
            if let Some(call) = &mut self.current_call {
                call.handle_update(update, now_ms);
            }
            self.flush_call_actions();
            return;
        }

        if let Some(call) = &mut self.current_call {
            call.handle_update(update, now_ms);
        }
        self.flush_call_actions();
    }

    /// Route an incoming `updatePhoneCallSignalingData` to the active call.
    ///
    /// Port of `Instance::handleSignalingData` (`calls_instance.cpp:831-840`).
    pub fn handle_signaling_data(&mut self, data: UpdatePhoneCallSignalingData) -> bool {
        let handled = self
            .current_call
            .as_mut()
            .is_some_and(|call| call.handle_signaling_data(data));
        self.flush_call_actions();
        handled
    }

    /// Process a `messages.getDhConfig` response and start the active call.
    ///
    /// Port of `Instance::refreshDhConfig` `.done()` / `.fail()` (`calls_instance.cpp:523-541`).
    pub fn handle_dh_config_response(&mut self, response: Result<DhConfigResponse, ()>) {
        match response {
            Ok(resp) => match self.dh_cache.update(resp) {
                Ok(random) => {
                    let config = self.dh_cache.config();
                    if let Some(call) = &mut self.current_call {
                        call.start(config, &random);
                    }
                    self.flush_call_actions();
                }
                Err(_) => {
                    self.destroy_current_call_failed();
                }
            },
            Err(()) => {
                self.destroy_current_call_failed();
            }
        }
    }

    /// Deterministic variant of [`Self::handle_dh_config_response`] for unit tests.
    pub fn handle_dh_config_response_with_rng<F>(
        &mut self,
        response: Result<DhConfigResponse, ()>,
        random_id: i32,
        fill_local_random: F,
    ) where
        F: FnMut(&mut [u8; RANDOM_POWER_SIZE]),
    {
        match response {
            Ok(resp) => match self.dh_cache.update(resp) {
                Ok(random) => {
                    let config = self.dh_cache.config();
                    if let Some(call) = &mut self.current_call {
                        call.start_with_rng(config, &random, random_id, fill_local_random);
                    }
                    self.flush_call_actions();
                }
                Err(_) => {
                    self.destroy_current_call_failed();
                }
            },
            Err(()) => {
                self.destroy_current_call_failed();
            }
        }
    }

    /// Mark `phone.getCallConfig` completed (`calls_instance.cpp:588-596`).
    pub fn handle_call_config_response(&mut self, ok: bool, now_ms: u64) {
        self.server_config_request_in_flight = false;
        if ok {
            self.last_server_config_update_ms = Some(now_ms);
        }
    }

    /// Trigger redial on a `Busy` call, requesting a fresh DH config.
    pub fn redial_current_call(&mut self) {
        let redialed = if let Some(call) = &mut self.current_call {
            if call.state() == CallState::Busy {
                call.redial();
                true
            } else {
                false
            }
        } else {
            false
        };
        self.flush_call_actions();
        if redialed {
            self.refresh_dh_config();
        }
    }

    /// Destroy the active call after `NotifyFinished` / `NotifyFailed` (matching
    /// `Instance::destroyCall`, `calls_instance.cpp:404-422`).
    pub fn destroy_current_call(&mut self) {
        if let Some(call) = &self.current_call
            && call.rating_in_panel()
        {
            return;
        }
        self.current_call = None;
    }

    /// Returns whether the active call prevents application quit (`Instance::isQuitPrevent`,
    /// `calls_instance.cpp:668-678`).
    pub fn is_quit_prevent(&mut self, now_ms: u64) -> bool {
        let Some(call) = &mut self.current_call else {
            return false;
        };
        if call.is_incoming_waiting() {
            return false;
        }
        call.hangup(None, now_ms, false);
        let ended_immediately = matches!(
            call.state(),
            CallState::Ended | CallState::EndedByOtherDevice | CallState::Failed | CallState::Busy
        );
        self.flush_call_actions();
        if ended_immediately {
            self.destroy_current_call();
        }
        self.current_call.is_some()
    }

    fn replace_current_call(&mut self, mut call: Call, is_confirmed: bool, now_ms: u64) {
        if is_confirmed {
            call.apply_user_confirmation();
        }
        if let Some(mut previous) = self.current_call.take() {
            if previous.rating_in_panel() {
                previous.finish_rating();
            } else {
                previous.hangup(None, now_ms, false);
            }
            for action in previous.take_actions() {
                self.actions.push(InstanceAction::Call(action));
            }
        }
        let needs_refresh = call.state() != CallState::WaitingUserConfirmation
            && !call.is_conference_invite()
            && (call.call_type() == CallType::Incoming || call.state() == CallState::Requesting);
        self.current_call = Some(call);
        self.flush_call_actions();
        if needs_refresh {
            self.refresh_server_config(now_ms);
            self.refresh_dh_config();
        }
    }

    fn refresh_dh_config(&mut self) {
        self.actions.push(InstanceAction::GetDhConfig {
            version: self.dh_cache.version(),
            random_length: RANDOM_POWER_SIZE as i32,
        });
    }

    fn refresh_server_config(&mut self, now_ms: u64) {
        if self.server_config_request_in_flight {
            return;
        }
        if let Some(last) = self.last_server_config_update_ms
            && now_ms.saturating_sub(last) < SERVER_CONFIG_UPDATE_TIMEOUT_MS
        {
            return;
        }
        self.server_config_request_in_flight = true;
        self.actions.push(InstanceAction::GetCallConfig);
    }

    fn destroy_current_call_failed(&mut self) {
        // Match `Instance::refreshDhConfig` (`calls/calls_instance.cpp:523-541`), which checks
        // `if (!call) { return; }` before failing the call when `messages.getDhConfig` completes
        // after the call was already destroyed.
        if self.current_call.is_none() {
            return;
        }
        self.current_call = None;
        self.actions
            .push(InstanceAction::Call(CallAction::NotifyFailed));
    }

    fn flush_call_actions(&mut self) {
        if let Some(call) = &mut self.current_call {
            for action in call.take_actions() {
                self.actions.push(InstanceAction::Call(action));
            }
        }
    }
}
