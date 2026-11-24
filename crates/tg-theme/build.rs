//! Build-time code generator for `resources/colors.palette`.
//!
//! Port of `Telegram/codegen/codegen/style/parsed_file.cpp` and `generator.cpp`
//! (palette mode).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

const PALETTE_REL_PATH: &str = "../../resources/colors.palette";

#[derive(Debug, Clone)]
struct PaletteEntry {
    camel_name: String,
    snake_name: String,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    /// `Some(idx)` (0-based in `Palette`, i.e. >= 2) if this entry either copies
    /// another color or declares an explicit `| fallback` color.
    fallback_index: Option<usize>,
    /// Original value string for `PaletteRow` (`#rrggbb[aa]` or copied color name).
    row_value: String,
    /// Explicit fallback name (non-empty only for `#rrggbb | fallback` entries).
    row_fallback: String,
    /// Trimmed single-line comment description.
    description: String,
}

fn crc32(data: &[u8]) -> i32 {
    const POLY: u32 = 0x04c1_1db7;
    let reflect = |mut val: u32, bits: u32| -> u32 {
        let mut res = 0u32;
        for i in 1..=bits {
            if (val & 1) != 0 {
                res |= 1u32 << (bits - i);
            }
            val >>= 1;
        }
        res
    };

    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut entry = reflect(i as u32, 8) << 24;
        for _ in 0..8 {
            entry = (entry << 1) ^ if (entry & (1u32 << 31)) != 0 { POLY } else { 0 };
        }
        *slot = reflect(entry, 32);
    }

    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc = (crc >> 8) ^ table[((crc & 0xff) as usize) ^ (byte as usize)];
    }
    (crc ^ 0xffff_ffffu32) as i32
}

fn camel_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    let chars: Vec<char> = name.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                let prev = chars[i - 1];
                let next_is_lower = chars.get(i + 1).is_some_and(|c| c.is_ascii_lowercase());
                if prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_is_lower)
                {
                    out.push('_');
                }
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_hex_color(hex: &str) -> Result<(u8, u8, u8, u8), String> {
    let bytes = hex.as_bytes();
    if bytes.len() != 6 && bytes.len() != 8 {
        return Err(format!("invalid hex color length: #{hex}"));
    }
    let digit = |b: u8| -> Result<u8, String> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(format!("invalid hex digit '{}' in #{hex}", b as char)),
        }
    };
    let pair =
        |i: usize| -> Result<u8, String> { Ok((digit(bytes[i])? << 4) | digit(bytes[i + 1])?) };
    let r = pair(0)?;
    let g = pair(2)?;
    let b = pair(4)?;
    let a = if bytes.len() == 8 { pair(6)? } else { 255 };
    Ok((r, g, b, a))
}

