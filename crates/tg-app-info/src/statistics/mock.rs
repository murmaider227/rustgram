//! Mock [`StatsBackend`] serving `tg-stats` sample JSON charts and synthetic overview data.
//!
//! All names, channels, bots, and transaction identifiers in this file are strictly
//! synthetic placeholders (no real people's names or real Telegram accounts).

use std::collections::BTreeMap;

use tg_stats::{
    AdministratorActionsInfo, Boost, BoostPrepaidGiveaway, BoostStatus, BoostsListSlice,
    BoostsOffsetToken, BoostsOverview, ChannelStatistics, CreditsAmount, CreditsEarnStatistics,
    CreditsType, EarnHistoryEntry, EarnHistorySlice, EarnStatistics, GiftCodeLink, InviterInfo,
    MessageInteractionInfo, MessageSenderInfo, MessageStatistics, ONE_DAY_MS, StatisticalChart,
    StatisticalGraph, StatisticalValue, StatisticsLists, SupergroupStatistics,
};

use crate::statistics::backend::{
    BoostStatusBundle, BotEarnBundle, ChannelEarnBundle, ChannelStatsBundle, MessageStatsBundle,
    PeerDisplayInfo, PublicForwardRow, RecentPostPreview, StatsBackend, SupergroupStatsBundle,
};

/// Sample JSON charts embedded directly from `crates/tg-stats/assets/`.
const SAMPLE_LINEAR_JSON: &str = include_str!("../../../tg-stats/assets/sample_linear.json");
const SAMPLE_DOUBLE_LINEAR_JSON: &str =
    include_str!("../../../tg-stats/assets/sample_double_linear.json");
const SAMPLE_BAR_JSON: &str = include_str!("../../../tg-stats/assets/sample_bar.json");
const SAMPLE_STACK_BAR_JSON: &str = include_str!("../../../tg-stats/assets/sample_stack_bar.json");
const SAMPLE_STACK_LINEAR_JSON: &str =
    include_str!("../../../tg-stats/assets/sample_stack_linear.json");

/// Synthetic TON ad revenue stacked-bar JSON (`yTickFormatter: statsFormat('TON')`).
const SYNTHETIC_TON_REVENUE_JSON: &str = r#"{
  "columns": [
    ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000, 1700438400000, 1700524800000, 1700611200000, 1700697600000, 1700784000000],
    ["y0", 1250000000, 1840000000, 1620000000, 2310000000, 2750000000, 2190000000, 3120000000, 3480000000, 2940000000, 3850000000]
  ],
  "types": { "x": "x", "y0": "bar" },
  "names": { "y0": "Ad Revenue" },
  "colors": { "y0": "BLUE#327fe5" },
  "stacked": true,
  "yTickFormatter": "statsFormat('TON')",
  "subchart": { "show": true }
}"#;

/// Synthetic Stars revenue stacked-bar JSON (`yTickFormatter: statsFormat('XTR')`).
const SYNTHETIC_STARS_REVENUE_JSON: &str = r#"{
  "columns": [
    ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000, 1700438400000, 1700524800000, 1700611200000, 1700697600000, 1700784000000],
    ["y0", 420, 680, 540, 890, 1120, 960, 1340, 1520, 1210, 1680]
  ],
  "types": { "x": "x", "y0": "bar" },
  "names": { "y0": "Stars Revenue" },
  "colors": { "y0": "GOLDEN#eba52d" },
  "stacked": true,
  "yTickFormatter": "statsFormat('XTR')",
  "subchart": { "show": true }
}"#;

/// Synthetic single-series Growth linear JSON.
const SYNTHETIC_GROWTH_JSON: &str = r#"{
  "columns": [
    ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000, 1700438400000, 1700524800000, 1700611200000, 1700697600000, 1700784000000, 1700870400000, 1700956800000, 1701043200000, 1701129600000],
    ["y0", 14020, 14135, 14226, 14369, 14550, 14701, 14903, 15137, 15320, 15570, 15849, 16092, 16399, 16742]
  ],
  "types": { "x": "x", "y0": "line" },
  "names": { "y0": "Total Followers" },
  "colors": { "y0": "BLUE#327fe5" },
  "subchart": {
    "show": true,
    "defaultZoom": [1700265600000, 1701129600000]
  }
}"#;

/// Synthetic Views by Hour JSON (hourly x-axis 0..23, footer hidden).
const SYNTHETIC_HOURLY_JSON: &str = r#"{
  "columns": [
    ["x", 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23],
    ["y0", 310, 240, 195, 170, 185, 290, 520, 890, 1340, 1780, 2050, 2290, 2410, 2350, 2180, 2460, 2720, 2890, 2640, 2210, 1840, 1420, 960, 580],
    ["y1", 260, 210, 160, 150, 160, 245, 460, 790, 1200, 1590, 1880, 2090, 2150, 2120, 1990, 2210, 2430, 2580, 2390, 1980, 1620, 1250, 840, 490]
  ],
  "types": { "x": "x", "y0": "line", "y1": "line" },
  "names": { "y0": "This week", "y1": "Last week" },
  "colors": { "y0": "BLUE#327fe5", "y1": "LIGHTBLUE#58a8ed" },
  "subchart": { "show": false }
}"#;

/// Synthetic Notifications mute/unmute linear JSON.
const SYNTHETIC_MUTE_JSON: &str = r#"{
  "columns": [
    ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000, 1700352000000, 1700438400000, 1700524800000, 1700611200000, 1700697600000, 1700784000000],
    ["y0", 48, 52, 61, 44, 70, 58, 65, 73, 55, 81],
    ["y1", 19, 24, 21, 30, 18, 22, 16, 25, 29, 17]
  ],
  "types": { "x": "x", "y0": "line", "y1": "line" },
  "names": { "y0": "Unmuted", "y1": "Muted" },
  "colors": { "y0": "GREEN#61c752", "y1": "RED#e05356" },
  "subchart": { "show": true }
}"#;

