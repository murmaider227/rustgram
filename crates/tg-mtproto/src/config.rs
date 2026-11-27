//! Port of `mtproto/mtproto_config.cpp` plus caller-supplied client configuration
//! (`api_id`, device/app metadata, lang codes, proxy, transport racing, PFS toggle).
//!
//! Includes tdesktop-compatible `QDataStream::Qt_5_1` binary persistence (`Config::serialize` /
//! `Config::from_serialized`) so `tg-storage` can store and reload MTProto configs.
//! Never embeds any hardcoded `api_id` or `api_hash`.

use std::time::Duration;

use tg_tl::{enums as tl_enums, types as tl_types};
use tg_transport::{ProxyData, ProxyType, RaceOptions, TransportKind};

use crate::dc_options::{DEFAULT_MAIN_DC, DcId, DcOptions, Environment};
use crate::qdatastream::{QDataReader, QDataWriter};

/// Serialization format version matching `kVersion = 1` in `mtproto_config.cpp`.
const CONFIG_SERIALIZE_VERSION: i32 = 1;

/// Default reaction emoji (`"\xf0\x9f\x91\x8d"`, 👍) matching `ConfigDefaultReactionEmoji`.
pub const DEFAULT_REACTION_EMOJI: &str = "\u{1F44D}";

/// Optional proxy address reported inside `initConnection.proxy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProxyInfo {
    /// Proxy hostname or IP.
    pub address: String,
    /// Proxy port.
    pub port: i32,
}

/// Caller-supplied configuration for [`crate::Client`].
///
/// All app-identifying fields (`api_id`, `device_model`, `system_version`, `app_version`,
/// language codes) are supplied by the caller at runtime; none are hardcoded in this crate.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Telegram API ID (must be provided by the caller when sending `initConnection`).
    pub api_id: i32,
    /// Device model string passed in `initConnection`.
    pub device_model: String,
    /// Operating system version passed in `initConnection`.
    pub system_version: String,
    /// Application version passed in `initConnection`.
    pub app_version: String,
    /// System language code (e.g. `"en"`).
    pub system_lang_code: String,
    /// Cloud language pack name (e.g. `"tdesktop"`).
    pub lang_pack: String,
    /// Active UI language code (e.g. `"en"`).
    pub lang_code: String,
    /// Optional timezone offset in seconds (rounded to 900s like tdesktop's `prepareInitParams`).
    pub tz_offset_secs: Option<i32>,
    /// Optional MTProto proxy info explicitly reported in `initConnection`.
    /// When `None`, automatically populated from `proxy` if `proxy.proxy_type == ProxyType::Mtproto`.
    pub init_proxy: Option<ClientProxyInfo>,
    /// Active proxy configuration (direct, SOCKS5, HTTP CONNECT, or MTProxy incl. fake-TLS).
    pub proxy: ProxyData,
    /// Primary MTProto TCP framing transport (default: [`TransportKind::Intermediate`]).
    pub transport: TransportKind,
    /// Additional TCP transports raced alongside `transport` (e.g. `PaddedIntermediate`, `Abridged`).
    pub fallback_transports: Vec<TransportKind>,
    /// Enable obfuscated2 TCP encryption (default: `true`; always forced on with secrets/MTProxy).
    pub obfuscated: bool,
    /// Enable IPv4 connection candidates during connection racing (default: `true`).
    pub use_ipv4: bool,
    /// Enable IPv6 connection candidates during connection racing (default: `false`).
    pub use_ipv6: bool,
    /// Prefer IPv6 priority over IPv4 during connection racing (`OptionPreferIPv6`, default: `false`).
    pub prefer_ipv6: bool,
    /// Enable TCP candidates during connection racing (default: `true`).
    pub use_tcp: bool,
    /// Enable plain HTTP `/api` candidates during connection racing (default: `true`).
    pub use_http: bool,
    /// Send a `req_pq_multi`/`resPQ` connectivity probe during connection racing (default: `true`).
    pub probe_pq_on_connect: bool,
    /// Timing options for [`tg_transport::race_connections`] (`kWaitForBetterTimeout` = 2s).
    pub race_options: RaceOptions,
    /// Production or Test datacenters.
    pub environment: Environment,
    /// Initial main DC ID (defaults to DC 2).
    pub main_dc_id: DcId,
    /// Whether to use Perfect Forward Secrecy temporary auth keys + `auth.bindTempAuthKey`
    /// (tdesktop default: `true`).
    pub use_pfs: bool,
    /// Optional phone prefix string used solely for filtering `help.configSimple` access-point rules
    /// (empty matches wildcard rules).
    pub phone_prefix_hint: String,
    /// Maximum `FLOOD_WAIT` seconds to wait and retry automatically before returning the error
    /// to the caller (`None` = always auto-retry as in tdesktop; `Some(0)` = never auto-wait).
    pub max_auto_flood_wait_secs: Option<u32>,
    /// Keepalive ping interval on the main session (default: 30s).
    pub ping_interval: Duration,
    /// `ping_delay_disconnect` disconnect delay in seconds (default: 60s).
    pub ping_disconnect_delay_secs: i32,
}