fn format_hex_value(r: u8, g: u8, b: u8, a: u8) -> String {
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

fn parse_palette_file(src: &str) -> Result<Vec<PaletteEntry>, String> {
    let mut entries: Vec<PaletteEntry> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut snake_seen: HashSet<String> = HashSet::new();
    snake_seen.insert("transparent".to_string());
    snake_seen.insert("white".to_string());

    let keywords: HashSet<&str> = [
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
        "gen",
    ]
    .into_iter()
    .collect();

    for (line_idx, raw_line) in src.lines().enumerate() {
        let line_no = line_idx + 1;
        let (code_part, description) = match raw_line.find("//") {
            Some(pos) => (&raw_line[..pos], raw_line[pos + 2..].trim().to_string()),
            None => (raw_line, String::new()),
        };
        let trimmed = code_part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let body = trimmed
            .strip_suffix(';')
            .ok_or_else(|| format!("line {line_no}: missing trailing ';': {raw_line}"))?
            .trim();
        let (name_raw, value_raw) = body
            .split_once(':')
            .ok_or_else(|| format!("line {line_no}: missing ':' separator: {raw_line}"))?;
        let camel_name = name_raw.trim().to_string();
        if camel_name.is_empty()
            || !camel_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("line {line_no}: invalid color name '{camel_name}'"));
        }
        if by_name.contains_key(&camel_name) {
            return Err(format!("line {line_no}: duplicate color '{camel_name}'"));
        }
        let snake_name = camel_to_snake(&camel_name);
        if keywords.contains(snake_name.as_str()) {
            return Err(format!(
                "line {line_no}: snake_case name '{snake_name}' collides with Rust keyword"
            ));
        }
        if !snake_seen.insert(snake_name.clone()) {
            return Err(format!(
                "line {line_no}: duplicate snake_case name '{snake_name}' (from '{camel_name}')"
            ));
        }

        let value_str = value_raw.trim();
        let (r, g, b, a, fallback_index, row_value, row_fallback) =
            if let Some(rest) = value_str.strip_prefix('#') {
                let (hex_part, explicit_fallback) = match rest.split_once('|') {
                    Some((h, fb)) => (h.trim(), Some(fb.trim())),
                    None => (rest.trim(), None),
                };
                let (r, g, b, a) = parse_hex_color(hex_part)
                    .map_err(|e| format!("line {line_no} ({camel_name}): {e}"))?;
                let (fb_idx, row_fb) = match explicit_fallback {
                    Some(fb_name) => {
                        let &idx = by_name.get(fb_name).ok_or_else(|| {
                            format!("line {line_no} ({camel_name}): unknown fallback '{fb_name}'")
                        })?;
                        (Some(idx), fb_name.to_string())
                    }
                    None => (None, String::new()),
                };
                (r, g, b, a, fb_idx, format_hex_value(r, g, b, a), row_fb)
            } else {
                let copy_name = value_str;
                let &target_palette_idx = by_name.get(copy_name).ok_or_else(|| {
                    format!("line {line_no} ({camel_name}): unknown color reference '{copy_name}'")
                })?;
                let target_entry = &entries[target_palette_idx - 2];
                (
                    target_entry.r,
                    target_entry.g,
                    target_entry.b,
                    target_entry.a,
                    Some(target_palette_idx),
                    copy_name.to_string(),
                    String::new(),
                )
            };

        let palette_index = entries.len() + 2;
        by_name.insert(camel_name.clone(), palette_index);
        entries.push(PaletteEntry {
            camel_name,
            snake_name,
            r,
            g,
            b,
            a,
            fallback_index,
            row_value,
            row_fallback,
            description,
        });
    }

    Ok(entries)
}

fn compute_checksum(entries: &[PaletteEntry]) -> i32 {
    let mut s = String::with_capacity(entries.len() * 40 + 64);
    s.push_str("&transparent:{ 255, 255, 255, 0 }");
    s.push_str("&white:{ 255, 255, 255, 255 }");
    for e in entries {
        let _ = write!(
            s,
            "&{}:{{ {}, {}, {}, {} }}",
            e.camel_name, e.r, e.g, e.b, e.a
        );
    }
    crc32(s.as_bytes())
}