/// Synthetic fake implementation of [`StatsBackend`].
#[derive(Debug, Clone, Default)]
pub struct MockStatsBackend;

impl MockStatsBackend {
    /// Creates a new mock statistics backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn parse_graph(json: &str, zoom_token: &str) -> StatisticalGraph {
        let chart = StatisticalChart::from_json_str(json).unwrap_or_default();
        StatisticalGraph {
            chart,
            zoom_token: zoom_token.to_owned(),
            error: String::new(),
        }
    }

    fn channel_peer() -> PeerDisplayInfo {
        PeerDisplayInfo {
            id: 100_420_001,
            title: "Signal Tech Digest".to_owned(),
            subtitle: "@signal_tech_digest".to_owned(),
            initials: "ST".to_owned(),
            avatar_rgb: [0x32, 0x7F, 0xE5],
        }
    }

    fn supergroup_peer() -> PeerDisplayInfo {
        PeerDisplayInfo {
            id: 100_420_002,
            title: "Rust Systems Forum".to_owned(),
            subtitle: "@rust_systems_forum".to_owned(),
            initials: "RS".to_owned(),
            avatar_rgb: [0x61, 0xC7, 0x52],
        }
    }

    fn bot_peer() -> PeerDisplayInfo {
        PeerDisplayInfo {
            id: 500_420_003,
            title: "Release Helper Bot".to_owned(),
            subtitle: "@release_helper_bot".to_owned(),
            initials: "RH".to_owned(),
            avatar_rgb: [0xEB, 0xA5, 0x2D],
        }
    }

    fn mock_users() -> BTreeMap<u64, PeerDisplayInfo> {
        let entries = [
            (
                1001,
                "Contributor Alpha",
                "@contrib_alpha",
                "CA",
                [0x32, 0x7F, 0xE5],
            ),
            (
                1002,
                "Contributor Beta",
                "@contrib_beta",
                "CB",
                [0x61, 0xC7, 0x52],
            ),
            (
                1003,
                "Contributor Gamma",
                "@contrib_gamma",
                "CG",
                [0xF2, 0x8C, 0x39],
            ),
            (
                1004,
                "Contributor Delta",
                "@contrib_delta",
                "CD",
                [0x9F, 0x79, 0xE8],
            ),
            (
                1005,
                "Contributor Epsilon",
                "@contrib_epsilon",
                "CE",
                [0x40, 0xD0, 0xCA],
            ),
            (1006, "Moderator One", "@mod_one", "M1", [0xE0, 0x53, 0x56]),
            (1007, "Moderator Two", "@mod_two", "M2", [0x7F, 0x79, 0xF3]),
            (
                1008,
                "Community Host",
                "@community_host",
                "CH",
                [0x58, 0xA8, 0xED],
            ),
            (
                1009,
                "Archive Maintainer",
                "@archive_maint",
                "AM",
                [0x8F, 0xCF, 0x39],
            ),
            (
                1010,
                "Release Coordinator",
                "@release_coord",
                "RC",
                [0xEB, 0xA5, 0x2D],
            ),
        ];
        entries
            .into_iter()
            .map(|(id, title, sub, init, rgb)| {
                (
                    id,
                    PeerDisplayInfo {
                        id,
                        title: title.to_owned(),
                        subtitle: sub.to_owned(),
                        initials: init.to_owned(),
                        avatar_rgb: rgb,
                    },
                )
            })
            .collect()
    }

    fn mock_recent_posts() -> Vec<RecentPostPreview> {
        type PostSpec = (i32, i32, &'static str, i32, i32, i32, [u8; 3], &'static str);
        let base_ts = 1_701_129_600i64; // 28 Nov 2023 00:00 UTC
        let specs: &[PostSpec] = &[
            (
                901,
                0,
                "Release 2.4 notes: zero-copy chart rendering and smoother 60 fps rulers",
                18_420,
                412,
                638,
                [0x32, 0x7F, 0xE5],
                "R2",
            ),
            (
                0,
                401,
                "Story: Behind the scenes of our weekly benchmark dashboard",
                9_840,
                165,
                410,
                [0x9F, 0x79, 0xE8],
                "ST",
            ),
            (
                902,
                0,
                "Architecture deep-dive: segment trees for O(log n) chart Y-axis limits",
                14_910,
                328,
                512,
                [0x61, 0xC7, 0x52],
                "AD",
            ),
            (
                903,
                0,
                "Community poll: preferred layout density for desktop analytics panels",
                12_350,
                184,
                445,
                [0xF2, 0x8C, 0x39],
                "CP",
            ),
            (
                0,
                402,
                "Story: Quick tour of Night Mode contrast adjustments",
                8_120,
                92,
                289,
                [0x40, 0xD0, 0xCA],
                "NM",
            ),
            (
                904,
                0,
                "Weekly engineering summary: storage format and startup latency wins",
                11_640,
                215,
                376,
                [0x7F, 0x79, 0xF3],
                "WS",
            ),
            (
                905,
                0,
                "Monetization guide: understanding TON ad revenue and Stars payouts",
                15_890,
                490,
                594,
                [0xEB, 0xA5, 0x2D],
                "MG",
            ),
            (
                906,
                0,
                "Boosts milestone: Level 4 unlocked with custom channel reactions",
                13_210,
                274,
                502,
                [0xE0, 0x53, 0x56],
                "BM",
            ),
            (
                907,
                0,
                "Design tokens update: crisp 1px hairlines and accessible color palettes",
                9_430,
                132,
                268,
                [0x58, 0xA8, 0xED],
                "DT",
            ),
            (
                908,
                0,
                "Q&A recap: keyboard navigation and multi-column adaptive breakpoints",
                10_780,
                198,
                319,
                [0x8F, 0xCF, 0x39],
                "QA",
            ),
            (
                909,
                0,
                "Performance benchmark: 120-day chart scrubbing under 1.2 ms per frame",
                12_940,
                286,
                421,
                [0x32, 0x7F, 0xE5],
                "PB",
            ),
            (
                910,
                0,
                "Audio & media pipeline notes: low-latency playback and decoding caches",
                8_650,
                119,
                204,
                [0x61, 0xC7, 0x52],
                "AM",
            ),
            (
                911,
                0,
                "Localization update: plural rules and compact number formatting",
                7_920,
                84,
                187,
                [0xF2, 0x8C, 0x39],
                "LU",
            ),
            (
                912,
                0,
                "Monthly changelog archive and contributor acknowledgements",
                11_120,
                203,
                355,
                [0x9F, 0x79, 0xE8],
                "MC",
            ),
        ];

        specs
            .iter()
            .enumerate()
            .map(
                |(i, &(msg_id, story_id, snippet, views, forwards, reactions, rgb, init))| {
                    RecentPostPreview {
                        interaction: MessageInteractionInfo {
                            message_id: msg_id,
                            story_id,
                            views_count: views,
                            forwards_count: forwards,
                            reactions_count: reactions,
                        },
                        is_story: story_id != 0,
                        snippet: snippet.to_owned(),
                        date_unix: base_ts - (i as i64) * 54_000,
                        author_title: "Signal Tech Digest".to_owned(),
                        initials: init.to_owned(),
                        avatar_rgb: rgb,
                    }
                },
            )
            .collect()
    }

    fn build_hourly_zoom_graph(zoom_token: &str, x_timestamp: f64) -> StatisticalGraph {
        type SeriesDef = (&'static str, &'static str, &'static str, i64, i64);
        type ZoomSpec = (
            Vec<SeriesDef>,
            bool,
            bool,
            &'static str,
            Option<&'static str>,
        );

        const HOUR_MS: f64 = 3_600_000.0;
        let base_ts = if x_timestamp >= ONE_DAY_MS {
            x_timestamp
        } else {
            1_700_611_200_000.0
        };
        let day_seed = ((base_ts / ONE_DAY_MS) as i64).unsigned_abs() as usize;

        let mut x_col = vec![serde_json::Value::String("x".to_owned())];
        for h in 0..24 {
            x_col.push(serde_json::json!(base_ts + (h as f64) * HOUR_MS));
        }

        let (series_defs, stacked, y_scaled, chart_type_str, tick_fmt): ZoomSpec = match zoom_token
        {
            "zoom_growth" => (
                vec![("y0", "Total Followers", "BLUE#327fe5", 15_100, 18)],
                false,
                false,
                "line",
                None,
            ),
            "zoom_followers" => (
                vec![
                    ("y0", "Joined", "GREEN#61c752", 14, 9),
                    ("y1", "Left", "RED#e05356", 4, 3),
                ],
                false,
                false,
                "line",
                None,
            ),
            "zoom_mute" => (
                vec![
                    ("y0", "Unmuted", "GREEN#61c752", 8, 5),
                    ("y1", "Muted", "RED#e05356", 3, 2),
                ],
                false,
                false,
                "line",
                None,
            ),
            "zoom_interactions" => (
                vec![
                    ("y0", "Views", "BLUE#327fe5", 1_120, 640),
                    ("y1", "Shares", "GREEN#61c752", 16, 11),
                ],
                false,
                true,
                "line",
                None,
            ),
            "zoom_reactions" => (
                vec![
                    ("y0", "Positive", "CYAN#40d0ca", 12, 8),
                    ("y1", "Negative", "INDIGO#7f79f3", 4, 3),
                ],
                false,
                false,
                "bar",
                None,
            ),
            "zoom_sources" => (
                vec![
                    ("y0", "Followers", "BLUE#327fe5", 45, 28),
                    ("y1", "Search", "LIGHTGREEN#8fcf39", 18, 12),
                    ("y2", "Channels", "ORANGE#f28c39", 10, 8),
                ],
                true,
                false,
                "bar",
                None,
            ),
            "zoom_ton_revenue" => (
                vec![("y0", "Ad Revenue", "BLUE#327fe5", 120_000_000, 90_000_000)],
                true,
                false,
                "bar",
                Some("statsFormat('TON')"),
            ),
            "zoom_stars_revenue" => (
                vec![("y0", "Stars Revenue", "GOLDEN#eba52d", 52, 38)],
                true,
                false,
                "bar",
                Some("statsFormat('XTR')"),
            ),
            _ => (
                vec![
                    ("y0", "Primary", "BLUE#327fe5", 120, 60),
                    ("y1", "Secondary", "GREEN#61c752", 42, 24),
                ],
                false,
                false,
                "line",
                None,
            ),
        };

        let mut columns = vec![serde_json::Value::Array(x_col)];
        let mut types = serde_json::Map::new();
        types.insert("x".to_owned(), serde_json::Value::String("x".to_owned()));
        let mut names = serde_json::Map::new();
        let mut colors = serde_json::Map::new();

        for (s_idx, (key, name, color, base_val, amp)) in series_defs.into_iter().enumerate() {
            let mut col = vec![serde_json::Value::String(key.to_owned())];
            for h in 0..24 {
                // Smooth diurnal bell curve peaking mid-afternoon, modulated by day_seed.
                let phase = ((h as f64 - 14.0) / 5.5).powi(2);
                let bell = (-phase).exp();
                let ripple = (((h + day_seed + s_idx * 3) as f64) * 0.9).sin() * 0.18;
                let value = if zoom_token == "zoom_growth" {
                    base_val + (h as i64) * amp + ((day_seed % 7) as i64) * 12
                } else {
                    let scaled = (base_val as f64) * (0.35 + bell) + (amp as f64) * (0.5 + ripple);
                    scaled.round().max(1.0) as i64
                };
                col.push(serde_json::json!(value));
            }
            columns.push(serde_json::Value::Array(col));
            types.insert(
                key.to_owned(),
                serde_json::Value::String(chart_type_str.to_owned()),
            );
            names.insert(key.to_owned(), serde_json::Value::String(name.to_owned()));
            colors.insert(key.to_owned(), serde_json::Value::String(color.to_owned()));
        }

        let mut root = serde_json::Map::new();
        root.insert("columns".to_owned(), serde_json::Value::Array(columns));
        root.insert("types".to_owned(), serde_json::Value::Object(types));
        root.insert("names".to_owned(), serde_json::Value::Object(names));
        root.insert("colors".to_owned(), serde_json::Value::Object(colors));
        root.insert("stacked".to_owned(), serde_json::Value::Bool(stacked));
        root.insert("y_scaled".to_owned(), serde_json::Value::Bool(y_scaled));
        root.insert("subchart".to_owned(), serde_json::json!({ "show": false }));
        if let Some(fmt) = tick_fmt {
            root.insert(
                "yTickFormatter".to_owned(),
                serde_json::Value::String(fmt.to_owned()),
            );
        }

        let json_bytes = serde_json::to_vec(&serde_json::Value::Object(root)).unwrap_or_default();
        let mut chart = StatisticalChart::from_json(&json_bytes).unwrap_or_default();
        if zoom_token == "zoom_ton_revenue" {
            chart.currency_rate = 5.42;
        } else if zoom_token == "zoom_stars_revenue" {
            chart.currency_rate = 0.013;
        }

        StatisticalGraph {
            chart,
            zoom_token: String::new(),
            error: String::new(),
        }
    }
}

