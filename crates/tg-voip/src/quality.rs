//! Adaptive bitrate control, network type handling, signal-bar estimation,
//! traffic statistics, and `phone.saveCallDebug` JSON log generation.
//!
//! Port of `TelegramMessenger/tgcalls`:
//! - `tgcalls/Instance.h` (`NetworkType`, `DataSaving`, `TrafficStats`, `FinalState`)
//! - `tgcalls/Stats.h` (`CallStats`, `CallStatsNetworkRecord`, `CallStatsBitrateRecord`)
//! - `tgcalls/v2/InstanceV2Impl.cpp` (`writeStateLogRecords` signal bars & `statsLog` JSON)

use serde::{Deserialize, Serialize};
use tg_calls::SIGNAL_BAR_COUNT;

use crate::jitter::JitterBufferStats;

/// Minimum Opus voice bitrate under heavy congestion/data-saving (12 kbps).
pub const MIN_AUDIO_BITRATE_BPS: i32 = 12_000;

/// Maximum Opus voice bitrate for 1:1 calls (`32 * 1024` bps in `InstanceV2Impl.cpp`).
pub const MAX_AUDIO_BITRATE_BPS: i32 = 32_000;

/// Minimum video bitrate under heavy congestion (150 kbps).
pub const MIN_VIDEO_BITRATE_BPS: u32 = 150_000;

/// Maximum video bitrate for 1:1 720p30 calls (1.5 Mbps).
pub const MAX_VIDEO_BITRATE_BPS: u32 = 1_500_000;

/// Adaptive video encoding resolution, frame rate, and bitrate tier driven by [`QualityController`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoQualityProfile {
    /// Target encoded width in pixels.
    pub width: u32,
    /// Target encoded height in pixels.
    pub height: u32,
    /// Target frames per second.
    pub fps: u32,
    /// Target video bitrate in bits per second.
    pub bitrate_bps: u32,
}

impl VideoQualityProfile {
    /// Select a resolution/FPS tier appropriate for `bitrate_bps`.
    #[must_use]
    pub const fn for_bitrate(bitrate_bps: u32) -> Self {
        if bitrate_bps >= 900_000 {
            Self {
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_bps,
            }
        } else if bitrate_bps >= 500_000 {
            Self {
                width: 854,
                height: 480,
                fps: 30,
                bitrate_bps,
            }
        } else if bitrate_bps >= 250_000 {
            Self {
                width: 640,
                height: 360,
                fps: 24,
                bitrate_bps,
            }
        } else {
            Self {
                width: 426,
                height: 240,
                fps: 15,
                bitrate_bps,
            }
        }
    }
}

/// Bitrate normalization ceiling for 4-bar audio quality (`16.0 kbps` in `InstanceV2Impl.cpp`).
const AUDIO_SIGNAL_BARS_BITRATE_NORM_KBPS: f32 = 16.0;

/// Network link type reported by the host OS (`tgcalls::NetworkType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NetworkType {
    /// Unknown link type.
    #[default]
    Unknown,
    /// 2G GPRS.
    Gprs,
    /// 2.5G EDGE.
    Edge,
    /// 3G UMTS.
    ThirdGeneration,
    /// 3.5G HSPA.
    Hspa,
    /// 4G LTE.
    Lte,
    /// Wi-Fi (low-cost).
    WiFi,
    /// Wired Ethernet (low-cost).
    Ethernet,
    /// Other high-speed link.
    OtherHighSpeed,
    /// Other low-speed link.
    OtherLowSpeed,
    /// Other metered cellular link.
    OtherMobile,
    /// Dial-up link.
    Dialup,
}

impl NetworkType {
    /// Returns `true` if this link is metered cellular/mobile.
    #[must_use]
    pub const fn is_mobile(self) -> bool {
        matches!(
            self,
            NetworkType::Gprs
                | NetworkType::Edge
                | NetworkType::ThirdGeneration
                | NetworkType::Hspa
                | NetworkType::Lte
                | NetworkType::OtherMobile
        )
    }

    /// Returns `true` if this link is unmetered/low-cost (Wi-Fi / Ethernet).
    #[must_use]
    pub const fn is_low_cost(self) -> bool {
        matches!(
            self,
            NetworkType::WiFi | NetworkType::Ethernet | NetworkType::OtherHighSpeed
        )
    }