fn escape_rust_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn generate_code(entries: &[PaletteEntry]) -> String {
    let total_count = entries.len() + 2;
    let checksum = compute_checksum(entries);
    let dialogs_menu_icon_fg_idx = entries
        .iter()
        .position(|e| e.camel_name == "dialogsMenuIconFg")
        .expect("dialogsMenuIconFg missing from colors.palette")
        + 2;
    let dialogs_bg_idx = entries
        .iter()
        .position(|e| e.camel_name == "dialogsBg")
        .expect("dialogsBg missing from colors.palette")
        + 2;
    let window_bold_fg_idx = entries
        .iter()
        .position(|e| e.camel_name == "windowBoldFg")
        .expect("windowBoldFg missing from colors.palette")
        + 2;
    let window_bg_idx = entries
        .iter()
        .position(|e| e.camel_name == "windowBg")
        .expect("windowBg missing from colors.palette")
        + 2;

    let mut out = String::with_capacity(256 * 1024);
    let _ = writeln!(
        out,
        "// @generated by build.rs from resources/colors.palette"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "/// Total number of palette color slots (including `transparent` and `white`)."
    );
    let _ = writeln!(out, "pub const PALETTE_COUNT: usize = {total_count};");
    let _ = writeln!(
        out,
        "/// Number of named color entries declared in `colors.palette`."
    );
    let _ = writeln!(
        out,
        "pub const PALETTE_ENTRY_COUNT: usize = {};",
        entries.len()
    );
    let _ = writeln!(
        out,
        "/// CRC32 checksum of the palette schema, matching tdesktop `style::palette::Checksum()`."
    );
    let _ = writeln!(out, "pub const PALETTE_CHECKSUM: i32 = {checksum};");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "/// Generated color palette containing every color from `colors.palette`."
    );
    let _ = writeln!(out, "#[derive(Debug, Clone)]");
    let _ = writeln!(out, "pub struct Palette {{");
    let _ = writeln!(out, "    /// Special transparent color (index 0).");
    let _ = writeln!(out, "    pub transparent: Color,");
    let _ = writeln!(out, "    /// Special white color (index 1).");
    let _ = writeln!(out, "    pub white: Color,");
    for (idx, e) in entries.iter().enumerate() {
        let palette_idx = idx + 2;
        if e.description.is_empty() {
            let _ = writeln!(out, "    /// `{}` (index {palette_idx}).", e.camel_name);
        } else {
            let doc = e.description.replace('\n', " ");
            let _ = writeln!(
                out,
                "    /// `{}` (index {palette_idx}): {doc}",
                e.camel_name
            );
        }
        let _ = writeln!(out, "    pub {}: Color,", e.snake_name);
    }
    let _ = writeln!(out, "    status: [SlotStatus; PALETTE_COUNT],");
    let _ = writeln!(out, "    ready: bool,");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);

    // PartialEq / Eq comparing only color values
    let _ = writeln!(out, "impl PartialEq for Palette {{");
    let _ = writeln!(out, "    fn eq(&self, other: &Self) -> bool {{");
    let _ = writeln!(
        out,
        "        self.transparent == other.transparent && self.white == other.white"
    );
    for e in entries {
        let _ = writeln!(out, "            && self.{0} == other.{0}", e.snake_name);
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out, "impl Eq for Palette {{}}");
    let _ = writeln!(out);

    // Static ROWS table
    let _ = writeln!(
        out,
        "/// Metadata rows for every color entry in `colors.palette` (matching `style::main_palette::data()`)."
    );
    let _ = writeln!(out, "pub const PALETTE_ROWS: &[PaletteRow] = &[");
    for (idx, e) in entries.iter().enumerate() {
        let palette_idx = idx + 2;
        let fb_opt = match e.fallback_index {
            Some(i) => format!("Some({i})"),
            None => "None".to_string(),
        };
        let _ = writeln!(
            out,
            "    PaletteRow {{ index: {palette_idx}, name: {}, value: {}, fallback: {}, fallback_index: {fb_opt}, default_color: Color::rgba({}, {}, {}, {}), description: {} }},",
            escape_rust_str(&e.camel_name),
            escape_rust_str(&e.row_value),
            escape_rust_str(&e.row_fallback),
            e.r,
            e.g,
            e.b,
            e.a,
            escape_rust_str(&e.description),
        );
    }
    let _ = writeln!(out, "];");
    let _ = writeln!(out);

    // Static default specs for all slots 0..PALETTE_COUNT
    let _ = writeln!(
        out,
        "pub(crate) const PALETTE_SPECS: [(Option<usize>, Color); PALETTE_COUNT] = ["
    );
    let _ = writeln!(out, "    (None, Color::rgba(255, 255, 255, 0)),");
    let _ = writeln!(out, "    (None, Color::rgba(255, 255, 255, 255)),");
    for e in entries {
        let fb_opt = match e.fallback_index {
            Some(i) => format!("Some({i})"),
            None => "None".to_string(),
        };
        let _ = writeln!(
            out,
            "    ({fb_opt}, Color::rgba({}, {}, {}, {})),",
            e.r, e.g, e.b, e.a
        );
    }
    let _ = writeln!(out, "];");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "pub(crate) const CONTRAST_PAIRS: [(usize, usize); 2] = [({dialogs_menu_icon_fg_idx}, {dialogs_bg_idx}), ({window_bold_fg_idx}, {window_bg_idx})];"
    );
    let _ = writeln!(out);

    // Helper methods on Palette
    let _ = writeln!(out, "impl Palette {{");
    let _ = writeln!(
        out,
        "    /// Creates an uninitialized palette whose slots are all `Initial`."
    );
    let _ = writeln!(
        out,
        "    /// Call `set_color` / `set_color_from` and then `finalize` or `finalize_with`."
    );
    let _ = writeln!(out, "    pub fn new_uninitialized() -> Self {{");
    let _ = writeln!(out, "        Self {{");
    let _ = writeln!(
        out,
        "            transparent: Color::rgba(255, 255, 255, 0),"
    );
    let _ = writeln!(out, "            white: Color::rgba(255, 255, 255, 255),");
    for e in entries {
        let _ = writeln!(
            out,
            "            {}: Color::rgba(0, 0, 0, 0),",
            e.snake_name
        );
    }
    let _ = writeln!(
        out,
        "            status: [SlotStatus::Initial; PALETTE_COUNT],"
    );
    let _ = writeln!(out, "            ready: false,");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    // index_of_name (matches GetPaletteIndex: only colors.palette entries 2..COUNT)
    let _ = writeln!(
        out,
        "    /// Looks up the palette index of an original camelCase color name."
    );
    let _ = writeln!(out, "    ///");
    let _ = writeln!(
        out,
        "    /// Matches tdesktop's `GetPaletteIndex`: covers the {} entries from `colors.palette`",
        entries.len()
    );
    let _ = writeln!(
        out,
        "    /// (indices `2..{total_count}`). Special slots `transparent` (0) and `white` (1) are"
    );
    let _ = writeln!(out, "    /// intentionally not overridable by name.");
    let _ = writeln!(
        out,
        "    pub fn index_of_name(name: &str) -> Option<usize> {{"
    );
    let _ = writeln!(out, "        match name {{");
    for (idx, e) in entries.iter().enumerate() {
        let palette_idx = idx + 2;
        let _ = writeln!(
            out,
            "            {} => Some({palette_idx}),",
            escape_rust_str(&e.camel_name)
        );
    }
    let _ = writeln!(out, "            _ => None,");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    // name_at_index
    let _ = writeln!(
        out,
        "    /// Returns the original camelCase name for a palette index (`0..PALETTE_COUNT`)."
    );
    let _ = writeln!(
        out,
        "    pub fn name_at_index(index: usize) -> Option<&'static str> {{"
    );
    let _ = writeln!(out, "        match index {{");
    let _ = writeln!(out, "            0 => Some(\"transparent\"),");
    let _ = writeln!(out, "            1 => Some(\"white\"),");
    let _ = writeln!(
        out,
        "            i => PALETTE_ROWS.get(i - 2).map(|r| r.name),"
    );
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    // raw_color_at / raw_color_at_mut
    let _ = writeln!(
        out,
        "    pub(crate) fn raw_color_at(&self, index: usize) -> Option<Color> {{"
    );
    let _ = writeln!(out, "        match index {{");
    let _ = writeln!(out, "            0 => Some(self.transparent),");
    let _ = writeln!(out, "            1 => Some(self.white),");
    for (idx, e) in entries.iter().enumerate() {
        let palette_idx = idx + 2;
        let _ = writeln!(
            out,
            "            {palette_idx} => Some(self.{}),",
            e.snake_name
        );
    }
    let _ = writeln!(out, "            _ => None,");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "    pub(crate) fn raw_color_at_mut(&mut self, index: usize) -> Option<&mut Color> {{"
    );
    let _ = writeln!(out, "        match index {{");
    let _ = writeln!(out, "            0 => Some(&mut self.transparent),");
    let _ = writeln!(out, "            1 => Some(&mut self.white),");
    for (idx, e) in entries.iter().enumerate() {
        let palette_idx = idx + 2;
        let _ = writeln!(
            out,
            "            {palette_idx} => Some(&mut self.{}),",
            e.snake_name
        );
    }
    let _ = writeln!(out, "            _ => None,");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");

    out
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let palette_path = manifest_dir.join(PALETTE_REL_PATH);
    println!("cargo:rerun-if-changed={}", palette_path.display());

    let src = fs::read_to_string(&palette_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", palette_path.display()));
    let entries = parse_palette_file(&src)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", palette_path.display()));

    let generated = generate_code(&entries);
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("palette_generated.rs");
    fs::write(&out_path, generated)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));
}