impl StatsBackend for MockStatsBackend {
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String> {
        let stats = ChannelStatistics {
            start_date: 1_700_006_400, // 15 Nov 2023
            end_date: 1_701_129_600,   // 28 Nov 2023
            member_count: StatisticalValue::from_current_and_previous(16_742.0, 15_410.0),
            mean_view_count: StatisticalValue::from_current_and_previous(12_480.0, 11_320.0),
            mean_share_count: StatisticalValue::from_current_and_previous(264.0, 218.0),
            mean_reaction_count: StatisticalValue::from_current_and_previous(438.0, 392.0),
            mean_story_view_count: StatisticalValue::from_current_and_previous(8_960.0, 9_450.0),
            mean_story_share_count: StatisticalValue::from_current_and_previous(128.0, 104.0),
            mean_story_reaction_count: StatisticalValue::from_current_and_previous(349.0, 310.0),
            enabled_notifications_percentage:
                ChannelStatistics::compute_enabled_notifications_percentage(13_812.0, 16_742.0),
            member_count_graph: Self::parse_graph(SYNTHETIC_GROWTH_JSON, "zoom_growth"),
            join_graph: Self::parse_graph(SAMPLE_LINEAR_JSON, "zoom_followers"),
            mute_graph: Self::parse_graph(SYNTHETIC_MUTE_JSON, "zoom_mute"),
            view_count_by_hour_graph: Self::parse_graph(SYNTHETIC_HOURLY_JSON, ""),
            view_count_by_source_graph: Self::parse_graph(SAMPLE_STACK_BAR_JSON, "zoom_sources"),
            join_by_source_graph: Self::parse_graph(SAMPLE_STACK_BAR_JSON, "zoom_sources"),
            language_graph: Self::parse_graph(SAMPLE_STACK_LINEAR_JSON, ""),
            message_interaction_graph: Self::parse_graph(
                SAMPLE_DOUBLE_LINEAR_JSON,
                "zoom_interactions",
            ),
            instant_view_interaction_graph: Self::parse_graph(
                SAMPLE_DOUBLE_LINEAR_JSON,
                "zoom_interactions",
            ),
            reactions_by_emotion_graph: Self::parse_graph(SAMPLE_BAR_JSON, "zoom_reactions"),
            story_interactions_graph: Self::parse_graph(
                SAMPLE_DOUBLE_LINEAR_JSON,
                "zoom_interactions",
            ),
            story_reactions_by_emotion_graph: Self::parse_graph(SAMPLE_BAR_JSON, "zoom_reactions"),
        };

        Ok(ChannelStatsBundle {
            peer: Self::channel_peer(),
            stats,
            recent_posts: Self::mock_recent_posts(),
        })
    }

    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String> {
        let stats = SupergroupStatistics {
            start_date: 1_700_006_400,
            end_date: 1_701_129_600,
            member_count: StatisticalValue::from_current_and_previous(4_820.0, 4_510.0),
            message_count: StatisticalValue::from_current_and_previous(18_940.0, 16_820.0),
            viewer_count: StatisticalValue::from_current_and_previous(2_140.0, 2_280.0),
            sender_count: StatisticalValue::from_current_and_previous(612.0, 540.0),
            member_count_graph: Self::parse_graph(SYNTHETIC_GROWTH_JSON, "zoom_growth"),
            join_graph: Self::parse_graph(SAMPLE_LINEAR_JSON, "zoom_followers"),
            join_by_source_graph: Self::parse_graph(SAMPLE_STACK_BAR_JSON, "zoom_sources"),
            language_graph: Self::parse_graph(SAMPLE_STACK_LINEAR_JSON, ""),
            message_content_graph: Self::parse_graph(SAMPLE_STACK_BAR_JSON, "zoom_sources"),
            action_graph: Self::parse_graph(SAMPLE_DOUBLE_LINEAR_JSON, "zoom_interactions"),
            day_graph: Self::parse_graph(SYNTHETIC_HOURLY_JSON, ""),
            week_graph: Self::parse_graph(SAMPLE_STACK_LINEAR_JSON, ""),
        };

        let lists = StatisticsLists {
            recent_message_interactions: Vec::new(),
            top_senders: vec![
                MessageSenderInfo {
                    user_id: 1001,
                    sent_message_count: 412,
                    average_character_count: 84,
                },
                MessageSenderInfo {
                    user_id: 1002,
                    sent_message_count: 356,
                    average_character_count: 112,
                },
                MessageSenderInfo {
                    user_id: 1003,
                    sent_message_count: 289,
                    average_character_count: 67,
                },
                MessageSenderInfo {
                    user_id: 1004,
                    sent_message_count: 241,
                    average_character_count: 95,
                },
                MessageSenderInfo {
                    user_id: 1005,
                    sent_message_count: 198,
                    average_character_count: 53,
                },
                MessageSenderInfo {
                    user_id: 1008,
                    sent_message_count: 164,
                    average_character_count: 72,
                },
            ],
            top_administrators: vec![
                AdministratorActionsInfo {
                    user_id: 1006,
                    deleted_message_count: 42,
                    banned_user_count: 6,
                    restricted_user_count: 11,
                },
                AdministratorActionsInfo {
                    user_id: 1007,
                    deleted_message_count: 28,
                    banned_user_count: 3,
                    restricted_user_count: 5,
                },
                AdministratorActionsInfo {
                    user_id: 1010,
                    deleted_message_count: 15,
                    banned_user_count: 0,
                    restricted_user_count: 2,
                },
            ],
            top_inviters: vec![
                InviterInfo {
                    user_id: 1008,
                    added_member_count: 34,
                },
                InviterInfo {
                    user_id: 1009,
                    added_member_count: 21,
                },
                InviterInfo {
                    user_id: 1001,
                    added_member_count: 14,
                },
            ],
        };

        Ok(SupergroupStatsBundle {
            peer: Self::supergroup_peer(),
            stats,
            lists,
            users: Self::mock_users(),
        })
    }