impl ClientConfig {
    /// Returns the `inputClientProxy` info to embed in `initConnection`, preferring explicit
    /// `init_proxy` or falling back to `proxy` when using an MTProto/Web proxy
    /// (matching `session_private.cpp:689-695`).
    pub fn effective_init_proxy(&self) -> Option<ClientProxyInfo> {
        if let Some(info) = &self.init_proxy {
            return Some(info.clone());
        }
        if matches!(self.proxy.proxy_type, ProxyType::Mtproto | ProxyType::Web)
            && !self.proxy.host.is_empty()
            && self.proxy.port != 0
        {
            return Some(ClientProxyInfo {
                address: self.proxy.host.clone(),
                port: i32::from(self.proxy.port),
            });
        }
        None
    }
}

/// Language fields of `initConnection` (`SessionOptions` `systemLangCode`, `langPackName`,
/// `cloudLangCode`). They can change at run time, when the user switches the app language.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitLang {
    /// System language code.
    pub system_lang_code: String,
    /// Cloud language pack name (`""` for a custom pack).
    pub lang_pack: String,
    /// Cloud language code of the current pack.
    pub lang_code: String,
}

impl InitLang {
    /// The language fields of `config`.
    pub fn from_config(config: &ClientConfig) -> Self {
        Self {
            system_lang_code: config.system_lang_code.clone(),
            lang_pack: config.lang_pack.clone(),
            lang_code: config.lang_code.clone(),
        }
    }

    /// Writes these fields into `config`.
    pub fn apply_to(&self, config: &mut ClientConfig) {
        config.system_lang_code.clone_from(&self.system_lang_code);
        config.lang_pack.clone_from(&self.lang_pack);
        config.lang_code.clone_from(&self.lang_code);
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            api_id: 0,
            device_model: "Rustgram Desktop".to_string(),
            system_version: "Windows".to_string(),
            app_version: "Rustgram 0.1.0".to_string(),
            system_lang_code: "en".to_string(),
            // Match tdesktop `Lang::Instance::langPackName()` (`lang/lang_instance.cpp`) and
            // `SessionPrivate::tryToSend` (`mtproto/session_private.cpp`), which always send
            // `"tdesktop"` as `initConnection.lang_pack`.
            lang_pack: "tdesktop".to_string(),
            lang_code: "en".to_string(),
            tz_offset_secs: Some(0),
            init_proxy: None,
            proxy: ProxyData::none(),
            transport: TransportKind::Intermediate,
            fallback_transports: Vec::new(),
            obfuscated: true,
            use_ipv4: true,
            use_ipv6: false,
            prefer_ipv6: false,
            use_tcp: true,
            use_http: true,
            probe_pq_on_connect: true,
            race_options: RaceOptions::default(),
            environment: Environment::Production,
            main_dc_id: DEFAULT_MAIN_DC,
            use_pfs: true,
            phone_prefix_hint: String::new(),
            max_auto_flood_wait_secs: Some(60),
            ping_interval: Duration::from_secs(30),
            ping_disconnect_delay_secs: 60,
        }
    }
}

/// Server-pushed configuration parameters (port of `MTP::ConfigFields` in `mtproto_config.h/.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFields {
    pub chat_size_max: i32,
    pub megagroup_size_max: i32,
    pub forwarded_count_max: i32,
    pub online_update_period_ms: i32,
    pub offline_blur_timeout_ms: i32,
    pub offline_idle_timeout_ms: i32,
    pub online_focus_timeout_ms: i32,
    pub online_cloud_timeout_ms: i32,
    pub notify_cloud_delay_ms: i32,
    pub notify_default_delay_ms: i32,
    pub edit_time_limit: i32,
    pub revoke_time_limit: i32,
    pub revoke_private_time_limit: i32,
    pub revoke_private_inbox: bool,
    pub stickers_recent_limit: i32,
    pub internal_links_domain: String,
    pub channels_read_media_period: i32,
    pub call_receive_timeout_ms: i32,
    pub call_ring_timeout_ms: i32,
    pub call_connect_timeout_ms: i32,
    pub call_packet_timeout_ms: i32,
    pub web_file_dc_id: DcId,
    pub txt_domain_string: String,
    pub blocked_mode: bool,
    pub caption_length_max: i32,
    pub rating_decay: i32,
    pub reaction_default_emoji: String,
    pub reaction_default_custom: u64,
    pub autologin_token: String,
    pub gif_search_username: String,
}

