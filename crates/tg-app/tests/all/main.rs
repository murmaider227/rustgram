//! Unified integration test binary for `tg-app`.
//!
//! Consolidates the `e2e_*` integration test modules into a single test binary so
//! `cargo test -p tg-app` compiles and links against `tg_app` once instead of once per file.

#[path = "../common/mod.rs"]
mod common;

mod e2e_a11y;
mod e2e_adaptive;
mod e2e_bots;
mod e2e_boxes;
mod e2e_communities;
mod e2e_deep_links;
mod e2e_dialogs;
mod e2e_fu1;
mod e2e_fu4_glyphs;
mod e2e_history;
mod e2e_intro;
mod e2e_layer233;
mod e2e_live_window;
mod e2e_multi_account;
mod e2e_notifications;
mod e2e_peer_management;
mod e2e_profile_extras;
mod e2e_profile_media;
mod e2e_rich_editor;
mod e2e_settings;
mod e2e_share_theme_qr_entry;
mod e2e_snapshots;
mod e2e_subsection_tabs;
mod e2e_wire1;
mod e2e_wp52_ux_extras;
mod e2e_wp60_cocoon;
mod e2e_wp61_engagement_ui;
mod e2e_wp62_svg_wallpapers;
mod e2e_wp63_profile_stickers_ui;
mod e2e_wp64_wallet_calls_ui;

mod live_inline_tests;
mod live_tests;
mod live_tests_cable_tunnel;
mod live_tests_calls_e2e;
mod live_tests_chat_import;
mod live_tests_compose_extras;
mod live_tests_deep_links;
mod live_tests_export_themes;
mod live_tests_langpack;
mod live_tests_passkeys;
mod live_tests_proxy;
mod live_tests_search_media_polish;
mod live_tests_settings_sync;
mod live_tests_star_gifts;
mod live_tests_story_compose;
mod live_tests_wallet;
mod live_tests_widget_helper;
mod live_tests_widgets;
mod live_tests_window_wiring;
mod live_tests_wire3;
mod live_tests_wp12;
mod live_tests_wp21;
mod live_tests_wp28;
mod live_tests_wp31_wiring;
mod live_tests_wp32_entrypoints;
mod live_tests_wp33_placeholders;
mod live_tests_wp34_web_proxy_miniapp;
mod live_tests_wp35_win32_geo;
mod live_tests_wp36_folders_topics;
mod live_tests_wp36_window;
mod live_tests_wp37_inline_media;
mod live_tests_wp37b;
mod live_tests_wp38_chat_l233;
mod live_tests_wp38b;
mod live_tests_wp39_wallet_collectibles;
mod live_tests_wp40_polish;
mod live_tests_wp40_wiring;
mod live_tests_wp50_payments_live;
mod live_tests_wp51_chat_cards;