    fn message_statistics(&self, message_id: i32) -> Result<MessageStatsBundle, String> {
        let posts = Self::mock_recent_posts();
        let post = posts
            .into_iter()
            .find(|p| !p.is_story && p.interaction.message_id == message_id)
            .unwrap_or_else(|| RecentPostPreview {
                interaction: MessageInteractionInfo {
                    message_id,
                    story_id: 0,
                    views_count: 18_420,
                    forwards_count: 412,
                    reactions_count: 638,
                },
                is_story: false,
                snippet: format!("Channel post #{message_id} performance summary"),
                date_unix: 1_701_129_600,
                author_title: "Signal Tech Digest".to_owned(),
                initials: "ST".to_owned(),
                avatar_rgb: [0x32, 0x7F, 0xE5],
            });

        let stats = MessageStatistics {
            message_interaction_graph: Self::parse_graph(
                SAMPLE_DOUBLE_LINEAR_JSON,
                "zoom_interactions",
            ),
            reactions_by_emotion_graph: Self::parse_graph(SAMPLE_BAR_JSON, "zoom_reactions"),
            public_forwards: (post.interaction.forwards_count / 3).max(1),
            private_forwards: (post.interaction.forwards_count * 2 / 3).max(1),
            views: post.interaction.views_count,
            reactions: post.interaction.reactions_count,
        };

        let public_forwards = vec![
            PublicForwardRow {
                peer: PeerDisplayInfo {
                    id: 200_101,
                    title: "Desktop Architecture News".to_owned(),
                    subtitle: "@desktop_arch_news".to_owned(),
                    initials: "DA".to_owned(),
                    avatar_rgb: [0x40, 0xD0, 0xCA],
                },
                message_id: 4_120,
                story_id: 0,
                subscribers: 24_600,
                views: 6_340,
            },
            PublicForwardRow {
                peer: PeerDisplayInfo {
                    id: 200_102,
                    title: "Open Tooling Weekly".to_owned(),
                    subtitle: "@open_tooling_weekly".to_owned(),
                    initials: "OT".to_owned(),
                    avatar_rgb: [0xF2, 0x8C, 0x39],
                },
                message_id: 1_892,
                story_id: 0,
                subscribers: 11_200,
                views: 3_180,
            },
            PublicForwardRow {
                peer: PeerDisplayInfo {
                    id: 200_103,
                    title: "Graphics & UI Feed".to_owned(),
                    subtitle: "@graphics_ui_feed".to_owned(),
                    initials: "GU".to_owned(),
                    avatar_rgb: [0x9F, 0x79, 0xE8],
                },
                message_id: 774,
                story_id: 0,
                subscribers: 8_450,
                views: 2_090,
            },
        ];

        Ok(MessageStatsBundle {
            peer: Self::channel_peer(),
            post,
            stats,
            public_forwards,
            public_forwards_next_offset: String::new(),
        })
    }

