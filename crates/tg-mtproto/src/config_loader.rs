//! Port of `mtproto/config_loader.cpp` and periodic `help.getConfig` scheduling from
//! `mtproto/mtp_instance.cpp` (`requestConfig`, `requestConfigIfOld`, `requestConfigIfExpired`,
//! `configLoadDone`).

use std::time::{Duration, Instant};

use rand::Rng;
use tg_tl::enums::Config as TlConfigEnum;
use tg_tl::functions::help::GetConfig;
use tg_tl::types::Config as TlConfig;
use tg_transport::ProxyType;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::client::{Client, InvokeOptions};
use crate::dc_options::{DcEndpoint, DcId, DcOptionFlags, config_dc_id, temporary_dc_id_from_real};
use crate::error::InvocationError;
use crate::special_config::{SpecialEndpoint, fetch_special_config};

/// Timeout before enumerating to the next DC (`kEnumerateDcTimeout = 8000ms`).
pub const ENUMERATE_DC_TIMEOUT: Duration = Duration::from_secs(8);
/// Timeout per specially-fetched temporary DC endpoint (`kSpecialRequestTimeoutMs = 6000ms`).
pub const SPECIAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
/// Minimum age before `request_config_if_old` reloads normal config (`kConfigBecomesOldIn = 120s`).
pub const CONFIG_BECOMES_OLD: Duration = Duration::from_secs(120);
/// Minimum age before `request_config_if_old` reloads when in `blocked_mode` (`8s`).
pub const CONFIG_BECOMES_OLD_FOR_BLOCKED: Duration = Duration::from_secs(8);
/// Maximum single sleep slice when awaiting config expiry (`3600s` in `requestConfigIfExpired`).
pub const MAX_EXPIRE_TIMER_STEP: Duration = Duration::from_secs(3600);

/// Tuning parameters for [`ConfigLoader`].
#[derive(Debug, Clone, Copy)]
pub struct ConfigLoaderOptions {
    /// Delay before moving from main DC to DC enumeration + special fallback (default 8s).
    pub enumerate_dc_timeout: Duration,
    /// Timeout for each temporary DC `help.getConfig` probe (default 6s).
    pub special_request_timeout: Duration,
    /// Enable Firebase-free DoH fallback config requests when no proxy is active (default `true`).
    pub enable_doh_fallback: bool,
    /// Maximum DC enumeration rounds before returning an error (default `2`).
    pub max_enum_rounds: usize,
}

impl Default for ConfigLoaderOptions {
    fn default() -> Self {
        Self {
            enumerate_dc_timeout: ENUMERATE_DC_TIMEOUT,
            special_request_timeout: SPECIAL_REQUEST_TIMEOUT,
            enable_doh_fallback: true,
            max_enum_rounds: 2,
        }
    }
}

/// Loads `help.getConfig` from the main DC with automatic DC enumeration and Firebase-free DoH
/// special-config fallback (`MTP::details::ConfigLoader`).
pub struct ConfigLoader {
    client: Client,
    options: ConfigLoaderOptions,
}

impl ConfigLoader {
    /// Creates a new `ConfigLoader` for `client`.
    pub fn new(client: Client, options: ConfigLoaderOptions) -> Self {
        Self { client, options }
    }