impl ConfigFields {
    /// Creates default `ConfigFields` for `environment` (matching `mtproto_config.cpp`).
    pub fn for_environment(environment: Environment) -> Self {
        let is_test = environment == Environment::Test;
        Self {
            chat_size_max: 200,
            megagroup_size_max: 10_000,
            forwarded_count_max: 100,
            online_update_period_ms: 120_000,
            offline_blur_timeout_ms: 5_000,
            offline_idle_timeout_ms: 30_000,
            online_focus_timeout_ms: 1_000,
            online_cloud_timeout_ms: 300_000,
            notify_cloud_delay_ms: 30_000,
            notify_default_delay_ms: 1_500,
            edit_time_limit: 172_800,
            revoke_time_limit: 172_800,
            revoke_private_time_limit: 172_800,
            revoke_private_inbox: false,
            stickers_recent_limit: 30,
            internal_links_domain: "https://t.me/".to_string(),
            channels_read_media_period: 86_400 * 7,
            call_receive_timeout_ms: 20_000,
            call_ring_timeout_ms: 90_000,
            call_connect_timeout_ms: 30_000,
            call_packet_timeout_ms: 10_000,
            web_file_dc_id: if is_test { 2 } else { 4 },
            txt_domain_string: if is_test {
                "tapv3.stel.com".to_string()
            } else {
                "apv3.stel.com".to_string()
            },
            blocked_mode: false,
            caption_length_max: 1024,
            rating_decay: 2_419_200,
            reaction_default_emoji: DEFAULT_REACTION_EMOJI.to_string(),
            reaction_default_custom: 0,
            autologin_token: String::new(),
            gif_search_username: if is_test {
                "izgifbot".to_string()
            } else {
                "gif".to_string()
            },
        }
    }

    /// Applies fields from a server `config#cc1a241e` (`Config::apply` in `mtproto_config.cpp`).
    pub fn apply_tl(&mut self, data: &tl_types::Config, environment: Environment) {
        self.chat_size_max = data.chat_size_max;
        self.megagroup_size_max = data.megagroup_size_max;
        self.forwarded_count_max = data.forwarded_count_max;
        self.online_update_period_ms = data.online_update_period_ms;
        self.offline_blur_timeout_ms = data.offline_blur_timeout_ms;
        self.offline_idle_timeout_ms = data.offline_idle_timeout_ms;
        self.online_cloud_timeout_ms = data.online_cloud_timeout_ms;
        self.notify_cloud_delay_ms = data.notify_cloud_delay_ms;
        self.notify_default_delay_ms = data.notify_default_delay_ms;
        self.edit_time_limit = data.edit_time_limit;
        self.revoke_time_limit = data.revoke_time_limit;
        self.revoke_private_time_limit = data.revoke_pm_time_limit;
        self.revoke_private_inbox = data.revoke_pm_inbox;
        self.stickers_recent_limit = data.stickers_recent_limit;
        self.internal_links_domain = data.me_url_prefix.clone();
        self.channels_read_media_period = data.channels_read_media_period;
        self.web_file_dc_id = data.webfile_dc_id;
        self.call_receive_timeout_ms = data.call_receive_timeout_ms;
        self.call_ring_timeout_ms = data.call_ring_timeout_ms;
        self.call_connect_timeout_ms = data.call_connect_timeout_ms;
        self.call_packet_timeout_ms = data.call_packet_timeout_ms;
        self.blocked_mode = data.blocked_mode;
        self.caption_length_max = data.caption_length_max;
        self.reaction_default_emoji = DEFAULT_REACTION_EMOJI.to_string();
        self.reaction_default_custom = 0;
        if let Some(reaction) = &data.reactions_default {
            match reaction {
                tl_enums::Reaction::Empty => {}
                tl_enums::Reaction::Emoji(emoji) => {
                    self.reaction_default_emoji = emoji.emoticon.clone();
                }
                tl_enums::Reaction::CustomEmoji(custom) => {
                    self.reaction_default_custom = custom.document_id as u64;
                }
                tl_enums::Reaction::Paid => {
                    self.reaction_default_emoji = "*".to_string();
                }
            }
        }
        self.autologin_token = data.autologin_token.clone().unwrap_or_default();
        self.rating_decay = if data.rating_e_decay > 0 {
            data.rating_e_decay
        } else {
            Self::for_environment(environment).rating_decay
        };
        self.gif_search_username = data.gif_search_username.clone().unwrap_or_default();
        if !data.dc_txt_domain_name.is_empty() {
            self.txt_domain_string = data.dc_txt_domain_name.clone();
        }
    }
}

