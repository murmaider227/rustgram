//! Port of `mtproto/mtproto_dc_options.cpp`, `mtproto/facade.h`, and `mtproto/core_types.h`.
//!
//! Contains built-in production and test DC endpoints (IPv4 & IPv6), Telegram's built-in RSA
//! public keys (sourced from `tg-crypto`), DC ID shifting helpers, CDN key management,
//! endpoint lookup/filtering, and tdesktop `QDataStream` serialization (`serialize` /
//! `construct_from_serialized`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::RwLock;
use tg_crypto::rsa::{PRODUCTION_PUBLIC_KEYS, PublicKey, TEST_PUBLIC_KEYS};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::qdatastream::{QDataReader, QDataWriter};

/// Bare data center identifier (e.g. 1..=5).
pub type DcId = i32;

/// Shifted data center identifier encoding both bare DC ID and session slot (`dc_id + DC_SHIFT * shift`).
pub type ShiftedDcId = i32;

/// Base shift multiplier separating DC ID from session shift (`kDcShift`).
pub const DC_SHIFT: ShiftedDcId = 10_000;
/// Shift used for `help.getConfig` enumeration (`kConfigDcShift`).
pub const CONFIG_DC_SHIFT: i32 = 0x01;
/// Shift used for logging out guest DCs (`kLogoutDcShift`).
pub const LOGOUT_DC_SHIFT: i32 = 0x02;
/// Shift used by dedicated file loader / updater (`kUpdaterDcShift`).
pub const UPDATER_DC_SHIFT: i32 = 0x03;
/// Shift used for data export (`kExportDcShift`).
pub const EXPORT_DC_SHIFT: i32 = 0x04;
/// Shift used for export media downloads (`kExportMediaDcShift`).
pub const EXPORT_MEDIA_DC_SHIFT: i32 = 0x05;
/// Shift used for group call streams (`kGroupCallStreamDcShift`).
pub const GROUP_CALL_STREAM_DC_SHIFT: i32 = 0x06;
/// Shift used for stats (`kStatsDcShift`).
pub const STATS_DC_SHIFT: i32 = 0x07;
/// Maximum number of parallel media download/upload sessions per DC (`kMaxMediaDcCount`).
pub const MAX_MEDIA_DC_COUNT: i32 = 0x10;
/// Base shift for download sessions (`kBaseDownloadDcShift`).
pub const BASE_DOWNLOAD_DC_SHIFT: i32 = 0x10;
/// Base shift for upload sessions (`kBaseUploadDcShift`).
pub const BASE_UPLOAD_DC_SHIFT: i32 = 0x20;
/// Base shift for key-destruction sessions (`kDestroyKeyStartDcShift`).
pub const DESTROY_KEY_START_DC_SHIFT: i32 = 0x100;
/// Protocol DC ID offset used on test DCs (`kTestModeDcIdShift`).
pub const TEST_MODE_DC_ID_SHIFT: i32 = 10_000;
/// Default initial main DC ID (`Instance::Fields::kDefaultMainDc`).
pub const DEFAULT_MAIN_DC: DcId = 2;
/// Offset for temporary main DCs (`Instance::Fields::kTemporaryMainDc`).
pub const TEMPORARY_MAIN_DC: DcId = 1_000;

/// Current `DcOptions::serialize` format version (`kVersion = 2` in `mtproto_dc_options.cpp`).
const DC_OPTIONS_SERIALIZE_VERSION: i32 = 2;
/// Maximum IP string length accepted by `constructFromSerialized` (`kMaxIpSize = 45`).
const MAX_IP_SIZE: usize = 45;
/// Maximum endpoint secret length accepted by `constructFromSerialized` (`kMaxSecretSize = 32`).
const MAX_SECRET_SIZE: usize = 32;

/// Built-in production Telegram server RSA public keys (re-exported from `tg-crypto`).
pub const BUILT_IN_RSA_KEYS_PRODUCTION: &[&str] = PRODUCTION_PUBLIC_KEYS;
/// Built-in test Telegram server RSA public keys (re-exported from `tg-crypto`).
pub const BUILT_IN_RSA_KEYS_TEST: &[&str] = TEST_PUBLIC_KEYS;

/// Extracts the bare DC ID (`BareDcId`).
pub const fn bare_dc_id(shifted_dc_id: ShiftedDcId) -> DcId {
    shifted_dc_id % DC_SHIFT
}