    /// Recommended bitrate ceiling for this network type.
    #[must_use]
    pub const fn max_bitrate_bps(self) -> i32 {
        match self {
            NetworkType::Gprs | NetworkType::Dialup => 12_000,
            NetworkType::Edge | NetworkType::OtherLowSpeed => 16_000,
            NetworkType::ThirdGeneration | NetworkType::Hspa | NetworkType::OtherMobile => 24_000,
            NetworkType::Lte
            | NetworkType::WiFi
            | NetworkType::Ethernet
            | NetworkType::OtherHighSpeed
            | NetworkType::Unknown => MAX_AUDIO_BITRATE_BPS,
        }
    }
}

/// Call data-saving preference (`tgcalls::DataSaving`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DataSaving {
    /// Never throttle bitrate for data saving.
    #[default]
    Never,
    /// Throttle bitrate when on mobile/cellular networks.
    Mobile,
    /// Always use low-bitrate voice profile.
    Always,
}

/// Active transport route kind (`tgcalls::CallStatsConnectionEndpointType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConnectionEndpointKind {
    /// Direct peer-to-peer UDP route.
    P2p = 0,
    /// Relayed through Telegram UDP reflector or TURN server.
    #[default]
    TurnOrReflector = 1,
}

/// Byte counters split by Wi-Fi vs mobile (`tgcalls::TrafficStats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficStats {
    /// Bytes sent over unmetered Wi-Fi / Ethernet.
    pub bytes_sent_wifi: u64,
    /// Bytes received over unmetered Wi-Fi / Ethernet.
    pub bytes_received_wifi: u64,
    /// Bytes sent over metered mobile networks.
    pub bytes_sent_mobile: u64,
    /// Bytes received over metered mobile networks.
    pub bytes_received_mobile: u64,
}

impl TrafficStats {
    /// Record outgoing bytes under the active [`NetworkType`].
    pub fn record_sent(&mut self, bytes: u64, network: NetworkType) {
        if network.is_mobile() {
            self.bytes_sent_mobile += bytes;
        } else {
            self.bytes_sent_wifi += bytes;
        }
    }

    /// Record incoming bytes under the active [`NetworkType`].
    pub fn record_received(&mut self, bytes: u64, network: NetworkType) {
        if network.is_mobile() {
            self.bytes_received_mobile += bytes;
        } else {
            self.bytes_received_wifi += bytes;
        }
    }
}

/// One network route transition sample (`tgcalls::CallStatsNetworkRecord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkLogRecord {
    /// Offset timestamp in milliseconds from call start.
    pub timestamp_ms: u64,
    /// Whether the route was P2P or relayed.
    pub endpoint_kind: ConnectionEndpointKind,
    /// Whether the network was low-cost (Wi-Fi/Ethernet).
    pub is_low_cost: bool,
}

/// One periodic bitrate sample (`tgcalls::CallStatsBitrateRecord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitrateLogRecord {
    /// Offset timestamp in milliseconds from call start.
    pub timestamp_ms: u64,
    /// Bitrate in kbps (`b` in `InstanceV2Impl.cpp`).
    pub bitrate_kbps: i32,
}

/// Adaptive Opus & video bitrate controller and signal-bars quality estimator.
#[derive(Debug, Clone)]
pub struct QualityController {
    network_type: NetworkType,
    data_saving: DataSaving,
    current_bitrate_bps: i32,
    current_video_bitrate_bps: u32,
    expected_loss_perc: i32,
    smoothed_rtt_ms: f64,
    smoothed_loss_ratio: f64,
    last_signal_bars: i32,
    network_records: Vec<NetworkLogRecord>,
    bitrate_records: Vec<BitrateLogRecord>,
}

impl Default for QualityController {
    fn default() -> Self {
        Self::new(NetworkType::WiFi, DataSaving::Never)
    }
}

impl QualityController {
    /// Create a new controller for the initial network type and data-saving mode.
    #[must_use]
    pub fn new(network_type: NetworkType, data_saving: DataSaving) -> Self {
        let initial_bps = Self::cap_for(network_type, data_saving);
        let initial_video_bps = Self::video_cap_for(network_type, data_saving);
        Self {
            network_type,
            data_saving,
            current_bitrate_bps: initial_bps,
            current_video_bitrate_bps: initial_video_bps,
            expected_loss_perc: 5,
            smoothed_rtt_ms: 0.0,
            smoothed_loss_ratio: 0.0,
            last_signal_bars: SIGNAL_BAR_COUNT as i32,
            network_records: Vec::new(),
            bitrate_records: Vec::new(),
        }
    }