/// Complete MTProto configuration pairing [`DcOptions`] and [`ConfigFields`]
/// (port of `MTP::Config` in `mtproto/mtproto_config.h/.cpp`).
///
/// Supports byte-for-byte `QDataStream::Qt_5_1` persistence via [`Config::serialize`] and
/// [`Config::from_serialized`] so `tg-storage` can persist and reload configs seamlessly.
#[derive(Debug, Clone)]
pub struct Config {
    dc_options: DcOptions,
    fields: ConfigFields,
}

impl Config {
    /// Creates a default `Config` for `environment`.
    pub fn new(environment: Environment) -> Self {
        Self {
            dc_options: DcOptions::new(environment),
            fields: ConfigFields::for_environment(environment),
        }
    }

    /// Creates a `Config` from explicit [`DcOptions`] and [`ConfigFields`].
    pub fn from_parts(dc_options: DcOptions, fields: ConfigFields) -> Self {
        Self { dc_options, fields }
    }

    /// Returns the environment (`Production` or `Test`).
    pub fn environment(&self) -> Environment {
        self.dc_options.environment()
    }

    /// Returns `true` if configured for Telegram test servers.
    pub fn is_test_mode(&self) -> bool {
        self.dc_options.is_test_mode()
    }

    /// Returns a reference to the underlying [`DcOptions`].
    pub fn dc_options(&self) -> &DcOptions {
        &self.dc_options
    }

    /// Returns a reference to the current [`ConfigFields`].
    pub fn values(&self) -> &ConfigFields {
        &self.fields
    }

    /// Returns a mutable reference to [`ConfigFields`].
    pub fn values_mut(&mut self) -> &mut ConfigFields {
        &mut self.fields
    }

    /// Applies a `help.getConfig` response (`Config::apply` in `mtproto_config.cpp`).
    ///
    /// Returns `false` if `data.test_mode` does not match `self.is_test_mode()`.
    pub fn apply(&mut self, data: &tl_types::Config) -> bool {
        if data.test_mode != self.is_test_mode() {
            tracing::warn!(
                expected_test_mode = self.is_test_mode(),
                got_test_mode = data.test_mode,
                "config with wrong test_mode field received"
            );
            return false;
        }
        let env = self.environment();
        self.fields.apply_tl(data, env);
        if !data.dc_options.is_empty() {
            self.dc_options.set_from_list(&data.dc_options);
        }
        true
    }

    /// Serializes this `Config` (including embedded `DcOptions`) into tdesktop's
    /// `QDataStream::Qt_5_1` byte format (`Config::serialize` in `mtproto_config.cpp`).
    pub fn serialize(&self) -> Vec<u8> {
        let options = self.dc_options.serialize();
        let mut w = QDataWriter::with_capacity(256 + options.len());

        w.write_i32(CONFIG_SERIALIZE_VERSION);
        w.write_i32(if self.dc_options.is_test_mode() {
            Environment::Test.as_i32()
        } else {
            Environment::Production.as_i32()
        });
        w.write_byte_array(&options);
        w.write_i32(self.fields.chat_size_max);
        w.write_i32(self.fields.megagroup_size_max);
        w.write_i32(self.fields.forwarded_count_max);
        w.write_i32(self.fields.online_update_period_ms);
        w.write_i32(self.fields.offline_blur_timeout_ms);
        w.write_i32(self.fields.offline_idle_timeout_ms);
        w.write_i32(self.fields.online_focus_timeout_ms);
        w.write_i32(self.fields.online_cloud_timeout_ms);
        w.write_i32(self.fields.notify_cloud_delay_ms);
        w.write_i32(self.fields.notify_default_delay_ms);
        w.write_i32(0); // legacy savedGifsLimit
        w.write_i32(self.fields.edit_time_limit);
        w.write_i32(self.fields.revoke_time_limit);
        w.write_i32(self.fields.revoke_private_time_limit);
        w.write_i32(if self.fields.revoke_private_inbox {
            1
        } else {
            0
        });
        w.write_i32(self.fields.stickers_recent_limit);
        w.write_i32(0); // legacy stickersFavedLimit
        w.write_i32(0); // legacy pinnedDialogsCountMax
        w.write_i32(0); // legacy pinnedDialogsInFolderMax
        w.write_qstring(&self.fields.internal_links_domain);
        w.write_i32(self.fields.channels_read_media_period);
        w.write_i32(self.fields.call_receive_timeout_ms);
        w.write_i32(self.fields.call_ring_timeout_ms);
        w.write_i32(self.fields.call_connect_timeout_ms);
        w.write_i32(self.fields.call_packet_timeout_ms);
        w.write_i32(self.fields.web_file_dc_id);
        w.write_qstring(&self.fields.txt_domain_string);
        w.write_i32(1); // legacy phoneCallsEnabled
        w.write_i32(if self.fields.blocked_mode { 1 } else { 0 });
        w.write_i32(self.fields.caption_length_max);
        w.write_qstring(&self.fields.reaction_default_emoji);
        w.write_u64(self.fields.reaction_default_custom);
        w.write_i32(self.fields.rating_decay);
        w.write_qstring(&self.fields.gif_search_username);
        w.write_qstring(""); // legacy venue search username

        w.into_bytes()
    }