    fn story_statistics(&self, story_id: i32) -> Result<MessageStatsBundle, String> {
        let posts = Self::mock_recent_posts();
        let post = posts
            .into_iter()
            .find(|p| p.is_story && p.interaction.story_id == story_id)
            .unwrap_or_else(|| RecentPostPreview {
                interaction: MessageInteractionInfo {
                    message_id: 0,
                    story_id,
                    views_count: 9_840,
                    forwards_count: 165,
                    reactions_count: 410,
                },
                is_story: true,
                snippet: format!("Channel story #{story_id} interaction breakdown"),
                date_unix: 1_701_075_600,
                author_title: "Signal Tech Digest".to_owned(),
                initials: "ST".to_owned(),
                avatar_rgb: [0x9F, 0x79, 0xE8],
            });

        let stats = MessageStatistics {
            message_interaction_graph: Self::parse_graph(
                SAMPLE_DOUBLE_LINEAR_JSON,
                "zoom_interactions",
            ),
            reactions_by_emotion_graph: Self::parse_graph(SAMPLE_BAR_JSON, "zoom_reactions"),
            public_forwards: (post.interaction.forwards_count / 4).max(1),
            private_forwards: (post.interaction.forwards_count * 3 / 4).max(1),
            views: post.interaction.views_count,
            reactions: post.interaction.reactions_count,
        };

        let public_forwards = vec![PublicForwardRow {
            peer: PeerDisplayInfo {
                id: 200_201,
                title: "UI Motion Showcase".to_owned(),
                subtitle: "@ui_motion_showcase".to_owned(),
                initials: "UM".to_owned(),
                avatar_rgb: [0x7F, 0x79, 0xF3],
            },
            message_id: 0,
            story_id: 92,
            subscribers: 15_300,
            views: 4_120,
        }];

        Ok(MessageStatsBundle {
            peer: Self::channel_peer(),
            post,
            stats,
            public_forwards,
            public_forwards_next_offset: String::new(),
        })
    }

