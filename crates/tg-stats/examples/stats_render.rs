//! CLI example rendering a Telegram statistics chart JSON file to a PNG image.
//!
//! Usage:
//! `cargo run -p tg-stats --example stats_render -- <in.json> <out.png> [--type linear|double_linear|bar|stack_bar|stack_linear|pie] [--width 640] [--height 360] [--selected N] [--title "..."] [--dark]`

use std::env;
use std::fs;
use std::process;

use tg_stats::{
    ChartPalette, LinesFilterController, RenderChartType, RenderOptions, StatisticalChart,
    render_chart,
};

fn parse_type(raw: &str) -> Option<RenderChartType> {
    match raw.to_ascii_lowercase().as_str() {
        "linear" => Some(RenderChartType::Linear),
        "double_linear" | "double-linear" | "double" => Some(RenderChartType::DoubleLinear),
        "bar" => Some(RenderChartType::Bar),
        "stack_bar" | "stack-bar" | "stackbar" => Some(RenderChartType::StackBar),
        "stack_linear" | "stack-linear" | "area" => Some(RenderChartType::StackLinear),
        "pie" => Some(RenderChartType::Pie),
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: render <in.json> <out.png> [--type linear|double_linear|bar|stack_bar|stack_linear|pie] [--width W] [--height H] [--selected IDX] [--title TEXT] [--dark]"
        );
        process::exit(2);
    }

    let input_path = &args[0];
    let output_path = &args[1];

    let mut explicit_type: Option<RenderChartType> = None;
    let mut width: u32 = 640;
    let mut height: u32 = 360;
    let mut selected: Option<usize> = None;
    let mut title: Option<String> = None;
    let mut dark = false;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--type" if i + 1 < args.len() => {
                explicit_type = parse_type(&args[i + 1]);
                if explicit_type.is_none() {
                    eprintln!("Unknown --type value: {}", args[i + 1]);
                    process::exit(2);
                }
                i += 2;
            }
            "--width" if i + 1 < args.len() => {
                width = args[i + 1].parse().unwrap_or(640);
                i += 2;
            }
            "--height" if i + 1 < args.len() => {
                height = args[i + 1].parse().unwrap_or(360);
                i += 2;
            }
            "--selected" if i + 1 < args.len() => {
                selected = args[i + 1].parse().ok();
                i += 2;
            }
            "--title" if i + 1 < args.len() => {
                title = Some(args[i + 1].clone());
                i += 2;
            }
            "--dark" => {
                dark = true;
                i += 1;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                process::exit(2);
            }
        }
    }

    let bytes = match fs::read(input_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read {input_path}: {e}");
            process::exit(1);
        }
    };

    let mut chart = match StatisticalChart::from_json(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse chart JSON: {e}");
            process::exit(1);
        }
    };

    let palette = if dark {
        ChartPalette::dark()
    } else {
        ChartPalette::light()
    };
    chart.apply_palette(&palette);

    let mut filter = LinesFilterController::new();
    for line in &chart.lines {
        if line.is_hidden_on_start {
            filter.set_enabled(line.id, false, 1);
        }
    }
    filter.tick(1.0);

    let chart_type = explicit_type
        .or_else(|| chart.inferred_type.map(RenderChartType::from))
        .unwrap_or(RenderChartType::Linear);

    let options = RenderOptions {
        width,
        height,
        chart_type,
        title: title.or_else(|| Some(format!("{chart_type:?} Chart"))),
        selected_x_index: selected,
        palette,
        ..Default::default()
    };

    let rendered = match render_chart(&chart, &filter, &options) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Render error: {e}");
            process::exit(1);
        }
    };

    if let Err(e) = rendered.save_png(output_path) {
        eprintln!("Failed to save {output_path}: {e}");
        process::exit(1);
    }

    println!(
        "Rendered {chart_type:?} ({}x{}) -> {output_path}",
        rendered.width, rendered.height
    );
}