    /// Update the active [`NetworkType`] and clamp audio & video bitrates to their caps.
    pub fn set_network_type(
        &mut self,
        network_type: NetworkType,
        elapsed_ms: u64,
        endpoint: ConnectionEndpointKind,
    ) {
        self.network_type = network_type;
        let cap = Self::cap_for(self.network_type, self.data_saving);
        if self.current_bitrate_bps > cap {
            self.current_bitrate_bps = cap;
        }
        let vcap = Self::video_cap_for(self.network_type, self.data_saving);
        if self.current_video_bitrate_bps > vcap {
            self.current_video_bitrate_bps = vcap;
        }
        self.network_records.push(NetworkLogRecord {
            timestamp_ms: elapsed_ms,
            endpoint_kind: endpoint,
            is_low_cost: network_type.is_low_cost(),
        });
    }

    /// Update [`DataSaving`] preference.
    pub fn set_data_saving(&mut self, mode: DataSaving) {
        self.data_saving = mode;
        let cap = Self::cap_for(self.network_type, self.data_saving);
        self.current_bitrate_bps = self.current_bitrate_bps.min(cap);
        let vcap = Self::video_cap_for(self.network_type, self.data_saving);
        self.current_video_bitrate_bps = self.current_video_bitrate_bps.min(vcap);
    }

    /// Current active network type.
    #[must_use]
    pub const fn network_type(&self) -> NetworkType {
        self.network_type
    }

    /// Current recommended Opus target bitrate in bps.
    #[must_use]
    pub const fn bitrate_bps(&self) -> i32 {
        self.current_bitrate_bps
    }

    /// Current recommended H.264 video target bitrate in bps.
    #[must_use]
    pub const fn video_bitrate_bps(&self) -> u32 {
        self.current_video_bitrate_bps
    }

    /// Current recommended video encoding resolution, FPS, and bitrate tier.
    #[must_use]
    pub const fn video_profile(&self) -> VideoQualityProfile {
        VideoQualityProfile::for_bitrate(self.current_video_bitrate_bps)
    }

    /// Current expected packet loss percentage (`0..=100`) to feed into `OpusEncoder::packet_loss_perc`.
    #[must_use]
    pub const fn expected_loss_perc(&self) -> i32 {
        self.expected_loss_perc
    }

    /// Smoothed round-trip time estimate in milliseconds.
    #[must_use]
    pub const fn rtt_ms(&self) -> f64 {
        self.smoothed_rtt_ms
    }

    /// Smoothed packet loss ratio (`0.0..=1.0`).
    #[must_use]
    pub const fn loss_ratio(&self) -> f64 {
        self.smoothed_loss_ratio
    }

    /// Most recently computed signal bar count (`0..=4`).
    #[must_use]
    pub const fn signal_bars(&self) -> i32 {
        self.last_signal_bars
    }

    /// Incorporate a new RTT measurement (in milliseconds) using EWMA smoothing.
    pub fn observe_rtt(&mut self, rtt_ms: f64) {
        let clamped = rtt_ms.clamp(0.0, 10_000.0);
        if self.smoothed_rtt_ms == 0.0 {
            self.smoothed_rtt_ms = clamped;
        } else {
            self.smoothed_rtt_ms = self.smoothed_rtt_ms * 0.75 + clamped * 0.25;
        }
    }

