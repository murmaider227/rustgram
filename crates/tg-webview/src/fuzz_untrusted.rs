//! Panic fuzz over Mini App bridge messages and other web-page input (TGB-3065).

use std::time::Instant;

use serde_json::{Map, Value, json};
use tg_fuzz::Fuzz;
use tg_theme::Palette;

use crate::interface::WebMessage;
use crate::mini_app::{MiniAppController, MiniAppPanelConfig};

const ITERATIONS: usize = 3_000;

const COMMANDS: &[&str] = &[
    "iframe_ready",
    "iframe_will_reload",
    "web_app_add_to_home_screen",
    "web_app_biometry_get_info",
    "web_app_biometry_open_settings",
    "web_app_biometry_request_access",
    "web_app_biometry_request_auth",
    "web_app_biometry_update_token",
    "web_app_check_home_screen",
    "web_app_check_location",
    "web_app_close",
    "web_app_close_scan_qr_popup",
    "web_app_data_send",
    "web_app_device_storage_clear",
    "web_app_device_storage_get_key",
    "web_app_device_storage_save_key",
    "web_app_exit_fullscreen",
    "web_app_expand",
    "web_app_invoke_custom_method",
    "web_app_open_invoice",
    "web_app_open_link",
    "web_app_open_location_settings",
    "web_app_open_popup",
    "web_app_open_scan_qr_popup",
    "web_app_open_tg_link",
    "web_app_read_text_from_clipboard",
    "web_app_ready",
    "web_app_request_chat",
    "web_app_request_content_safe_area",
    "web_app_request_emoji_status_access",
    "web_app_request_file_download",
    "web_app_request_fullscreen",
    "web_app_request_location",
    "web_app_request_phone",
    "web_app_request_safe_area",
    "web_app_request_theme",
    "web_app_request_viewport",
    "web_app_request_write_access",
    "web_app_secure_storage_clear",
    "web_app_secure_storage_get_key",
    "web_app_secure_storage_restore_key",
    "web_app_secure_storage_save_key",
    "web_app_send_prepared_message",
    "web_app_set_background_color",
    "web_app_set_bottom_bar_color",
    "web_app_set_emoji_status",
    "web_app_set_header_color",
    "web_app_setup_back_button",
    "web_app_setup_closing_behavior",
    "web_app_setup_main_button",
    "web_app_setup_secondary_button",
    "web_app_setup_settings_button",
    "web_app_setup_swipe_behavior",
    "web_app_share_to_story",
    "web_app_start_accelerometer",
    "web_app_start_device_orientation",
    "web_app_start_gyroscope",
    "web_app_stop_accelerometer",
    "web_app_stop_device_orientation",
    "web_app_stop_gyroscope",
    "web_app_switch_inline_query",
    "web_app_toggle_orientation_lock",
    "web_app_trigger_haptic_feedback",
    "web_app_verify_age",
    "unknown_cmd",
];

const KEYS: &[&str] = &[
    "access_granted",
    "access_requested",
    "age",
    "allow_vertical_swipe",
    "available",
    "biometry_type",
    "bottom",
    "button",
    "button_type",
    "buttons",
    "chat_types",
    "color",
    "color_key",
    "custom_emoji_id",
    "data",
    "device_id",
    "duration",
    "file_name",
    "force_request",
    "has_shine_effect",
    "height",
    "icon_custom_emoji_id",
    "id",
    "is_active",
    "is_expanded",
    "is_progress_visible",
    "is_state_stable",
    "is_visible",
    "key",
    "left",
    "locked",
    "media_url",
    "message",
    "method",
    "name",
    "need_absolute",
    "need_confirmation",
    "params",
    "passed",
    "path_full",
    "position",
    "query",
    "reason",
    "refresh_rate",
    "req_id",
    "return_back",
    "right",
    "slug",
    "text",
    "text_color",
    "theme_params",
    "title",
    "token",
    "top",
    "try_browser",
    "try_instant_view",
    "type",
    "url",
    "value",
    "widget_link",
    "impact_style",
    "notification_type",
    "slug",
    "text_color",
];