    fn load_zoom_graph(
        &self,
        zoom_token: &str,
        x_timestamp: f64,
    ) -> Result<StatisticalGraph, String> {
        if zoom_token.is_empty() {
            return Err("Chart does not support remote zoom".to_owned());
        }
        Ok(Self::build_hourly_zoom_graph(zoom_token, x_timestamp))
    }

    fn boost_status(&self) -> Result<BoostStatusBundle, String> {
        let base_date = 1_700_611_200i64;
        let month_secs = 30 * 86_400;

        let first_slice_boosts = BoostsListSlice {
            list: vec![
                Boost {
                    id: "boost_1".to_owned(),
                    user_id: 1001,
                    giveaway_message_id: 0,
                    date: base_date - 5 * 86_400,
                    expires_at: base_date + 25 * 86_400,
                    expires_after_months: 1,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 1,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: false,
                    is_unclaimed: false,
                },
                Boost {
                    id: "boost_2".to_owned(),
                    user_id: 1002,
                    giveaway_message_id: 906,
                    date: base_date - 3 * 86_400,
                    expires_at: base_date + 3 * month_secs,
                    expires_after_months: 3,
                    gift_code_link: GiftCodeLink {
                        text: "t.me/giftcode/sample-code-01".to_owned(),
                        link: "https://t.me/giftcode/sample-code-01".to_owned(),
                        slug: "sample-code-01".to_owned(),
                    },
                    multiplier: 4,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: false,
                },
                Boost {
                    id: "boost_3".to_owned(),
                    user_id: 1003,
                    giveaway_message_id: 0,
                    date: base_date - 2 * 86_400,
                    expires_at: base_date + 6 * month_secs,
                    expires_after_months: 6,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 4,
                    credits: 0,
                    is_gift: true,
                    is_giveaway: false,
                    is_unclaimed: false,
                },
                Boost {
                    id: "boost_4".to_owned(),
                    user_id: 0,
                    giveaway_message_id: 906,
                    date: base_date - 86_400,
                    expires_at: base_date + 3 * month_secs,
                    expires_after_months: 3,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 4,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: true,
                },
                Boost {
                    id: "boost_5".to_owned(),
                    user_id: 0,
                    giveaway_message_id: 907,
                    date: base_date,
                    expires_at: base_date + 3 * month_secs,
                    expires_after_months: 3,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 4,
                    credits: 2_500,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: false,
                },
            ],
            multiplied_total: 19,
            all_loaded: false,
            token: BoostsOffsetToken {
                next: "boosts_page_2".to_owned(),
                gifts: false,
            },
        };

        let first_slice_gifts = BoostsListSlice {
            list: vec![
                Boost {
                    id: "gift_1".to_owned(),
                    user_id: 1002,
                    giveaway_message_id: 906,
                    date: base_date - 3 * 86_400,
                    expires_at: base_date + 3 * month_secs,
                    expires_after_months: 3,
                    gift_code_link: GiftCodeLink {
                        text: "t.me/giftcode/sample-code-01".to_owned(),
                        link: "https://t.me/giftcode/sample-code-01".to_owned(),
                        slug: "sample-code-01".to_owned(),
                    },
                    multiplier: 4,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: false,
                },
                Boost {
                    id: "gift_2".to_owned(),
                    user_id: 1003,
                    giveaway_message_id: 0,
                    date: base_date - 2 * 86_400,
                    expires_at: base_date + 6 * month_secs,
                    expires_after_months: 6,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 4,
                    credits: 0,
                    is_gift: true,
                    is_giveaway: false,
                    is_unclaimed: false,
                },
                Boost {
                    id: "gift_3".to_owned(),
                    user_id: 0,
                    giveaway_message_id: 906,
                    date: base_date - 86_400,
                    expires_at: base_date + 3 * month_secs,
                    expires_after_months: 3,
                    gift_code_link: GiftCodeLink::default(),
                    multiplier: 4,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: false,
                },
            ],
            multiplied_total: 12,
            all_loaded: true,
            token: BoostsOffsetToken {
                next: String::new(),
                gifts: true,
            },
        };

        let status = BoostStatus {
            overview: BoostsOverview {
                group: false,
                mine: 1,
                level: 4,
                boost_count: 19,
                current_level_boost_count: 15,
                next_level_boost_count: 25,
                premium_member_count: 340,
                premium_member_percentage: 12.3,
            },
            first_slice_boosts,
            first_slice_gifts,
            prepaid_giveaway: vec![
                BoostPrepaidGiveaway {
                    date: base_date - 7 * 86_400,
                    id: 7001,
                    credits: 0,
                    months: 3,
                    quantity: 5,
                    boosts: 20,
                },
                BoostPrepaidGiveaway {
                    date: base_date - 2 * 86_400,
                    id: 7002,
                    credits: 5_000,
                    months: 0,
                    quantity: 10,
                    boosts: 12,
                },
            ],
            link: "https://t.me/boost/signal_tech_digest".to_owned(),
        };

        Ok(BoostStatusBundle {
            peer: Self::channel_peer(),
            status,
            users: Self::mock_users(),
            giveaway_boosts_per_premium: 4,
        })
    }