    /// Attempts to load a fresh [`TlConfig`], applying it to `client` upon success.
    ///
    /// Mirrors `ConfigLoader::load` / `enumerate` / `sendSpecialRequest`:
    /// 1. Sends `help.getConfig` to the current main DC.
    /// 2. If no response arrives within `enumerate_dc_timeout`, begins cycling through
    ///    `dc_options.config_enum_dc_ids()` using `config_dc_id(dc)` sessions and (when no
    ///    proxy is enabled and `enable_doh_fallback` is true) launches a Firebase-free DoH
    ///    `SpecialConfigRequest` in parallel.
    /// 3. Special endpoints received from DoH are probed on temporary DCs (`dc_id + 1000`);
    ///    any returned `dc_options` are applied immediately to unblock the main DC.
    pub async fn load(&self) -> Result<TlConfig, InvocationError> {
        let main_dc = self.client.main_dc_id();
        let main_fut = self.send_get_config(main_dc);
        tokio::pin!(main_fut);

        // Phase 1: wait up to `enumerate_dc_timeout` for the main DC.
        tokio::select! {
            res = &mut main_fut => {
                if let Ok(cfg) = res {
                    self.client.apply_tl_config(&cfg);
                    return Ok(cfg);
                }
            }
            () = tokio::time::sleep(self.options.enumerate_dc_timeout) => {
                debug!(main_dc, "main DC help.getConfig timed out, starting DC enumeration");
            }
        }

        // Phase 2: enumerate other DCs and optionally run DoH special fallback in parallel.
        let mut special_task = self.maybe_spawn_special_fallback();
        let mut enum_current = main_dc;
        let ids = self.client.dc_options().config_enum_dc_ids();
        let total_steps = ids
            .len()
            .saturating_mul(self.options.max_enum_rounds)
            .max(1);

        let mut last_err: Option<InvocationError> = None;
        for _ in 0..total_steps {
            let current_ids = self.client.dc_options().config_enum_dc_ids();
            if current_ids.is_empty() {
                break;
            }
            enum_current = next_enum_dc(&current_ids, enum_current);
            let shifted = config_dc_id(enum_current);

            let enum_fut = self.send_get_config(shifted);
            let step_timeout = tokio::time::sleep(self.options.enumerate_dc_timeout);
            tokio::pin!(enum_fut);
            tokio::pin!(step_timeout);

            let winner = tokio::select! {
                res = &mut main_fut => {
                    match res {
                        Ok(cfg) => Some(cfg),
                        Err(e) => {
                            last_err = Some(e);
                            None
                        }
                    }
                }
                res = &mut enum_fut => {
                    match res {
                        Ok(cfg) => Some(cfg),
                        Err(e) => {
                            last_err = Some(e);
                            None
                        }
                    }
                }
                () = &mut step_timeout => {
                    debug!(dc = enum_current, "config enum DC timed out");
                    None
                }
            };

            self.client.kill_session(shifted);
            if let Some(cfg) = winner {
                if let Some(handle) = special_task.take() {
                    handle.abort();
                }
                self.client.apply_tl_config(&cfg);
                return Ok(cfg);
            }
        }

        if let Some(handle) = special_task.take() {
            handle.abort();
        }
        Err(last_err.unwrap_or(InvocationError::Timeout))
    }

    async fn send_get_config(&self, shifted_dc_id: DcId) -> Result<TlConfig, InvocationError> {
        let TlConfigEnum::Config(cfg) = self
            .client
            .invoke_with_options(
                &GetConfig,
                InvokeOptions {
                    dc_id: shifted_dc_id,
                    handle_all_errors: true,
                    ..InvokeOptions::default()
                },
            )
            .await?;
        Ok(cfg)
    }

    fn maybe_spawn_special_fallback(&self) -> Option<JoinHandle<()>> {
        if !self.options.enable_doh_fallback {
            return None;
        }
        if self.client.proxy().proxy_type != ProxyType::None {
            return None;
        }
        let client = self.client.clone();
        let req_timeout = self.options.special_request_timeout;
        Some(tokio::spawn(async move {
            let domain = client.config_fields().txt_domain_string;
            let phone = client.phone_prefix_hint();
            let mut endpoints = match fetch_special_config(&domain, &phone, client.clock()).await {
                Ok(eps) => eps,
                Err(err) => {
                    warn!(domain, error = %err, "DoH special config fallback failed");
                    return;
                }
            };
            let mut tried: Vec<SpecialEndpoint> = Vec::new();
            while !endpoints.is_empty() {
                let idx = rand::thread_rng().gen_range(0..endpoints.len());
                let ep = endpoints.remove(idx);
                if tried.contains(&ep) {
                    continue;
                }
                let temp_dc = temporary_dc_id_from_real(ep.dc_id);
                if temp_dc == 0 {
                    tried.push(ep);
                    continue;
                }
                let mut flags = DcOptionFlags::TCPO_ONLY;
                if !ep.secret.is_empty() {
                    flags |= DcOptionFlags::SECRET;
                }
                client.dc_options().add_endpoint(DcEndpoint {
                    id: temp_dc,
                    flags: DcOptionFlags(flags),
                    ip: ep.ip.clone(),
                    port: ep.port,
                    secret: ep.secret.clone(),
                });

                let probe = tokio::time::timeout(
                    req_timeout,
                    client.invoke_with_options(
                        &GetConfig,
                        InvokeOptions {
                            dc_id: temp_dc,
                            handle_all_errors: true,
                            ..InvokeOptions::default()
                        },
                    ),
                )
                .await;
                client.kill_session(temp_dc);
                tried.push(ep);

                if let Ok(Ok(TlConfigEnum::Config(cfg))) = probe
                    && !cfg.dc_options.is_empty()
                {
                    // Port of `ConfigLoader::specialConfigLoaded`: only apply dc_options;
                    // full config fields are awaited from a normal DC.
                    client.dc_options().set_from_list(&cfg.dc_options);
                    return;
                }
            }
        }))
    }
}

