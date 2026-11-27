//! Panic regression and fuzz tests over untrusted-input entry points (TGB-3065).

#[test]
fn hex_color_with_multibyte_char_does_not_panic() {
    // Old code sliced `s[0..1]` on a 3-byte string such as `\u{e9}a` (stats graph JSON colors).
    assert_eq!(crate::color::Color::from_hex("#\u{e9}a"), None);
    assert_eq!(crate::color::Color::from_hex("#a\u{e9}bcd"), None);
}

use serde_json::{Value, json};
use tg_fuzz::Fuzz;

use crate::chart::StatisticalChart;
use crate::filter::LinesFilterController;
use crate::format::{CreditsAmount, CreditsType};
use crate::render::{RenderChartType, RenderOptions};

const ITERATIONS: usize = 400;

fn number(fuzz: &mut Fuzz) -> Value {
    match fuzz.rng().below(6) {
        0 => json!(fuzz.rng().int(1 << 40)),
        1 => json!(-(fuzz.rng().below(1000) as i64)),
        2 => json!(fuzz.rng().below(1000) as f64 / 7.0),
        3 => json!(1.5e300),
        4 => json!("12"),
        _ => json!(1_600_000_000_000i64 + 86_400_000 * fuzz.rng().below(400) as i64),
    }
}

fn chart_json(fuzz: &mut Fuzz) -> String {
    let lines = fuzz.rng().below(5);
    let len = fuzz.rng().below(12);
    let mut columns = Vec::new();
    let mut x = vec![json!("x")];
    let mut t = 1_600_000_000_000i64;
    for _ in 0..len {
        t += if fuzz.rng().one_in(8) {
            -86_400_000
        } else {
            86_400_000
        };
        x.push(if fuzz.rng().one_in(10) {
            number(fuzz)
        } else {
            json!(t)
        });
    }
    if !fuzz.rng().one_in(10) {
        columns.push(Value::Array(x));
    }
    let mut types = serde_json::Map::new();
    let mut colors = serde_json::Map::new();
    let mut names = serde_json::Map::new();
    types.insert("x".to_string(), json!("x"));
    for l in 0..lines {
        let id = format!("y{l}");
        let mut column = vec![json!(id)];
        let column_len = if fuzz.rng().one_in(4) {
            fuzz.rng().below(14)
        } else {
            len
        };
        for _ in 0..column_len {
            column.push(number(fuzz));
        }
        columns.push(Value::Array(column));
        types.insert(
            id.clone(),
            json!(
                *fuzz
                    .rng()
                    .pick(&["line", "bar", "area", "step", "x", "pie", "?"])
            ),
        );
        colors.insert(
            id.clone(),
            json!(
                *fuzz
                    .rng()
                    .pick(&["#ff0000", "RED#00ff00", "#", "#\u{e9}a", "", "#12345"])
            ),
        );
        names.insert(id, json!(fuzz.text(&[], 3)));
    }
    let mut root = json!({ "columns": columns, "types": types, "colors": colors, "names": names });
    for flag in ["y_scaled", "percentage", "stacked"] {
        if fuzz.rng().one_in(3) {
            root[flag] = json!(true);
        }
    }
    if fuzz.rng().one_in(4) {
        root["subchart"] = json!({ "show": true, "defaultZoom": [number(fuzz), number(fuzz)] });
    }
    root.to_string()
}

#[test]
fn server_stats_graphs_do_not_panic() {
    let mut fuzz = Fuzz::new(0x5747_0001, ITERATIONS);
    let types = [
        RenderChartType::Linear,
        RenderChartType::DoubleLinear,
        RenderChartType::Bar,
        RenderChartType::StackBar,
        RenderChartType::StackLinear,
    ];
    for i in 0..fuzz.iterations() {
        let json = chart_json(&mut fuzz);
        let v = fuzz.rng().int(2_000_000_000_000) as f64;
        let index = fuzz.rng().int(20) as usize;
        let chart_type = *fuzz.rng().pick(&types);
        let selected = fuzz.rng().one_in(2).then(|| fuzz.rng().below(14));
        fuzz.check("stats chart", i, &json, || {
            let Ok(mut chart) = StatisticalChart::from_json_str(&json) else {
                return;
            };
            chart.measure();
            let _ = chart.get_day_string(index);
            let start = chart.find_start_index(v);
            let _ = chart.find_end_index(start, v);
            let _ = chart.default_zoom_x_percentage();
            let _ = crate::xlsx::serialize_xlsx(&[crate::xlsx::sheet_from_chart("t", &chart)]);
            let filter = LinesFilterController::new();
            let options = RenderOptions {
                width: 64,
                height: 48,
                chart_type,
                selected_x_index: selected,
                ..RenderOptions::default()
            };
            let _ = crate::render::render_chart(&chart, &filter, &options);
        });
    }
    fuzz.finish();
}

#[test]
fn stats_amount_formatting_does_not_panic() {
    let mut fuzz = Fuzz::new(0x5747_0002, ITERATIONS * 50);
    for i in 0..fuzz.iterations() {
        let a = fuzz.rng().int(1 << 40);
        let b = fuzz.rng().int(1 << 40);
        let rate = match fuzz.rng().below(3) {
            0 => *fuzz
                .rng()
                .pick(&[f64::NAN, f64::INFINITY, -f64::INFINITY, 1e300, -1e300]),
            _ => fuzz.rng().int(1000) as f64 / 3.0,
        };
        fuzz.check("amounts", i, &(a, b, rate), || {
            let amount = CreditsAmount::new(a, b, CreditsType::Ton);
            let _ = amount.multiplied(rate);
            let _ = CreditsAmount::from_nanoton(a);
            let _ = crate::format::format_credits_amount_decimal(amount);
            let _ = crate::format::format_count_to_short(a, false);
            let _ = crate::format::format_count_decimal(a);
            let _ = crate::format::format_exact_count_decimal(rate);
        });
    }
    fuzz.finish();
}

#[test]
fn out_of_order_timestamps_do_not_allocate_a_label_per_millisecond() {
    // Old code built one label per time step from the first to the last x value: x[1] < x[0]
    // makes the step 1 ms, so this chart got ~1e6 labels (a hostile span reaches tens of GB).
    let json = r#"{"columns":[["x",1600000001000,1600000000000,1600001000000],["y0",1,2,3]],
        "types":{"x":"x","y0":"line"}}"#;
    let chart = StatisticalChart::from_json_str(json).expect("valid chart");
    assert!(
        chart.days_lookup.len() <= 1 << 16,
        "{} labels",
        chart.days_lookup.len()
    );
}

#[test]
fn huge_server_values_saturate_in_rulers_and_amounts() {
    // Old code overflowed `max - min`, `min + value` and `i * step` on huge y values, and
    // negated `nano` (overflow on i64::MIN) when normalizing amounts.
    let _ = crate::rulers::ChartRulersData::new(i64::MAX, -5, true, 0.0, None, None);
    let _ = crate::rulers::ChartRulersData::new(i64::MAX, 0, false, 0.0, None, None);
    let amount = CreditsAmount::new(0, i64::MIN, CreditsType::Ton);
    assert!((0..1_000_000_000).contains(&amount.nano()));
}