/// Combines a bare DC ID and shift slot (`ShiftDcId`).
pub const fn shift_dc_id(dc_id: DcId, shift: i32) -> ShiftedDcId {
    dc_id + DC_SHIFT * shift
}

/// Extracts the shift slot (`GetDcIdShift`).
pub const fn get_dc_id_shift(shifted_dc_id: ShiftedDcId) -> i32 {
    shifted_dc_id / DC_SHIFT
}

/// Shifted DC ID for config enumeration (`MTP::configDcId`).
pub const fn config_dc_id(dc_id: DcId) -> ShiftedDcId {
    shift_dc_id(dc_id, CONFIG_DC_SHIFT)
}

/// Shifted DC ID for guest logout (`MTP::logoutDcId`).
pub const fn logout_dc_id(dc_id: DcId) -> ShiftedDcId {
    shift_dc_id(dc_id, LOGOUT_DC_SHIFT)
}

/// Shifted DC ID for dedicated file loader (`MTP::updaterDcId`).
pub const fn updater_dc_id(dc_id: DcId) -> ShiftedDcId {
    shift_dc_id(dc_id, UPDATER_DC_SHIFT)
}

/// Shifted DC ID for group call stream (`MTP::groupCallStreamDcId`).
pub const fn group_call_stream_dc_id(dc_id: DcId) -> ShiftedDcId {
    shift_dc_id(dc_id, GROUP_CALL_STREAM_DC_SHIFT)
}

/// Shifted DC ID for media download slot `index` (`0..MAX_MEDIA_DC_COUNT`).
pub const fn download_dc_id(dc_id: DcId, index: i32) -> ShiftedDcId {
    shift_dc_id(dc_id, BASE_DOWNLOAD_DC_SHIFT + index)
}

/// Shifted DC ID for media upload slot `index` (`0..MAX_MEDIA_DC_COUNT`, bare DC 0 = main DC).
pub const fn upload_dc_id(index: i32) -> ShiftedDcId {
    shift_dc_id(0, BASE_UPLOAD_DC_SHIFT + index)
}

/// Computes the next unique shifted DC ID for key destruction (`MTP::destroyKeyNextDcId`).
pub const fn destroy_key_next_dc_id(shifted_dc_id: ShiftedDcId) -> ShiftedDcId {
    let shift = get_dc_id_shift(shifted_dc_id);
    let next_shift = if shift != 0 {
        shift + 1
    } else {
        DESTROY_KEY_START_DC_SHIFT
    };
    shift_dc_id(bare_dc_id(shifted_dc_id), next_shift)
}

/// Returns `true` if `shifted_dc_id` is a download session slot.
pub const fn is_download_dc_id(shifted_dc_id: ShiftedDcId) -> bool {
    shifted_dc_id >= download_dc_id(0, 0)
        && shifted_dc_id < download_dc_id(0, MAX_MEDIA_DC_COUNT - 1) + DC_SHIFT
}

/// Returns `true` if `shifted_dc_id` is an upload session slot.
pub const fn is_upload_dc_id(shifted_dc_id: ShiftedDcId) -> bool {
    shifted_dc_id >= upload_dc_id(0)
        && shifted_dc_id < upload_dc_id(MAX_MEDIA_DC_COUNT - 1) + DC_SHIFT
}

/// Returns `true` if `shifted_dc_id` targets a media cluster DC (`isMediaClusterDcId`).
pub const fn is_media_cluster_dc_id(shifted_dc_id: ShiftedDcId) -> bool {
    let shift = get_dc_id_shift(shifted_dc_id);
    is_download_dc_id(shifted_dc_id)
        || shift == GROUP_CALL_STREAM_DC_SHIFT
        || shift == EXPORT_MEDIA_DC_SHIFT
        || shift == UPDATER_DC_SHIFT
}

/// Returns `true` if `shifted_dc_id` refers to a temporary DC (`isTemporaryDcId`).
pub const fn is_temporary_dc_id(shifted_dc_id: ShiftedDcId) -> bool {
    bare_dc_id(shifted_dc_id) >= TEMPORARY_MAIN_DC
}