    /// Periodic (~1 s) adaptation step driven by observed loss (`fraction_lost`: `0..=255`)
    /// and current RTT. Returns `(new_bitrate_bps, expected_loss_perc, signal_bars)`.
    pub fn on_quality_tick(&mut self, fraction_lost: u8, elapsed_ms: u64) -> (i32, i32, i32) {
        let instant_loss = f64::from(fraction_lost) / 256.0;
        self.smoothed_loss_ratio = self.smoothed_loss_ratio * 0.6 + instant_loss * 0.4;

        let cap = Self::cap_for(self.network_type, self.data_saving);
        let vcap = Self::video_cap_for(self.network_type, self.data_saving);
        if self.smoothed_loss_ratio > 0.10 || self.smoothed_rtt_ms > 450.0 {
            // Multiplicative decrease under congestion/loss.
            let decreased = ((f64::from(self.current_bitrate_bps)) * 0.80) as i32;
            self.current_bitrate_bps = decreased.max(MIN_AUDIO_BITRATE_BPS);
            let vdec = ((f64::from(self.current_video_bitrate_bps)) * 0.70) as u32;
            self.current_video_bitrate_bps = vdec.max(MIN_VIDEO_BITRATE_BPS);
        } else if self.smoothed_loss_ratio > 0.03 || self.smoothed_rtt_ms > 250.0 {
            let decreased = self.current_bitrate_bps - 2_000;
            self.current_bitrate_bps = decreased.max(MIN_AUDIO_BITRATE_BPS);
            let vdec = ((f64::from(self.current_video_bitrate_bps)) * 0.85) as u32;
            self.current_video_bitrate_bps = vdec.max(MIN_VIDEO_BITRATE_BPS);
        } else {
            // Additive increase when link is clean.
            self.current_bitrate_bps = (self.current_bitrate_bps + 2_000).min(cap);
            self.current_video_bitrate_bps = self
                .current_video_bitrate_bps
                .saturating_add(100_000)
                .min(vcap);
        }

        // Set Opus FEC loss hint slightly above observed loss for protection.
        let loss_perc = ((self.smoothed_loss_ratio * 100.0).ceil() as i32).clamp(0, 50);
        self.expected_loss_perc = if loss_perc > 0 { loss_perc.max(10) } else { 5 };

        // Compute 0..=4 signal bars:
        // Base formula from `InstanceV2Impl.cpp` (`sendBitrateKbps / 16.0 * 4.0`) penalized by
        // observed packet loss and high RTT so severe degradation shows fewer bars immediately.
        let bitrate_kbps = self.current_bitrate_bps as f32 / 1000.0;
        let mut quality = (bitrate_kbps / AUDIO_SIGNAL_BARS_BITRATE_NORM_KBPS).clamp(0.0, 1.0);
        let loss_penalty = (self.smoothed_loss_ratio as f32 * 2.5).min(0.75);
        let rtt_penalty = if self.smoothed_rtt_ms > 150.0 {
            (((self.smoothed_rtt_ms - 150.0) / 800.0) as f32).min(0.5)
        } else {
            0.0
        };
        quality = (quality - loss_penalty - rtt_penalty).clamp(0.0, 1.0);
        let bars = (quality * SIGNAL_BAR_COUNT as f32).round() as i32;
        self.last_signal_bars = bars.clamp(0, SIGNAL_BAR_COUNT as i32);

        self.bitrate_records.push(BitrateLogRecord {
            timestamp_ms: elapsed_ms,
            bitrate_kbps: self.current_bitrate_bps / 1000,
        });

        (
            self.current_bitrate_bps,
            self.expected_loss_perc,
            self.last_signal_bars,
        )
    }

    /// Recorded network state transitions.
    #[must_use]
    pub fn network_records(&self) -> &[NetworkLogRecord] {
        &self.network_records
    }

    /// Recorded bitrate samples.
    #[must_use]
    pub fn bitrate_records(&self) -> &[BitrateLogRecord] {
        &self.bitrate_records
    }

    const fn cap_for(network: NetworkType, saving: DataSaving) -> i32 {
        let net_cap = network.max_bitrate_bps();
        let save_cap = match saving {
            DataSaving::Never => MAX_AUDIO_BITRATE_BPS,
            DataSaving::Mobile => {
                if network.is_mobile() {
                    16_000
                } else {
                    MAX_AUDIO_BITRATE_BPS
                }
            }
            DataSaving::Always => 16_000,
        };
        if net_cap < save_cap {
            net_cap
        } else {
            save_cap
        }
    }

    const fn video_cap_for(network: NetworkType, saving: DataSaving) -> u32 {
        let net_cap = match network {
            NetworkType::Gprs | NetworkType::Dialup => MIN_VIDEO_BITRATE_BPS,
            NetworkType::Edge | NetworkType::OtherLowSpeed => 250_000,
            NetworkType::ThirdGeneration | NetworkType::Hspa | NetworkType::OtherMobile => 600_000,
            NetworkType::Lte => 1_000_000,
            NetworkType::WiFi
            | NetworkType::Ethernet
            | NetworkType::OtherHighSpeed
            | NetworkType::Unknown => MAX_VIDEO_BITRATE_BPS,
        };
        let save_cap = match saving {
            DataSaving::Never => MAX_VIDEO_BITRATE_BPS,
            DataSaving::Mobile => {
                if network.is_mobile() {
                    450_000
                } else {
                    MAX_VIDEO_BITRATE_BPS
                }
            }
            DataSaving::Always => 350_000,
        };
        if net_cap < save_cap {
            net_cap
        } else {
            save_cap
        }
    }
}