const STRINGS: &[&str] = &[
    "",
    "#fff",
    "#ffffff",
    "#ffffff80",
    "#\u{e9}a",
    "bg_color",
    "secondary_bg_color",
    "default",
    "ok",
    "cancel",
    "destructive",
    "https://example.com/a",
    "tg://resolve?domain=a",
    "/a?b",
    "//x",
    "a/../b",
    "file.txt",
    "..\\x.exe",
    "con",
    "\u{202e}gpj.exe",
    "\u{e9}\u{e9}\u{e9}",
    "light",
    "heavy",
    "left",
    "right",
    "top",
    "bottom",
    "users",
    "bots",
    "groups",
    "channels",
];

fn value(fuzz: &mut Fuzz, depth: usize) -> Value {
    match fuzz.rng().below(if depth > 2 { 4 } else { 6 }) {
        0 => Value::String(fuzz.rng().pick(STRINGS).to_string()),
        1 => Value::String(fuzz.text(&[], 6)),
        2 => json!(fuzz.rng().int(100_000)),
        3 => fuzz
            .rng()
            .pick(&[
                json!(true),
                json!(false),
                Value::Null,
                json!(1.5e300),
                json!(-0.5),
            ])
            .clone(),
        4 => Value::Array(
            (0..fuzz.rng().below(5))
                .map(|_| value(fuzz, depth + 1))
                .collect(),
        ),
        _ => Value::Object(object(fuzz, depth + 1)),
    }
}

fn object(fuzz: &mut Fuzz, depth: usize) -> Map<String, Value> {
    (0..fuzz.rng().below(6))
        .map(|_| (fuzz.rng().pick(KEYS).to_string(), value(fuzz, depth)))
        .collect()
}

#[test]
fn mini_app_bridge_messages_do_not_panic() {
    let mut fuzz = Fuzz::new(0x3EB0_0001, ITERATIONS);
    let palette = Palette::default();
    let origin = "https://app.example";
    for i in 0..fuzz.iterations() {
        let command = fuzz.rng().pick(COMMANDS).to_string();
        let arguments = object(&mut fuzz, 0);
        let event_data = if fuzz.rng().one_in(3) {
            Value::String(Value::Object(arguments.clone()).to_string())
        } else {
            Value::Object(arguments.clone())
        };
        let text = if fuzz.rng().one_in(6) {
            fuzz.text(
                &["{", "}", "\"eventType\"", ":", "\"eventData\"", ",", "["],
                12,
            )
        } else {
            json!([command, event_data]).to_string()
        };
        let messages: Vec<WebMessage> = (0..1 + fuzz.rng().below(3))
            .map(|_| WebMessage {
                text: text.clone(),
                source_url: format!("{origin}/index.html"),
            })
            .collect();
        fuzz.check("mini app messages", i, &text, || {
            let _ = crate::bridge::decode_mini_app_event(&command, &arguments);
            let _ = crate::bridge::parse_native_message(&text, origin);
            let _ = crate::bridge::parse_mini_app_event(&text, origin);
            let _ = crate::bridge::parse_web_proxy_bridge_message(&messages[0]);
            let mut app = MiniAppController::new(MiniAppPanelConfig {
                initial_url: format!("{origin}/index.html"),
                ..MiniAppPanelConfig::default()
            });
            for message in &messages {
                let _ = app.handle_web_message(message, &palette, Instant::now());
            }
        });
    }
    fuzz.finish();
}

#[test]
fn urls_paths_ranges_and_colors_do_not_panic() {
    const TOKENS: &[&str] = &[
        "https://",
        "http://",
        "tg://",
        "t.me/",
        "/",
        "//",
        "?",
        "#",
        "=",
        "&",
        "bytes=",
        "-",
        ",",
        "0",
        "9223372036854775807",
        "18446744073709551616",
        "-1",
        ":",
        "@",
        "[::1]",
        "%",
        "#fff",
        "#12345678",
    ];
    let mut fuzz = Fuzz::new(0x3EB0_0002, ITERATIONS * 5);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(TOKENS, 10);
        fuzz.check("strings", i, &s, || {
            let _ = crate::bridge::origin_from_url(&s);
            let _ = crate::bridge::parse_hex_color(&s);
            let _ = crate::data_stream::parse_range_header(&s);
            let _ = crate::data_stream::mime_from_path(&s);
            let _ = crate::embed::is_allowed_restricted_url(&s, "https://app.example");
            let _ = crate::mini_app::telegram_link_from_path(&s);
        });
    }
    fuzz.finish();
}