/// Maps a temporary DC ID back to its real bare DC ID (`getRealIdFromTemporaryDcId`).
pub const fn real_id_from_temporary_dc_id(shifted_dc_id: ShiftedDcId) -> DcId {
    let dc_id = bare_dc_id(shifted_dc_id);
    if dc_id >= TEMPORARY_MAIN_DC {
        dc_id - TEMPORARY_MAIN_DC
    } else {
        0
    }
}

/// Maps a real bare DC ID to its temporary DC ID (`getTemporaryIdFromRealDcId`).
pub const fn temporary_dc_id_from_real(shifted_dc_id: ShiftedDcId) -> DcId {
    let dc_id = bare_dc_id(shifted_dc_id);
    if dc_id < TEMPORARY_MAIN_DC {
        dc_id + TEMPORARY_MAIN_DC
    } else {
        0
    }
}

/// Computes the signed 16-bit protocol DC ID used in obfuscated2 init & `p_q_inner_data_dc`
/// (`SessionPrivate::getProtocolDcId`).
pub const fn protocol_dc_id(shifted_dc_id: ShiftedDcId, dc_type: DcType, test_mode: bool) -> i16 {
    let dc_id = bare_dc_id(shifted_dc_id);
    let simple = if is_temporary_dc_id(dc_id) {
        real_id_from_temporary_dc_id(dc_id)
    } else {
        dc_id
    };
    let is_media = matches!(dc_type, DcType::MediaCluster);
    tg_transport::protocol_dc_id(simple as i16, is_media, test_mode)
}

/// Environment selector (production or test servers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum Environment {
    /// Standard Telegram production datacenters (`0` in tdesktop `QDataStream`).
    #[default]
    Production = 0,
    /// Telegram test datacenters (`1` in tdesktop `QDataStream`).
    Test = 1,
}

impl Environment {
    /// Converts a serialized `i32` tag into an [`Environment`].
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Production),
            1 => Some(Self::Test),
            _ => None,
        }
    }

    /// Converts this [`Environment`] to its serialized `i32` tag.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Classification of a datacenter session (`MTP::DcType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DcType {
    /// Regular Telegram DC.
    #[default]
    Regular,
    /// Temporary DC (used during fallback config probing).
    Temporary,
    /// Media-only cluster DC.
    MediaCluster,
    /// CDN edge DC (does not bind persistent auth keys).
    Cdn,
}

/// Bitmask flags matching `dcOption#18b7a10d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DcOptionFlags(pub u32);

impl DcOptionFlags {
    /// Endpoint uses IPv6 (`flags.0`).
    pub const IPV6: u32 = 1 << 0;
    /// Endpoint is media-only (`flags.1`).
    pub const MEDIA_ONLY: u32 = 1 << 1;
    /// Endpoint supports obfuscated TCP only (`flags.2`).
    pub const TCPO_ONLY: u32 = 1 << 2;
    /// Endpoint is a CDN node (`flags.3`).
    pub const CDN: u32 = 1 << 3;
    /// Endpoint has a static IP suitable for proxies (`flags.4`).
    pub const STATIC: u32 = 1 << 4;
    /// Endpoint requires this exact port (`flags.5`).
    pub const THIS_PORT_ONLY: u32 = 1 << 5;
    /// Endpoint carries a transport secret (`flags.10`).
    pub const SECRET: u32 = 1 << 10;

    /// Returns true if all bits in `mask` are set.
    pub const fn contains(self, mask: u32) -> bool {
        (self.0 & mask) == mask
    }

    /// Returns true if any bit in `mask` is set.
    pub const fn intersects(self, mask: u32) -> bool {
        (self.0 & mask) != 0
    }

    /// Builds [`DcOptionFlags`] from a generated [`tg_tl::types::DcOption`].
    pub fn from_tl(opt: &tl_types::DcOption) -> Self {
        let mut bits = 0u32;
        if opt.ipv6 {
            bits |= Self::IPV6;
        }
        if opt.media_only {
            bits |= Self::MEDIA_ONLY;
        }
        if opt.tcpo_only {
            bits |= Self::TCPO_ONLY;
        }
        if opt.cdn {
            bits |= Self::CDN;
        }
        if opt.r#static {
            bits |= Self::STATIC;
        }
        if opt.this_port_only {
            bits |= Self::THIS_PORT_ONLY;
        }
        if opt.secret.is_some() {
            bits |= Self::SECRET;
        }
        Self(bits)
    }
}