    fn load_more_boosts(&self, token: &BoostsOffsetToken) -> Result<BoostsListSlice, String> {
        let base_date = 1_700_611_200i64;
        if token.next == "boosts_page_2" {
            Ok(BoostsListSlice {
                list: vec![
                    Boost {
                        id: "boost_6".to_owned(),
                        user_id: 1004,
                        giveaway_message_id: 0,
                        date: base_date - 10 * 86_400,
                        expires_at: base_date + 20 * 86_400,
                        expires_after_months: 1,
                        gift_code_link: GiftCodeLink::default(),
                        multiplier: 1,
                        credits: 0,
                        is_gift: false,
                        is_giveaway: false,
                        is_unclaimed: false,
                    },
                    Boost {
                        id: "boost_7".to_owned(),
                        user_id: 1005,
                        giveaway_message_id: 0,
                        date: base_date - 12 * 86_400,
                        expires_at: base_date + 18 * 86_400,
                        expires_after_months: 1,
                        gift_code_link: GiftCodeLink::default(),
                        multiplier: 1,
                        credits: 0,
                        is_gift: false,
                        is_giveaway: false,
                        is_unclaimed: false,
                    },
                ],
                multiplied_total: 19,
                all_loaded: true,
                token: BoostsOffsetToken {
                    next: String::new(),
                    gifts: token.gifts,
                },
            })
        } else {
            Ok(BoostsListSlice {
                list: Vec::new(),
                multiplied_total: 0,
                all_loaded: true,
                token: BoostsOffsetToken {
                    next: String::new(),
                    gifts: token.gifts,
                },
            })
        }
    }

    fn channel_earn(&self) -> Result<ChannelEarnBundle, String> {
        let ton_rate = 5.42;
        let stars_rate = 0.013;

        let mut ton_rev_graph = Self::parse_graph(SYNTHETIC_TON_REVENUE_JSON, "zoom_ton_revenue");
        ton_rev_graph.chart.currency_rate = ton_rate;

        let mut stars_rev_graph =
            Self::parse_graph(SYNTHETIC_STARS_REVENUE_JSON, "zoom_stars_revenue");
        stars_rev_graph.chart.currency_rate = stars_rate;

        let ton = EarnStatistics {
            top_hours_graph: Self::parse_graph(SAMPLE_BAR_JSON, "zoom_reactions"),
            revenue_graph: ton_rev_graph,
            available_balance: CreditsAmount::from_nanoton(24_850_000_000),
            current_balance: CreditsAmount::from_nanoton(31_420_000_000),
            overall_revenue: CreditsAmount::from_nanoton(148_960_000_000),
            usd_rate: ton_rate,
            switched_off: false,
            first_history_slice: EarnHistorySlice {
                list: vec![
                    EarnHistoryEntry {
                        id: "ton_tx_1".to_owned(),
                        title: "Proceeds from Telegram Ads".to_owned(),
                        date: 1_700_784_000,
                        ads_proceeds_to_date: 1_701_129_600,
                        provider: String::new(),
                        credits: CreditsAmount::from_nanoton(6_570_000_000),
                        incoming: true,
                        pending: false,
                        failed: false,
                    },
                    EarnHistoryEntry {
                        id: "ton_tx_2".to_owned(),
                        title: "Withdrawal via Fragment".to_owned(),
                        date: 1_700_524_800,
                        ads_proceeds_to_date: 0,
                        provider: "UQBx7SampleWalletAddr0001_RustgramMock".to_owned(),
                        credits: CreditsAmount::from_nanoton(15_000_000_000),
                        incoming: false,
                        pending: false,
                        failed: false,
                    },
                    EarnHistoryEntry {
                        id: "ton_tx_3".to_owned(),
                        title: "Proceeds from Telegram Ads".to_owned(),
                        date: 1_700_179_200,
                        ads_proceeds_to_date: 1_700_524_800,
                        provider: String::new(),
                        credits: CreditsAmount::from_nanoton(5_240_000_000),
                        incoming: true,
                        pending: false,
                        failed: false,
                    },
                    EarnHistoryEntry {
                        id: "ton_tx_4".to_owned(),
                        title: "Withdrawal via Fragment".to_owned(),
                        date: 1_700_006_400,
                        ads_proceeds_to_date: 0,
                        provider: "UQBx7SampleWalletAddr0001_RustgramMock".to_owned(),
                        credits: CreditsAmount::from_nanoton(4_000_000_000),
                        incoming: false,
                        pending: false,
                        failed: true,
                    },
                ],
                total: 6,
                all_loaded: false,
                token: "earn_page_2".to_owned(),
            },
        };

        let credits = CreditsEarnStatistics {
            revenue_graph: stars_rev_graph,
            current_balance: CreditsAmount::from_whole(12_840, CreditsType::Stars),
            available_balance: CreditsAmount::from_whole(9_650, CreditsType::Stars),
            overall_revenue: CreditsAmount::from_whole(38_420, CreditsType::Stars),
            usd_rate: stars_rate,
            is_withdrawal_enabled: true,
            next_withdrawal_at: 0,
            buy_ads_url: "https://ads.telegram.org".to_owned(),
        };

        let credits_history = EarnHistorySlice {
            list: vec![
                EarnHistoryEntry {
                    id: "ch_stars_1".to_owned(),
                    title: "Star Reactions on Post #901".to_owned(),
                    date: 1_701_100_000,
                    ads_proceeds_to_date: 0,
                    provider: "Community Supporters".to_owned(),
                    credits: CreditsAmount::from_whole(1_450, CreditsType::Stars),
                    incoming: true,
                    pending: false,
                    failed: false,
                },
                EarnHistoryEntry {
                    id: "ch_stars_2".to_owned(),
                    title: "Paid Media Unlock".to_owned(),
                    date: 1_700_980_000,
                    ads_proceeds_to_date: 0,
                    provider: "Channel Archive".to_owned(),
                    credits: CreditsAmount::from_whole(820, CreditsType::Stars),
                    incoming: true,
                    pending: false,
                    failed: false,
                },
                EarnHistoryEntry {
                    id: "ch_stars_3".to_owned(),
                    title: "Stars Withdrawal to Fragment".to_owned(),
                    date: 1_700_720_000,
                    ads_proceeds_to_date: 0,
                    provider: "Fragment Payout".to_owned(),
                    credits: CreditsAmount::from_whole(2_000, CreditsType::Stars),
                    incoming: false,
                    pending: true,
                    failed: false,
                },
            ],
            total: 3,
            all_loaded: true,
            token: String::new(),
        };

        Ok(ChannelEarnBundle {
            peer: Self::channel_peer(),
            ton,
            credits,
            credits_history,
            can_view_currency: true,
            can_view_credits: true,
            withdrawal_enabled: true,
            restrict_sponsored_level_min: 50,
            current_level: 4,
        })
    }