/// Computes the next DC ID in `ids` after `current` (wrapping around to `ids[0]`),
/// matching `ConfigLoader::enumerate` (`config_loader.cpp:105-110`).
pub fn next_enum_dc(ids: &[DcId], current: DcId) -> DcId {
    match ids.iter().position(|&id| id == current) {
        Some(pos) if pos + 1 < ids.len() => ids[pos + 1],
        _ => ids[0],
    }
}

/// Computes how long to wait before refreshing a `TlConfig` given `expires` and `now_secs`
/// (`Instance::Private::configLoadDone` + `requestConfigIfExpired`).
pub fn compute_config_refresh_delay(expires: i32, now_secs: i32) -> Duration {
    let diff = i64::from(expires) - i64::from(now_secs);
    if diff <= 0 {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(diff as u64).min(MAX_EXPIRE_TIMER_STEP)
    }
}

/// Handle controlling the background periodic `help.getConfig` refresh task.
///
/// Dropping this handle cancels the background scheduler cleanly.
pub struct ConfigSchedulerHandle {
    trigger: std::sync::Arc<Notify>,
    task: JoinHandle<()>,
}

impl ConfigSchedulerHandle {
    /// Spawns a periodic `help.getConfig` scheduler for `client`.
    pub fn spawn(client: Client, options: ConfigLoaderOptions) -> Self {
        let trigger = std::sync::Arc::new(Notify::new());
        let trigger_clone = trigger.clone();
        let task = tokio::spawn(async move {
            let loader = ConfigLoader::new(client.clone(), options);
            let mut next_refresh = Duration::ZERO;
            loop {
                if !next_refresh.is_zero() {
                    tokio::select! {
                        () = tokio::time::sleep(next_refresh) => {}
                        () = trigger_clone.notified() => {}
                    }
                }
                match loader.load().await {
                    Ok(cfg) => {
                        let now = client.clock().now_secs();
                        next_refresh = compute_config_refresh_delay(cfg.expires, now);
                        debug!(
                            expires = cfg.expires,
                            refresh_secs = next_refresh.as_secs(),
                            "scheduled next help.getConfig refresh"
                        );
                    }
                    Err(err) => {
                        warn!(error = %err, "periodic help.getConfig failed, retrying");
                        next_refresh = Duration::from_secs(30);
                    }
                }
            }
        });
        Self { trigger, task }
    }

    /// Requests an immediate config reload if the last loaded config is older than
    /// [`CONFIG_BECOMES_OLD`] (or [`CONFIG_BECOMES_OLD_FOR_BLOCKED`] when `blocked_mode` is set).
    pub fn request_if_old(&self, last_loaded: Option<Instant>, blocked_mode: bool) -> bool {
        let threshold = if blocked_mode {
            CONFIG_BECOMES_OLD_FOR_BLOCKED
        } else {
            CONFIG_BECOMES_OLD
        };
        let is_old = match last_loaded {
            None => true,
            Some(ts) => ts.elapsed() >= threshold,
        };
        if is_old {
            self.trigger.notify_one();
        }
        is_old
    }

    /// Triggers an immediate config reload regardless of age.
    pub fn trigger_now(&self) {
        self.trigger.notify_one();
    }

    /// Stops the background scheduler task.
    pub fn stop(self) {
        self.task.abort();
    }
}

impl Drop for ConfigSchedulerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}