/// One datacenter endpoint (`DcOptions::Endpoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcEndpoint {
    /// Bare DC ID.
    pub id: DcId,
    /// Option flags.
    pub flags: DcOptionFlags,
    /// IPv4 or IPv6 address literal.
    pub ip: String,
    /// TCP port (typically 443).
    pub port: u16,
    /// Optional MTProto transport secret.
    pub secret: Vec<u8>,
}

/// Categorized endpoint candidates returned by [`DcOptions::lookup`].
#[derive(Debug, Clone, Default)]
pub struct EndpointVariants {
    /// IPv4 TCP endpoints.
    pub ipv4_tcp: Vec<DcEndpoint>,
    /// IPv4 HTTP endpoints.
    pub ipv4_http: Vec<DcEndpoint>,
    /// IPv6 TCP endpoints.
    pub ipv6_tcp: Vec<DcEndpoint>,
    /// IPv6 HTTP endpoints.
    pub ipv6_http: Vec<DcEndpoint>,
}

// Built-in DC addresses copied verbatim from `mtproto_dc_options.cpp`.
const BUILT_IN_DCS: &[(DcId, &str, u16)] = &[
    (1, "149.154.175.50", 443),
    (2, "149.154.167.51", 443),
    (2, "95.161.76.100", 443),
    (3, "149.154.175.100", 443),
    (4, "149.154.167.91", 443),
    (5, "149.154.171.5", 443),
];

const BUILT_IN_DCS_IPV6: &[(DcId, &str, u16)] = &[
    (1, "2001:0b28:f23d:f001:0000:0000:0000:000a", 443),
    (2, "2001:067c:04e8:f002:0000:0000:0000:000a", 443),
    (3, "2001:0b28:f23d:f003:0000:0000:0000:000a", 443),
    (4, "2001:067c:04e8:f004:0000:0000:0000:000a", 443),
    (5, "2001:0b28:f23f:f005:0000:0000:0000:000a", 443),
];

const BUILT_IN_DCS_TEST: &[(DcId, &str, u16)] = &[
    (1, "149.154.175.10", 443),
    (2, "149.154.167.40", 443),
    (3, "149.154.175.117", 443),
];

const BUILT_IN_DCS_IPV6_TEST: &[(DcId, &str, u16)] = &[
    (1, "2001:0b28:f23d:f001:0000:0000:0000:000e", 443),
    (2, "2001:067c:04e8:f002:0000:0000:0000:000e", 443),
    (3, "2001:0b28:f23d:f003:0000:0000:0000:000e", 443),
];

#[derive(Debug, Clone)]
struct DcOptionsInner {
    environment: Environment,
    data: BTreeMap<DcId, Vec<DcEndpoint>>,
    cdn_dc_ids: BTreeSet<DcId>,
    public_keys: BTreeMap<u64, PublicKey>,
    cdn_public_keys: BTreeMap<DcId, BTreeMap<u64, PublicKey>>,
    immutable: bool,
}

/// Thread-safe registry of Telegram DC endpoints and RSA public keys (`MTP::DcOptions`).
#[derive(Debug, Clone)]
pub struct DcOptions {
    inner: Arc<RwLock<DcOptionsInner>>,
}