    fn load_more_channel_earn_history(&self, token: &str) -> Result<EarnHistorySlice, String> {
        if token == "earn_page_2" {
            Ok(EarnHistorySlice {
                list: vec![
                    EarnHistoryEntry {
                        id: "ton_tx_5".to_owned(),
                        title: "Proceeds from Telegram Ads".to_owned(),
                        date: 1_699_574_400,
                        ads_proceeds_to_date: 1_699_920_000,
                        provider: String::new(),
                        credits: CreditsAmount::from_nanoton(4_890_000_000),
                        incoming: true,
                        pending: false,
                        failed: false,
                    },
                    EarnHistoryEntry {
                        id: "ton_tx_6".to_owned(),
                        title: "Withdrawal via Fragment".to_owned(),
                        date: 1_699_228_800,
                        ads_proceeds_to_date: 0,
                        provider: "UQBx7SampleWalletAddr0001_RustgramMock".to_owned(),
                        credits: CreditsAmount::from_nanoton(10_000_000_000),
                        incoming: false,
                        pending: false,
                        failed: false,
                    },
                ],
                total: 6,
                all_loaded: true,
                token: String::new(),
            })
        } else {
            Ok(EarnHistorySlice {
                list: Vec::new(),
                total: 0,
                all_loaded: true,
                token: String::new(),
            })
        }
    }

    fn bot_earn(&self) -> Result<BotEarnBundle, String> {
        let stars_rate = 0.013;
        let mut rev_graph = Self::parse_graph(SYNTHETIC_STARS_REVENUE_JSON, "zoom_stars_revenue");
        rev_graph.chart.currency_rate = stars_rate;

        let credits = CreditsEarnStatistics {
            revenue_graph: rev_graph,
            current_balance: CreditsAmount::from_whole(18_250, CreditsType::Stars),
            available_balance: CreditsAmount::from_whole(14_600, CreditsType::Stars),
            overall_revenue: CreditsAmount::from_whole(64_900, CreditsType::Stars),
            usd_rate: stars_rate,
            is_withdrawal_enabled: true,
            next_withdrawal_at: 1_701_561_600,
            buy_ads_url: "https://ads.telegram.org".to_owned(),
        };

        let all_entries = vec![
            EarnHistoryEntry {
                id: "bot_tx_1".to_owned(),
                title: "Pro Toolkit Subscription".to_owned(),
                date: 1_701_110_000,
                ads_proceeds_to_date: 0,
                provider: "Contributor Alpha".to_owned(),
                credits: CreditsAmount::from_whole(250, CreditsType::Stars),
                incoming: true,
                pending: false,
                failed: false,
            },
            EarnHistoryEntry {
                id: "bot_tx_2".to_owned(),
                title: "Export Pack Purchase".to_owned(),
                date: 1_701_040_000,
                ads_proceeds_to_date: 0,
                provider: "Contributor Beta".to_owned(),
                credits: CreditsAmount::from_whole(150, CreditsType::Stars),
                incoming: true,
                pending: false,
                failed: false,
            },
            EarnHistoryEntry {
                id: "bot_tx_3".to_owned(),
                title: "Withdrawal via Fragment".to_owned(),
                date: 1_700_890_000,
                ads_proceeds_to_date: 0,
                provider: "Fragment Payout".to_owned(),
                credits: CreditsAmount::from_whole(5_000, CreditsType::Stars),
                incoming: false,
                pending: false,
                failed: false,
            },
            EarnHistoryEntry {
                id: "bot_tx_4".to_owned(),
                title: "Telegram Ads Campaign".to_owned(),
                date: 1_700_640_000,
                ads_proceeds_to_date: 0,
                provider: "Telegram Ads".to_owned(),
                credits: CreditsAmount::from_whole(1_200, CreditsType::Stars),
                incoming: false,
                pending: false,
                failed: false,
            },
            EarnHistoryEntry {
                id: "bot_tx_5".to_owned(),
                title: "CI Webhook Quota Top-Up".to_owned(),
                date: 1_700_510_000,
                ads_proceeds_to_date: 0,
                provider: "Contributor Gamma".to_owned(),
                credits: CreditsAmount::from_whole(500, CreditsType::Stars),
                incoming: true,
                pending: false,
                failed: false,
            },
        ];

        let in_list: Vec<_> = all_entries.iter().filter(|e| e.incoming).cloned().collect();
        let out_list: Vec<_> = all_entries
            .iter()
            .filter(|e| !e.incoming)
            .cloned()
            .collect();

        Ok(BotEarnBundle {
            peer: Self::bot_peer(),
            credits,
            full_history: EarnHistorySlice {
                total: all_entries.len() as i32,
                list: all_entries,
                all_loaded: true,
                token: String::new(),
            },
            in_history: EarnHistorySlice {
                total: in_list.len() as i32,
                list: in_list,
                all_loaded: true,
                token: String::new(),
            },
            out_history: EarnHistorySlice {
                total: out_list.len() as i32,
                list: out_list,
                all_loaded: true,
                token: String::new(),
            },
            affiliate_allowed: true,
        })
    }
}
