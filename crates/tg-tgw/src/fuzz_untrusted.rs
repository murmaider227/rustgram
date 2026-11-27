//! Panic fuzz over frames and JSON from the sandboxed widget renderer, stored widget data and
//! widget links (TGB-3065).

use tg_fuzz::Fuzz;

const ITERATIONS: usize = 3_000;

const TOKENS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    ":",
    ",",
    "\"",
    "\"type\"",
    "\"ready\"",
    "\"size\"",
    "\"height\"",
    "\"ops\"",
    "\"count\"",
    "\"open_link\"",
    "\"url\"",
    "\"copy_text\"",
    "\"text\"",
    "\"bot_query\"",
    "\"storage_commit\"",
    "\"key\"",
    "\"value\"",
    "-1",
    "1e400",
    "NaN",
    "null",
    "data:",
    "data:image/png;base64,",
    "iVBORw0K",
    ";",
    "base64",
    ",",
    "%",
    "https://",
    "tg://",
    "pointer",
    "url(",
    ")",
    "auto",
    "\u{e9}",
];

#[test]
fn renderer_frames_and_json_do_not_panic() {
    let mut fuzz = Fuzz::new(0x76A0_0001, ITERATIONS);
    let mut canvases = crate::canvas::CanvasStore::new();
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 20);
        let frame = if fuzz.rng().one_in(2) {
            fuzz.mutate(text.as_bytes())
        } else {
            fuzz.bytes(80)
        };
        let rect = (
            fuzz.rng().int(9000) as i32,
            fuzz.rng().int(9000) as i32,
            fuzz.rng().int(9000) as u32,
            fuzz.rng().int(9000) as u32,
        );
        fuzz.check("widget renderer input", i, &(&text, &frame, rect), || {
            let _ = crate::wire::decode_press(&frame);
            let _ = crate::wire::ControlMessage::decode(&frame);
            let _ = crate::wire::decode_host_message(&text);
            let _ = crate::wire::decode_renderer_message(&text);
            let _ = crate::storage::decode_storage_record(&frame);
            let _ = crate::model::unpack_payload(&frame);
            let _ = crate::backend::parse_data_url(&text);
            let _ = crate::bridge::is_passport_or_oauth_link(&text);
            let _ = crate::cursor::CursorKind::from_css(&text);
            let _ = crate::sandbox::is_allowed_widget_navigation(&text);
            let _ = canvases.run(1, rect.2, rect.3, &[]);
            let _ = canvases.read(1, rect.0, rect.1, rect.2, rect.3);
            let mut machine = crate::state_machine::WidgetStateMachine::new(&frame);
            machine.set_size(rect.0, f64::from(rect.1));
            let _ = machine.update_html(text.as_bytes());
        });
    }
    fuzz.finish();
}