impl DcOptions {
    /// Creates `DcOptions` populated with built-in endpoints and RSA keys for `environment`.
    pub fn new(environment: Environment) -> Self {
        let mut inner = DcOptionsInner {
            environment,
            data: BTreeMap::new(),
            cdn_dc_ids: BTreeSet::new(),
            public_keys: BTreeMap::new(),
            cdn_public_keys: BTreeMap::new(),
            immutable: false,
        };
        inner.load_built_in();
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Creates a deep independent copy of this `DcOptions` (not sharing the inner `Arc`).
    pub fn deep_clone(&self) -> Self {
        let snapshot = self.inner.read().clone();
        Self {
            inner: Arc::new(RwLock::new(snapshot)),
        }
    }

    /// Returns the configured environment.
    pub fn environment(&self) -> Environment {
        self.inner.read().environment
    }

    /// Returns true if running in test mode.
    pub fn is_test_mode(&self) -> bool {
        self.environment() != Environment::Production
    }

    /// Sets whether remote config updates are blocked (`_immutable` in `mtproto_dc_options.cpp`).
    pub fn set_immutable(&self, immutable: bool) {
        self.inner.write().immutable = immutable;
    }

    /// Returns whether this `DcOptions` ignores remote updates.
    pub fn is_immutable(&self) -> bool {
        self.inner.read().immutable
    }

    /// Validates an endpoint protocol secret (`DcOptions::ValidateSecret`).
    pub fn validate_secret(secret: &[u8]) -> bool {
        tg_transport::validate_secret_bytes(secret)
    }

    /// Adds a single endpoint without notifying listeners (`constructAddOne`).
    pub fn add_endpoint(&self, endpoint: DcEndpoint) {
        let mut guard = self.inner.write();
        apply_one_option(&mut guard.data, endpoint);
        guard.recompute_cdn_dc_ids();
    }

    /// Registers an additional RSA public key (primarily useful in tests or custom servers).
    pub fn add_public_key(&self, key: PublicKey) {
        let mut guard = self.inner.write();
        guard.public_keys.insert(key.fingerprint(), key);
    }

    /// Replaces all endpoints from a `help.Config.dc_options` list (`setFromList`), returning the
    /// list of DC IDs whose endpoint set changed.
    pub fn set_from_list(&self, options: &[tl_enums::DcOption]) -> Vec<DcId> {
        self.process_from_list(options, true)
    }

    /// Merges endpoints from a list (`addFromList`), returning the changed DC IDs.
    pub fn add_from_list(&self, options: &[tl_enums::DcOption]) -> Vec<DcId> {
        self.process_from_list(options, false)
    }

    fn process_from_list(&self, options: &[tl_enums::DcOption], overwrite: bool) -> Vec<DcId> {
        if options.is_empty() {
            return Vec::new();
        }
        let mut guard = self.inner.write();
        if guard.immutable {
            return Vec::new();
        }
        let mut new_data = if overwrite {
            BTreeMap::new()
        } else {
            guard.data.clone()
        };
        for opt_enum in options {
            let tl_enums::DcOption::DcOption(opt) = opt_enum;
            if opt.port <= 0 || opt.port > u16::MAX as i32 {
                continue;
            }
            apply_one_option(
                &mut new_data,
                DcEndpoint {
                    id: opt.id,
                    flags: DcOptionFlags::from_tl(opt),
                    ip: opt.ip_address.clone(),
                    port: opt.port as u16,
                    secret: opt.secret.clone().unwrap_or_default(),
                },
            );
        }
        let diff = count_options_difference(&guard.data, &new_data);
        if !diff.is_empty() {
            guard.data = new_data;
            guard.recompute_cdn_dc_ids();
        }
        diff
    }

    /// Installs CDN RSA public keys (`setCDNConfig`).
    pub fn set_cdn_keys(&self, keys: &[(DcId, PublicKey)]) {
        let mut guard = self.inner.write();
        guard.cdn_public_keys.clear();
        for (dc_id, key) in keys {
            guard
                .cdn_public_keys
                .entry(*dc_id)
                .or_default()
                .insert(key.fingerprint(), key.clone());
        }
    }

    /// Returns all registered CDN `(dc_id, PublicKey)` pairs.
    pub fn cdn_keys(&self) -> Vec<(DcId, PublicKey)> {
        let guard = self.inner.read();
        let mut out = Vec::new();
        for (&dc_id, keys) in &guard.cdn_public_keys {
            for key in keys.values() {
                out.push((dc_id, key.clone()));
            }
        }
        out
    }

    /// Returns whether CDN RSA keys are loaded for `dc_id`.
    pub fn has_cdn_keys_for_dc(&self, dc_id: DcId) -> bool {
        self.inner.read().cdn_public_keys.contains_key(&dc_id)
    }

    /// Returns all currently stored endpoints for `dc_id`.
    pub fn endpoints_for_dc(&self, dc_id: DcId) -> Vec<DcEndpoint> {
        self.inner
            .read()
            .data
            .get(&dc_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Selects the first matching RSA public key for `dc_id` given server `fingerprints`
    /// (`DcOptions::getDcRSAKey`).
    pub fn get_dc_rsa_key(&self, dc_id: DcId, fingerprints: &[u64]) -> Option<PublicKey> {
        let guard = self.inner.read();
        // Match `SessionPrivate::connectToServer` (`mtproto/session_private.cpp:1031-1036`) and
        // `DcOptions::getDcRSAKey` (`mtproto/mtproto_dc_options.cpp:635-661`): known CDN DCs must
        // ONLY authenticate against RSA keys returned by `help.getCdnConfig`, never falling back
        // to regular Telegram DC keys.
        if guard.cdn_dc_ids.contains(&dc_id) || guard.cdn_public_keys.contains_key(&dc_id) {
            let cdn_map = guard.cdn_public_keys.get(&dc_id)?;
            for fp in fingerprints {
                if let Some(key) = cdn_map.get(fp) {
                    return Some(key.clone());
                }
            }
            return None;
        }
        for fp in fingerprints {
            if let Some(key) = guard.public_keys.get(fp) {
                return Some(key.clone());
            }
        }
        None
    }

    /// Classifies `shifted_dc_id` (`DcOptions::dcType`).
    pub fn dc_type(&self, shifted_dc_id: ShiftedDcId) -> DcType {
        if is_temporary_dc_id(shifted_dc_id) {
            return DcType::Temporary;
        }
        let guard = self.inner.read();
        let bare = bare_dc_id(shifted_dc_id);
        if guard.cdn_dc_ids.contains(&bare) {
            return DcType::Cdn;
        }
        if is_media_cluster_dc_id(shifted_dc_id) && guard.has_media_only_options_for(bare) {
            return DcType::MediaCluster;
        }
        DcType::Regular
    }

    /// Returns sorted list of non-CDN, non-temporary DC IDs (`configEnumDcIds`).
    pub fn config_enum_dc_ids(&self) -> Vec<DcId> {
        let guard = self.inner.read();
        let mut out = Vec::new();
        for (&dc_id, endpoints) in &guard.data {
            if let Some(first) = endpoints.first()
                && !first.flags.contains(DcOptionFlags::CDN)
                && !is_temporary_dc_id(dc_id)
            {
                out.push(dc_id);
            }
        }
        out
    }

    /// Looks up candidate endpoints for `(dc_id, dc_type, through_proxy)` (`DcOptions::lookup`).
    pub fn lookup(&self, dc_id: DcId, dc_type: DcType, through_proxy: bool) -> EndpointVariants {
        let guard = self.inner.read();
        let mut result = EndpointVariants::default();
        let Some(endpoints) = guard.data.get(&dc_id) else {
            return result;
        };
        for ep in endpoints {
            let flags = ep.flags;
            if dc_type == DcType::Cdn && !flags.contains(DcOptionFlags::CDN) {
                continue;
            }
            if dc_type != DcType::MediaCluster && flags.contains(DcOptionFlags::MEDIA_ONLY) {
                continue;
            }
            if !Self::validate_secret(&ep.secret) {
                continue;
            }
            if flags.contains(DcOptionFlags::IPV6) {
                result.ipv6_tcp.push(ep.clone());
                if !flags.intersects(DcOptionFlags::TCPO_ONLY | DcOptionFlags::SECRET) {
                    result.ipv6_http.push(ep.clone());
                }
            } else {
                result.ipv4_tcp.push(ep.clone());
                if !flags.intersects(DcOptionFlags::TCPO_ONLY | DcOptionFlags::SECRET) {
                    result.ipv4_http.push(ep.clone());
                }
            }
        }
        if dc_type == DcType::MediaCluster {
            filter_if_has_with_flag(&mut result, DcOptionFlags::MEDIA_ONLY);
        }
        if through_proxy {
            filter_if_has_with_flag(&mut result, DcOptionFlags::STATIC);
        }
        result
    }

    /// Serializes endpoints and CDN RSA public keys into tdesktop's `QDataStream::Qt_5_1`
    /// binary format (`DcOptions::serialize` in `mtproto_dc_options.cpp`).
    ///
    /// Temporary DCs are excluded. If `self.is_immutable()`, built-in defaults for the
    /// environment are serialized instead of overridden options.
    pub fn serialize(&self) -> Vec<u8> {
        let guard = self.inner.read();
        if guard.immutable {
            let env = guard.environment;
            drop(guard);
            return DcOptions::new(env).serialize();
        }

        let mut options_count: i32 = 0;
        for (&dc_id, endpoints) in &guard.data {
            if is_temporary_dc_id(dc_id) {
                continue;
            }
            options_count = options_count.saturating_add(endpoints.len() as i32);
        }

        let mut cdn_entries: Vec<(DcId, &[u8], &[u8])> = Vec::new();
        for (&dc_id, keys) in &guard.cdn_public_keys {
            for key in keys.values() {
                cdn_entries.push((dc_id, key.n(), key.e()));
            }
        }

        let mut writer = QDataWriter::with_capacity(256 + (options_count as usize) * 48);
        writer.write_i32(-DC_OPTIONS_SERIALIZE_VERSION);

        // DC options.
        writer.write_i32(options_count);
        for (&dc_id, endpoints) in &guard.data {
            if is_temporary_dc_id(dc_id) {
                continue;
            }
            for ep in endpoints {
                writer.write_i32(ep.id);
                writer.write_i32(ep.flags.0 as i32);
                writer.write_i32(i32::from(ep.port));
                writer.write_i32(ep.ip.len() as i32);
                writer.write_raw(ep.ip.as_bytes());
                writer.write_i32(ep.secret.len() as i32);
                writer.write_raw(&ep.secret);
            }
        }

        // CDN public keys (version >= 2).
        writer.write_i32(cdn_entries.len() as i32);
        for (dc_id, n, e) in cdn_entries {
            writer.write_i32(dc_id);
            writer.write_byte_array(n);
            writer.write_byte_array(e);
        }

        writer.into_bytes()
    }

    /// Replaces endpoints and CDN public keys from tdesktop's `QDataStream::Qt_5_1` serialized
    /// bytes (`DcOptions::constructFromSerialized` in `mtproto_dc_options.cpp`).
    ///
    /// Supports format versions `0`, `1`, and `2`. Returns `false` without mutating `self`
    /// if the input is truncated or malformed.
    pub fn construct_from_serialized(&self, serialized: &[u8]) -> bool {
        let Some((new_data, new_cdn_keys)) = parse_serialized_dc_options(serialized) else {
            return false;
        };
        let mut guard = self.inner.write();
        guard.data = new_data;
        guard.cdn_public_keys = new_cdn_keys;
        guard.recompute_cdn_dc_ids();
        true
    }

    /// Constructs a new `DcOptions` for `environment` initialized from `serialized` bytes.
    pub fn from_serialized(environment: Environment, serialized: &[u8]) -> Option<Self> {
        let opts = Self::new(environment);
        if opts.construct_from_serialized(serialized) {
            Some(opts)
        } else {
            None
        }
    }
}

type ParsedDcOptions = (
    BTreeMap<DcId, Vec<DcEndpoint>>,
    BTreeMap<DcId, BTreeMap<u64, PublicKey>>,
);

fn parse_serialized_dc_options(serialized: &[u8]) -> Option<ParsedDcOptions> {
    let mut reader = QDataReader::new(serialized);
    let minus_version = reader.read_i32()?;
    let (version, count) = if minus_version < 0 {
        (-minus_version, reader.read_i32()?)
    } else {
        (0, minus_version)
    };
    if !(0..=10_000).contains(&count) {
        return None;
    }

    let mut data: BTreeMap<DcId, Vec<DcEndpoint>> = BTreeMap::new();
    for _ in 0..count {
        let id = reader.read_i32()?;
        let flags = reader.read_i32()?;
        let port = reader.read_i32()?;
        let ip_size = reader.read_i32()?;
        if ip_size <= 0 || (ip_size as usize) > MAX_IP_SIZE {
            return None;
        }
        let ip_bytes = reader.read_raw(ip_size as usize)?;
        let ip = std::str::from_utf8(ip_bytes).ok()?.to_string();

        let secret = if version > 0 {
            let secret_size = reader.read_i32()?;
            if secret_size < 0 || (secret_size as usize) > MAX_SECRET_SIZE {
                return None;
            }
            if secret_size > 0 {
                reader.read_raw(secret_size as usize)?.to_vec()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        if port <= 0 || port > i32::from(u16::MAX) {
            return None;
        }
        apply_one_option(
            &mut data,
            DcEndpoint {
                id,
                flags: DcOptionFlags(flags as u32),
                ip,
                port: port as u16,
                secret,
            },
        );
    }

    let mut cdn_public_keys: BTreeMap<DcId, BTreeMap<u64, PublicKey>> = BTreeMap::new();
    if !reader.at_end() && version > 1 {
        let cdn_count = reader.read_i32()?;
        if !(0..=1024).contains(&cdn_count) {
            return None;
        }
        for _ in 0..cdn_count {
            let dc_id = reader.read_i32()?;
            let n = reader.read_byte_array()?;
            let e = reader.read_byte_array()?;
            let key = PublicKey::from_components(&n, &e).ok()?;
            cdn_public_keys
                .entry(dc_id)
                .or_default()
                .insert(key.fingerprint(), key);
        }
    }

    Some((data, cdn_public_keys))
}

impl DcOptionsInner {
    fn load_built_in(&mut self) {
        self.data.clear();
        let rsa_pems = if self.environment == Environment::Test {
            TEST_PUBLIC_KEYS
        } else {
            PRODUCTION_PUBLIC_KEYS
        };
        for pem in rsa_pems {
            if let Ok(key) = PublicKey::from_pem(pem) {
                self.public_keys.insert(key.fingerprint(), key);
            }
        }
        let v4 = if self.environment == Environment::Test {
            BUILT_IN_DCS_TEST
        } else {
            BUILT_IN_DCS
        };
        for &(id, ip, port) in v4 {
            apply_one_option(
                &mut self.data,
                DcEndpoint {
                    id,
                    flags: DcOptionFlags(DcOptionFlags::STATIC),
                    ip: ip.to_string(),
                    port,
                    secret: Vec::new(),
                },
            );
        }
        let v6 = if self.environment == Environment::Test {
            BUILT_IN_DCS_IPV6_TEST
        } else {
            BUILT_IN_DCS_IPV6
        };
        for &(id, ip, port) in v6 {
            apply_one_option(
                &mut self.data,
                DcEndpoint {
                    id,
                    flags: DcOptionFlags(DcOptionFlags::STATIC | DcOptionFlags::IPV6),
                    ip: ip.to_string(),
                    port,
                    secret: Vec::new(),
                },
            );
        }
        self.recompute_cdn_dc_ids();
    }

    fn recompute_cdn_dc_ids(&mut self) {
        self.cdn_dc_ids.clear();
        for (&dc_id, endpoints) in &self.data {
            if let Some(first) = endpoints.first()
                && first.flags.contains(DcOptionFlags::CDN)
            {
                self.cdn_dc_ids.insert(bare_dc_id(dc_id));
            }
        }
    }

    fn has_media_only_options_for(&self, dc_id: DcId) -> bool {
        self.data
            .get(&dc_id)
            .map(|list| {
                list.iter()
                    .any(|ep| ep.flags.contains(DcOptionFlags::MEDIA_ONLY))
            })
            .unwrap_or(false)
    }
}

fn apply_one_option(data: &mut BTreeMap<DcId, Vec<DcEndpoint>>, ep: DcEndpoint) -> bool {
    let entry = data.entry(ep.id).or_default();
    if entry
        .iter()
        .any(|existing| existing.ip == ep.ip && existing.port == ep.port)
    {
        return false;
    }
    entry.push(ep);
    true
}

fn filter_if_has_with_flag(variants: &mut EndpointVariants, flag: u32) {
    for list in [
        &mut variants.ipv4_tcp,
        &mut variants.ipv4_http,
        &mut variants.ipv6_tcp,
        &mut variants.ipv6_http,
    ] {
        if list.iter().any(|ep| ep.flags.contains(flag)) {
            list.retain(|ep| ep.flags.contains(flag));
        }
    }
}

fn count_options_difference(
    a: &BTreeMap<DcId, Vec<DcEndpoint>>,
    b: &BTreeMap<DcId, Vec<DcEndpoint>>,
) -> Vec<DcId> {
    let mut keys: BTreeSet<DcId> = a.keys().copied().collect();
    keys.extend(b.keys().copied());
    let mut diff = Vec::new();
    for dc_id in keys {
        match (a.get(&dc_id), b.get(&dc_id)) {
            (Some(la), Some(lb)) => {
                let same = la.len() == lb.len()
                    && la
                        .iter()
                        .all(|ea| lb.iter().any(|eb| eb.ip == ea.ip && eb.port == ea.port));
                if !same {
                    diff.push(dc_id);
                }
            }
            _ => diff.push(dc_id),
        }
    }
    diff
}