/// Complete live or final media engine telemetry snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoipStatsSnapshot {
    /// Negotiated protocol version string (e.g. `"12.0.0"`).
    pub version: String,
    /// Outgoing audio codec name (`"opus"`).
    pub outgoing_codec: String,
    /// Current target Opus send bitrate in bits per second.
    pub audio_bitrate_bps: i32,
    /// Current signal quality bar count (`0..=4`).
    pub signal_bars: i32,
    /// Smoothed round-trip time in milliseconds.
    pub rtt_ms: f64,
    /// Smoothed packet loss ratio (`0.0..=1.0`).
    pub loss_ratio: f64,
    /// Total encrypted media packets sent.
    pub packets_sent: u64,
    /// Total encrypted media packets received.
    pub packets_received: u64,
    /// Total incoming packets rejected by auth/replay checks.
    pub packets_decrypt_failed: u64,
    /// Active endpoint route kind (P2P vs reflector/TURN).
    pub endpoint_kind: ConnectionEndpointKind,
    /// Preferred Telegram relay `endpoint_id` (0 if P2P or none).
    pub preferred_relay_id: i64,
    /// Traffic counters split by Wi-Fi / mobile.
    pub traffic: TrafficStats,
    /// Jitter buffer statistics.
    pub jitter_packets_received: u64,
    /// Jitter buffer lost frames count.
    pub jitter_packets_lost: u64,
    /// Jitter buffer out-of-order packets count.
    pub jitter_packets_reordered: u64,
    /// Jitter buffer Opus in-band FEC recoveries.
    pub jitter_fec_recovered: u64,
    /// Jitter buffer Opus PLC synthesized frames.
    pub jitter_plc_frames: u64,
    /// Jitter buffer smoothed jitter in ms.
    pub jitter_ms: f64,
    /// Jitter buffer target delay in ms.
    pub jitter_target_delay_ms: u32,
}

impl VoipStatsSnapshot {
    /// Populate jitter fields from [`JitterBufferStats`].
    pub fn apply_jitter(&mut self, jb: &JitterBufferStats) {
        self.jitter_packets_received = jb.packets_received;
        self.jitter_packets_lost = jb.packets_lost;
        self.jitter_packets_reordered = jb.packets_reordered;
        self.jitter_fec_recovered = jb.fec_recovered;
        self.jitter_plc_frames = jb.plc_frames;
        self.jitter_ms = jb.jitter_ms;
        self.jitter_target_delay_ms = jb.target_delay_ms;
    }

    /// Build a JSON debug log suitable for `phone.saveCallDebug` (`CallAction::SaveCallDebug`),
    /// matching `InstanceV2Impl::stop` (`network` + `bitrate` arrays plus VoIP metrics).
    #[must_use]
    pub fn to_debug_log_json(&self, quality: &QualityController) -> String {
        let network: Vec<serde_json::Value> = quality
            .network_records()
            .iter()
            .map(|r| {
                serde_json::json!({
                    "t": r.timestamp_ms,
                    "c": r.endpoint_kind as i32,
                    "l": r.is_low_cost,
                })
            })
            .collect();
        let bitrate: Vec<serde_json::Value> = quality
            .bitrate_records()
            .iter()
            .map(|r| serde_json::json!({ "b": r.bitrate_kbps }))
            .collect();

        serde_json::json!({
            "lib": "tg-voip",
            "version": self.version,
            "codec": self.outgoing_codec,
            "network": network,
            "bitrate": bitrate,
            "summary": {
                "rtt_ms": self.rtt_ms,
                "loss_ratio": self.loss_ratio,
                "signal_bars": self.signal_bars,
                "packets_sent": self.packets_sent,
                "packets_received": self.packets_received,
                "packets_lost": self.jitter_packets_lost,
                "packets_reordered": self.jitter_packets_reordered,
                "fec_recovered": self.jitter_fec_recovered,
                "plc_frames": self.jitter_plc_frames,
                "preferred_relay_id": self.preferred_relay_id,
                "bytes_sent_wifi": self.traffic.bytes_sent_wifi,
                "bytes_received_wifi": self.traffic.bytes_received_wifi,
                "bytes_sent_mobile": self.traffic.bytes_sent_mobile,
                "bytes_received_mobile": self.traffic.bytes_received_mobile,
            }
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_controller_adapts_bitrate_and_signal_bars() {
        let mut q = QualityController::new(NetworkType::WiFi, DataSaving::Never);
        assert_eq!(q.bitrate_bps(), 32_000);

        // Clean link keeps max bitrate and 4 signal bars.
        q.observe_rtt(30.0);
        let (bps, _, bars) = q.on_quality_tick(0, 1_000);
        assert_eq!(bps, 32_000);
        assert_eq!(bars, 4);

        // Heavy packet loss + high RTT steps down bitrate and lowers signal bars.
        q.observe_rtt(600.0);
        q.observe_rtt(600.0);
        let (bps_bad, loss_hint, bars_bad) = q.on_quality_tick(80, 2_000); // ~31% loss
        assert!(bps_bad < 32_000);
        assert!(loss_hint >= 10);
        assert!(bars_bad < 4);

        // DataSaving::Always caps bitrate to 16 kbps.
        q.set_data_saving(DataSaving::Always);
        assert!(q.bitrate_bps() <= 16_000);
    }
}