    /// Deserializes a `Config` from tdesktop's `QDataStream::Qt_5_1` byte format
    /// (`Config::FromSerialized` in `mtproto_config.cpp`).
    pub fn from_serialized(serialized: &[u8]) -> Option<Self> {
        let mut r = QDataReader::new(serialized);
        let version = r.read_i32()?;
        if version != CONFIG_SERIALIZE_VERSION {
            return None;
        }
        let env_raw = r.read_i32()?;
        let environment = Environment::from_i32(env_raw)?;

        let mut cfg = Self::new(environment);
        let dc_options_bytes = r.read_byte_array()?;

        cfg.fields.chat_size_max = r.read_i32()?;
        cfg.fields.megagroup_size_max = r.read_i32()?;
        cfg.fields.forwarded_count_max = r.read_i32()?;
        cfg.fields.online_update_period_ms = r.read_i32()?;
        cfg.fields.offline_blur_timeout_ms = r.read_i32()?;
        cfg.fields.offline_idle_timeout_ms = r.read_i32()?;
        cfg.fields.online_focus_timeout_ms = r.read_i32()?;
        cfg.fields.online_cloud_timeout_ms = r.read_i32()?;
        cfg.fields.notify_cloud_delay_ms = r.read_i32()?;
        cfg.fields.notify_default_delay_ms = r.read_i32()?;
        let _legacy_saved_gifs_limit = r.read_i32()?;
        cfg.fields.edit_time_limit = r.read_i32()?;
        cfg.fields.revoke_time_limit = r.read_i32()?;
        cfg.fields.revoke_private_time_limit = r.read_i32()?;
        cfg.fields.revoke_private_inbox = r.read_i32()? == 1;
        cfg.fields.stickers_recent_limit = r.read_i32()?;
        let _legacy_stickers_faved_limit = r.read_i32()?;
        let _legacy_pinned_dialogs_max = r.read_i32()?;
        let _legacy_pinned_dialogs_in_folder_max = r.read_i32()?;
        cfg.fields.internal_links_domain = r.read_qstring()?;
        cfg.fields.channels_read_media_period = r.read_i32()?;
        cfg.fields.call_receive_timeout_ms = r.read_i32()?;
        cfg.fields.call_ring_timeout_ms = r.read_i32()?;
        cfg.fields.call_connect_timeout_ms = r.read_i32()?;
        cfg.fields.call_packet_timeout_ms = r.read_i32()?;
        cfg.fields.web_file_dc_id = r.read_i32()?;
        cfg.fields.txt_domain_string = r.read_qstring()?;
        let _legacy_phone_calls_enabled = r.read_i32()?;
        cfg.fields.blocked_mode = r.read_i32()? == 1;
        cfg.fields.caption_length_max = r.read_i32()?;

        if !r.at_end() {
            cfg.fields.reaction_default_emoji = r.read_qstring()?;
            cfg.fields.reaction_default_custom = r.read_u64()?;
        }
        if !r.at_end() {
            cfg.fields.rating_decay = r.read_i32()?;
        }
        if !r.at_end() {
            cfg.fields.gif_search_username = r.read_qstring()?;
            let _legacy_venue_search_username = r.read_qstring()?;
        }

        if !cfg.dc_options.construct_from_serialized(&dc_options_bytes) {
            return None;
        }
        Some(cfg)
    }
}
