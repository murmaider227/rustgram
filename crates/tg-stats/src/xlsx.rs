//! Minimal OpenXML Spreadsheet (`.xlsx`) serializer and statistics sheet builder.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/statistics_xlsx.h`
//! - `Telegram/SourceFiles/statistics/statistics_xlsx.cpp`
//! - `Telegram/SourceFiles/statistics/statistics_sheets.h`
//! - `Telegram/SourceFiles/statistics/statistics_sheets.cpp`

use std::io::{Cursor, Write};

use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::chart::{ONE_DAY_MS, StatisticalChart, StatisticalGraph};
use crate::overview::{
    ChannelStatistics, MessageStatistics, StatisticalValue, StoryStatistics, SupergroupStatistics,
};

/// Excel serial date offset for the Unix epoch (1970-01-01).
pub const EPOCH_SERIAL: f64 = 25_569.0;

/// Milliseconds in a 24-hour day.
pub const MILLISECONDS_IN_DAY: f64 = 86_400_000.0;

const STYLE_GENERAL: u32 = 0;
const STYLE_DATE: u32 = 1;
const STYLE_DATE_TIME: u32 = 2;
const STYLE_HEADER: u32 = 3;
const STYLE_HEADER_RIGHT: u32 = 4;

const MAX_SHEET_NAME_LENGTH: usize = 31;

const COLUMN_PADDING: usize = 2;
const HEADER_PADDING: usize = 3;
const MIN_COLUMN_WIDTH: usize = 8;
const MAX_COLUMN_WIDTH: usize = 48;

const DATE_WIDTH: usize = 10;
const DATE_TIME_WIDTH: usize = 16;

const VALUE_DIGITS: usize = 6;
const SERIAL_DIGITS: usize = 10;

/// Errors returned when serializing an XLSX workbook.
#[derive(Debug, Error)]
pub enum XlsxError {
    /// No worksheets were provided.
    #[error("cannot serialize an XLSX workbook with zero sheets")]
    EmptyWorkbook,
    /// Failed to write a ZIP archive entry.
    #[error("zip archive error: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// Failed to write bytes into the archive buffer.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Discriminant for a spreadsheet [`Cell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellType {
    /// Blank cell (omitted from sheet XML).
    #[default]
    Empty,
    /// Plain UTF-8 string (`inlineStr`).
    Text,
    /// Bold column header (`inlineStr`, right-aligned over numeric columns).
    Header,
    /// Floating-point or integer numeric value.
    Number,
    /// Excel serial date (`numFmtId=14`).
    Date,
    /// Excel serial date-time (`numFmtId=22`).
    DateTime,
}

/// A single cell in an XLSX worksheet (`Statistic::Xlsx::Cell`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    /// Cell value kind.
    pub cell_type: CellType,
    /// Text payload for [`CellType::Text`] and [`CellType::Header`].
    pub text: String,
    /// Numeric payload for [`CellType::Number`], [`CellType::Date`], and [`CellType::DateTime`].
    pub number: f64,
}

impl Cell {
    /// Creates an empty cell.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            cell_type: CellType::Empty,
            text: String::new(),
            number: 0.0,
        }
    }

    /// Creates a plain text cell.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            cell_type: CellType::Text,
            text: text.into(),
            number: 0.0,
        }
    }

    /// Creates a header text cell.
    #[must_use]
    pub fn header(text: impl Into<String>) -> Self {
        Self {
            cell_type: CellType::Header,
            text: text.into(),
            number: 0.0,
        }
    }

    /// Creates a numeric cell.
    #[must_use]
    pub const fn number(value: f64) -> Self {
        Self {
            cell_type: CellType::Number,
            text: String::new(),
            number: value,
        }
    }

    /// Creates an Excel serial date cell from Unix epoch milliseconds.
    #[must_use]
    pub fn date(milliseconds: f64) -> Self {
        Self {
            cell_type: CellType::Date,
            text: String::new(),
            number: (milliseconds / MILLISECONDS_IN_DAY) + EPOCH_SERIAL,
        }
    }

    /// Creates an Excel serial date-time cell from Unix epoch milliseconds.
    #[must_use]
    pub fn date_time(milliseconds: f64) -> Self {
        Self {
            cell_type: CellType::DateTime,
            text: String::new(),
            number: (milliseconds / MILLISECONDS_IN_DAY) + EPOCH_SERIAL,
        }
    }
}

/// A single worksheet containing a title and rows of cells (`Statistic::Xlsx::Sheet`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sheet {
    /// Requested worksheet name (sanitized and deduplicated during serialization).
    pub name: String,
    /// Row-major grid of cells.
    pub rows: Vec<Vec<Cell>>,
}

impl Sheet {
    /// Creates a worksheet with the given name and rows.
    #[must_use]
    pub fn new(name: impl Into<String>, rows: Vec<Vec<Cell>>) -> Self {
        Self {
            name: name.into(),
            rows,
        }
    }
}

/// Tagged union of Telegram statistics objects exportable to XLSX (`Data::AnyStatistics`).
#[derive(Debug, Clone)]
pub enum AnyStatistics {
    /// Broadcast channel statistics.
    Channel(Box<ChannelStatistics>),
    /// Supergroup / megagroup statistics.
    Supergroup(Box<SupergroupStatistics>),
    /// Single message statistics.
    Message(Box<MessageStatistics>),
    /// Single story statistics.
    Story(Box<StoryStatistics>),
}

/// Converts a 0-based column index into an Excel column reference (`0 -> "A"`, `26 -> "AA"`).
#[must_use]
pub fn column_name(index: usize) -> String {
    let mut chars = Vec::new();
    let mut i = index + 1;
    while i > 0 {
        let rem = (i - 1) % 26;
        chars.push((b'A' + rem as u8) as char);
        i = (i - 1) / 26;
    }
    chars.into_iter().rev().collect()
}

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            c if (c as u32) >= 0x20 => out.push(c),
            _ => {}
        }
    }
    out
}

fn number_text(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let rounded = value.round();
    if (value - rounded).abs() == 0.0 && value.abs() < 1e15 {
        format!("{}", rounded as i64)
    } else {
        format!("{value:.digits$}")
    }
}

fn sanitize_sheet_name(name: &str, index: usize, limit: usize) -> String {
    const FORBIDDEN: &str = ":\\/?*[]";
    let filtered: String = name
        .chars()
        .filter(|ch| !FORBIDDEN.contains(*ch))
        .collect::<String>();
    let trimmed = filtered.trim();
    let truncated: String = trimmed.chars().take(limit).collect();
    let final_name = truncated.trim();
    if final_name.is_empty() {
        format!("Sheet{}", index + 1)
    } else {
        final_name.to_owned()
    }
}

fn unique_sheet_names(sheets: &[Sheet]) -> Vec<String> {
    let mut result = Vec::with_capacity(sheets.len());
    for (index, sheet) in sheets.iter().enumerate() {
        let mut candidate = sanitize_sheet_name(&sheet.name, index, MAX_SHEET_NAME_LENGTH);
        let mut attempt = 2usize;
        while result.iter().any(|existing| existing == &candidate) {
            let suffix = format!(" ({attempt})");
            let max_prefix = MAX_SHEET_NAME_LENGTH.saturating_sub(suffix.chars().count());
            candidate = format!(
                "{}{suffix}",
                sanitize_sheet_name(&sheet.name, index, max_prefix)
            );
            attempt += 1;
        }
        result.push(candidate);
    }
    result
}

fn numeric_columns(sheet: &Sheet) -> Vec<bool> {
    let mut is_numeric = Vec::new();
    let mut has_data = Vec::new();
    for row in sheet.rows.iter().skip(1) {
        if is_numeric.len() < row.len() {
            is_numeric.resize(row.len(), true);
            has_data.resize(row.len(), false);
        }
        for (j, cell) in row.iter().enumerate() {
            match cell.cell_type {
                CellType::Empty => {}
                CellType::Number | CellType::Date | CellType::DateTime => {
                    has_data[j] = true;
                }
                CellType::Text | CellType::Header => {
                    is_numeric[j] = false;
                }
            }
        }
    }
    for (flag, filled) in is_numeric.iter_mut().zip(has_data.iter()) {
        *flag = *flag && *filled;
    }
    is_numeric
}

fn append_cell_xml(out: &mut String, cell: &Cell, cell_ref: &str, numeric_column: bool) {
    let write_open = |buf: &mut String, style: u32, cell_type: &str| {
        buf.push_str("<c r=\"");
        buf.push_str(cell_ref);
        buf.push('"');
        if style != STYLE_GENERAL {
            buf.push_str(" s=\"");
            buf.push_str(&style.to_string());
            buf.push('"');
        }
        if !cell_type.is_empty() {
            buf.push_str(" t=\"");
            buf.push_str(cell_type);
            buf.push('"');
        }
        buf.push('>');
    };

    match cell.cell_type {
        CellType::Empty => {}
        CellType::Text => {
            write_open(out, STYLE_GENERAL, "inlineStr");
            out.push_str("<is><t xml:space=\"preserve\">");
            out.push_str(&escape_xml(&cell.text));
            out.push_str("</t></is></c>");
        }
        CellType::Header => {
            let style = if numeric_column {
                STYLE_HEADER_RIGHT
            } else {
                STYLE_HEADER
            };
            write_open(out, style, "inlineStr");
            out.push_str("<is><t xml:space=\"preserve\">");
            out.push_str(&escape_xml(&cell.text));
            out.push_str("</t></is></c>");
        }
        CellType::Number => {
            write_open(out, STYLE_GENERAL, "");
            out.push_str("<v>");
            out.push_str(&number_text(cell.number, VALUE_DIGITS));
            out.push_str("</v></c>");
        }
        CellType::Date => {
            write_open(out, STYLE_DATE, "");
            out.push_str("<v>");
            out.push_str(&number_text(cell.number, SERIAL_DIGITS));
            out.push_str("</v></c>");
        }
        CellType::DateTime => {
            write_open(out, STYLE_DATE_TIME, "");
            out.push_str("<v>");
            out.push_str(&number_text(cell.number, SERIAL_DIGITS));
            out.push_str("</v></c>");
        }
    }
}

fn cell_width(cell: &Cell) -> usize {
    match cell.cell_type {
        CellType::Empty => 0,
        CellType::Text => cell.text.chars().count() + COLUMN_PADDING,
        CellType::Header => cell.text.chars().count() + HEADER_PADDING,
        CellType::Number => number_text(cell.number, VALUE_DIGITS).len() + COLUMN_PADDING,
        CellType::Date => DATE_WIDTH + COLUMN_PADDING,
        CellType::DateTime => DATE_TIME_WIDTH + COLUMN_PADDING,
    }
}

fn columns_xml(sheet: &Sheet) -> String {
    let mut widths: Vec<usize> = Vec::new();
    for row in &sheet.rows {
        if widths.len() < row.len() {
            widths.resize(row.len(), 0);
        }
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell_width(cell));
        }
    }
    if widths.is_empty() {
        return String::new();
    }
    let mut out = String::from("<cols>");
    for (idx, width) in widths.into_iter().enumerate() {
        let clamped = width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        out.push_str(&format!(
            "<col min=\"{col}\" max=\"{col}\" width=\"{clamped}\" customWidth=\"1\"/>",
            col = idx + 1
        ));
    }
    out.push_str("</cols>");
    out
}

fn starts_with_header(sheet: &Sheet) -> bool {
    sheet
        .rows
        .first()
        .is_some_and(|row| row.iter().any(|cell| cell.cell_type == CellType::Header))
}

fn sheet_xml(sheet: &Sheet) -> String {
    let numeric = numeric_columns(sheet);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    if starts_with_header(sheet) {
        out.push_str(
            "<sheetViews><sheetView workbookViewId=\"0\">\
<pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/>\
</sheetView></sheetViews>",
        );
    }
    out.push_str(&columns_xml(sheet));
    out.push_str("<sheetData>");
    for (row_idx, row) in sheet.rows.iter().enumerate() {
        let row_num = row_idx + 1;
        out.push_str(&format!("<row r=\"{row_num}\">"));
        for (col_idx, cell) in row.iter().enumerate() {
            if cell.cell_type != CellType::Empty {
                let cell_ref = format!("{}{row_num}", column_name(col_idx));
                let is_num_col = numeric.get(col_idx).copied().unwrap_or(false);
                append_cell_xml(&mut out, cell, &cell_ref, is_num_col);
            }
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData></worksheet>");
    out
}

fn styles_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"2\">\
<font><sz val=\"11\"/><name val=\"Calibri\"/></font>\
<font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font>\
</fonts>\
<fills count=\"2\">\
<fill><patternFill patternType=\"none\"/></fill>\
<fill><patternFill patternType=\"gray125\"/></fill>\
</fills>\
<borders count=\"1\"><border/></borders>\
<cellStyleXfs count=\"1\">\
<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/>\
</cellStyleXfs>\
<cellXfs count=\"5\">\
<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>\
<xf numFmtId=\"14\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>\
<xf numFmtId=\"22\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>\
<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/>\
<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyAlignment=\"1\">\
<alignment horizontal=\"right\"/>\
</xf>\
</cellXfs>\
</styleSheet>"
}

fn workbook_xml(names: &[String]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
    );
    for (idx, name) in names.iter().enumerate() {
        let id = idx + 1;
        out.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>",
            escape_xml(name)
        ));
    }
    out.push_str("</sheets></workbook>");
    out
}

fn workbook_rels_xml(sheet_count: usize) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for idx in 0..sheet_count {
        let id = idx + 1;
        out.push_str(&format!(
            "<Relationship Id=\"rId{id}\" \
Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" \
Target=\"worksheets/sheet{id}.xml\"/>"
        ));
    }
    let styles_id = sheet_count + 1;
    out.push_str(&format!(
        "<Relationship Id=\"rId{styles_id}\" \
Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" \
Target=\"styles.xml\"/>"
    ));
    out.push_str("</Relationships>");
    out
}

fn root_rels_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" \
Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" \
Target=\"xl/workbook.xml\"/>\
</Relationships>"
}

fn content_types_xml(sheet_count: usize) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>",
    );
    for idx in 0..sheet_count {
        let id = idx + 1;
        out.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{id}.xml\" \
ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
    }
    out.push_str("</Types>");
    out
}

/// Serializes one or more [`Sheet`]s into a valid `.xlsx` ZIP archive byte buffer.
///
/// Port of `Statistic::Xlsx::Serialize` (`statistics_xlsx.cpp:390-427`).
pub fn serialize_xlsx(sheets: &[Sheet]) -> Result<Vec<u8>, XlsxError> {
    if sheets.is_empty() {
        return Err(XlsxError::EmptyWorkbook);
    }
    let names = unique_sheet_names(sheets);
    let count = sheets.len();

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let mut add_entry = |path: &str, bytes: &[u8]| -> Result<(), XlsxError> {
            writer.start_file(path, options)?;
            writer.write_all(bytes)?;
            Ok(())
        };

        add_entry("[Content_Types].xml", content_types_xml(count).as_bytes())?;
        add_entry("_rels/.rels", root_rels_xml().as_bytes())?;
        add_entry("xl/workbook.xml", workbook_xml(&names).as_bytes())?;
        add_entry(
            "xl/_rels/workbook.xml.rels",
            workbook_rels_xml(count).as_bytes(),
        )?;
        add_entry("xl/styles.xml", styles_xml().as_bytes())?;

        for (idx, sheet) in sheets.iter().enumerate() {
            let path = format!("xl/worksheets/sheet{}.xml", idx + 1);
            add_entry(&path, sheet_xml(sheet).as_bytes())?;
        }
        writer.finish()?;
    }

    Ok(cursor.into_inner())
}

fn without_timestamps(chart: &StatisticalChart) -> bool {
    chart.x.first().is_none_or(|&first| first < ONE_DAY_MS)
}

/// Converts a single [`StatisticalChart`] into an XLSX [`Sheet`].
///
/// Port of `SheetFromChart` (`statistics_sheets.cpp:22-55`).
#[must_use]
pub fn sheet_from_chart(title: impl Into<String>, chart: &StatisticalChart) -> Sheet {
    let hours = without_timestamps(chart);
    let whole_day = chart.time_step >= ONE_DAY_MS;

    let mut rows = Vec::with_capacity(chart.x.len() + 1);
    let mut header = Vec::with_capacity(chart.lines.len() + 1);
    header.push(Cell::header(if hours { "Hour" } else { "Date" }));
    for line in &chart.lines {
        header.push(Cell::header(line.name.clone()));
    }
    rows.push(header);

    for (i, &x_val) in chart.x.iter().enumerate() {
        let mut row = Vec::with_capacity(chart.lines.len() + 1);
        row.push(if hours {
            Cell::number(x_val)
        } else if whole_day {
            Cell::date(x_val)
        } else {
            Cell::date_time(x_val)
        });
        for line in &chart.lines {
            if let Some(&y_val) = line.y.get(i) {
                row.push(Cell::number(y_val as f64));
            } else {
                row.push(Cell::empty());
            }
        }
        rows.push(row);
    }

    Sheet::new(title, rows)
}

fn push_chart(sheets: &mut Vec<Sheet>, title: &str, graph: &StatisticalGraph) {
    if !graph.chart.is_empty() {
        sheets.push(sheet_from_chart(title, &graph.chart));
    }
}

fn push_value(rows: &mut Vec<Vec<Cell>>, title: &str, value: StatisticalValue) {
    rows.push(vec![
        Cell::text(title),
        Cell::number(value.value),
        Cell::number(value.previous_value),
    ]);
}

fn push_counter(rows: &mut Vec<Vec<Cell>>, title: &str, value: f64) {
    rows.push(vec![
        Cell::text(title),
        if value < 0.0 {
            Cell::empty()
        } else {
            Cell::number(value)
        },
    ]);
}

fn overview_rows(stats: &AnyStatistics) -> Vec<Vec<Cell>> {
    let mut rows = vec![vec![
        Cell::empty(),
        Cell::header("Value"),
        Cell::header("Previous"),
    ]];
    match stats {
        AnyStatistics::Channel(s) => {
            push_value(&mut rows, "Followers", s.member_count);
            push_value(&mut rows, "Views per post", s.mean_view_count);
            push_value(&mut rows, "Shares per post", s.mean_share_count);
            push_value(&mut rows, "Reactions per post", s.mean_reaction_count);
            push_value(&mut rows, "Views per story", s.mean_story_view_count);
            push_value(&mut rows, "Shares per story", s.mean_story_share_count);
            push_value(
                &mut rows,
                "Reactions per story",
                s.mean_story_reaction_count,
            );
            push_counter(
                &mut rows,
                "Enabled notifications",
                s.enabled_notifications_percentage,
            );
        }
        AnyStatistics::Supergroup(s) => {
            push_value(&mut rows, "Members", s.member_count);
            push_value(&mut rows, "Messages", s.message_count);
            push_value(&mut rows, "Viewing members", s.viewer_count);
            push_value(&mut rows, "Posting members", s.sender_count);
        }
        AnyStatistics::Message(post) | AnyStatistics::Story(post) => {
            push_counter(&mut rows, "Views", f64::from(post.views));
            push_counter(&mut rows, "Reactions", f64::from(post.reactions));
            push_counter(&mut rows, "Public shares", f64::from(post.public_forwards));
            push_counter(
                &mut rows,
                "Private shares",
                f64::from(post.private_forwards),
            );
        }
    }
    rows
}

/// Builds the full list of XLSX [`Sheet`]s for a channel, supergroup, message, or story.
///
/// Port of `Statistic::Sheets` (`statistics_sheets.cpp:169-263`).
#[must_use]
pub fn sheets_from_statistics(
    stats: &AnyStatistics,
    poll_votes: Option<&StatisticalGraph>,
) -> Vec<Sheet> {
    let mut sheets = vec![Sheet::new("Overview", overview_rows(stats))];
    match stats {
        AnyStatistics::Channel(s) => {
            push_chart(&mut sheets, "Growth", &s.member_count_graph);
            push_chart(&mut sheets, "Followers", &s.join_graph);
            push_chart(&mut sheets, "Notifications", &s.mute_graph);
            push_chart(&mut sheets, "Views by hour", &s.view_count_by_hour_graph);
            push_chart(
                &mut sheets,
                "Views by source",
                &s.view_count_by_source_graph,
            );
            push_chart(
                &mut sheets,
                "New followers by source",
                &s.join_by_source_graph,
            );
            push_chart(&mut sheets, "Languages", &s.language_graph);
            push_chart(&mut sheets, "Interactions", &s.message_interaction_graph);
            push_chart(
                &mut sheets,
                "Instant View interactions",
                &s.instant_view_interaction_graph,
            );
            push_chart(
                &mut sheets,
                "Reactions by emotion",
                &s.reactions_by_emotion_graph,
            );
            push_chart(
                &mut sheets,
                "Story interactions",
                &s.story_interactions_graph,
            );
            push_chart(
                &mut sheets,
                "Story reactions by emotion",
                &s.story_reactions_by_emotion_graph,
            );
        }
        AnyStatistics::Supergroup(s) => {
            push_chart(&mut sheets, "Growth", &s.member_count_graph);
            push_chart(&mut sheets, "Members", &s.join_graph);
            push_chart(
                &mut sheets,
                "New members by source",
                &s.join_by_source_graph,
            );
            push_chart(&mut sheets, "Languages", &s.language_graph);
            push_chart(&mut sheets, "Messages", &s.message_content_graph);
            push_chart(&mut sheets, "Actions", &s.action_graph);
            push_chart(&mut sheets, "Top hours", &s.day_graph);
            push_chart(&mut sheets, "Top days of week", &s.week_graph);
        }
        AnyStatistics::Message(post) | AnyStatistics::Story(post) => {
            push_chart(&mut sheets, "Interactions", &post.message_interaction_graph);
            push_chart(
                &mut sheets,
                "Reactions by emotion",
                &post.reactions_by_emotion_graph,
            );
            if let Some(votes) = poll_votes {
                push_chart(&mut sheets, "Poll votes", votes);
            }
        }
    }
    sheets
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use zip::ZipArchive;

    use super::*;

    fn sample_chart(hourly: bool) -> StatisticalChart {
        let json = if hourly {
            br##"{
                "columns": [
                    ["x", 0, 1, 2],
                    ["y0", 12, 34, 56]
                ],
                "types": {"x": "x", "y0": "line"},
                "names": {"y0": "Views & <Clicks>"},
                "colors": {"y0": "#2ea6ff"}
            }"##
            .as_slice()
        } else {
            br##"{
                "columns": [
                    ["x", 1704067200000, 1704153600000],
                    ["y0", 100, 150],
                    ["y1", 5, 12]
                ],
                "types": {"x": "x", "y0": "line", "y1": "line"},
                "names": {"y0": "Joined", "y1": "Left"},
                "colors": {"y0": "#2ea6ff", "y1": "#ff3b30"}
            }"##
            .as_slice()
        };
        StatisticalChart::from_json(json).expect("sample chart json should parse")
    }

    #[test]
    fn column_names_match_excel() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }

    #[test]
    fn empty_workbook_is_rejected() {
        let err = serialize_xlsx(&[]).unwrap_err();
        assert!(matches!(err, XlsxError::EmptyWorkbook));
    }

    #[test]
    fn xlsx_opens_as_valid_zip_with_expected_xml() {
        let sheet1 = Sheet::new(
            "Overview: 2026/Q1?",
            vec![
                vec![Cell::empty(), Cell::header("Value"), Cell::header("Notes")],
                vec![
                    Cell::text("Followers & \"VIP\""),
                    Cell::number(1234.0),
                    Cell::text("ok <tag>"),
                ],
                vec![Cell::text("Rate"), Cell::number(12.5), Cell::empty()],
            ],
        );
        let sheet2 = sheet_from_chart("Growth", &sample_chart(false));
        let sheet3 = sheet_from_chart("Growth", &sample_chart(true));

        let bytes = serialize_xlsx(&[sheet1, sheet2, sheet3]).expect("xlsx should serialize");
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output must be a valid zip");

        let read_entry = |zip: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str| -> String {
            let mut file = zip
                .by_name(name)
                .unwrap_or_else(|e| panic!("missing zip entry {name}: {e}"));
            let mut content = String::new();
            file.read_to_string(&mut content)
                .expect("entry should be valid UTF-8");
            content
        };

        let content_types = read_entry(&mut archive, "[Content_Types].xml");
        assert!(content_types.contains("/xl/workbook.xml"));
        assert!(content_types.contains("/xl/styles.xml"));
        assert!(content_types.contains("/xl/worksheets/sheet1.xml"));
        assert!(content_types.contains("/xl/worksheets/sheet2.xml"));
        assert!(content_types.contains("/xl/worksheets/sheet3.xml"));

        let root_rels = read_entry(&mut archive, "_rels/.rels");
        assert!(root_rels.contains("Target=\"xl/workbook.xml\""));

        let wb_rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels");
        assert!(wb_rels.contains("Target=\"worksheets/sheet1.xml\""));
        assert!(wb_rels.contains("Target=\"worksheets/sheet3.xml\""));
        assert!(wb_rels.contains("Id=\"rId4\""));
        assert!(wb_rels.contains("Target=\"styles.xml\""));

        let workbook = read_entry(&mut archive, "xl/workbook.xml");
        // Forbidden characters `:` `/` `?` stripped, duplicate "Growth" deduplicated.
        assert!(workbook.contains("name=\"Overview 2026Q1\""));
        assert!(workbook.contains("name=\"Growth\""));
        assert!(workbook.contains("name=\"Growth (2)\""));

        let styles = read_entry(&mut archive, "xl/styles.xml");
        assert!(styles.contains("cellXfs count=\"5\""));
        assert!(styles.contains("numFmtId=\"14\""));
        assert!(styles.contains("numFmtId=\"22\""));

        let s1 = read_entry(&mut archive, "xl/worksheets/sheet1.xml");
        // Frozen header row.
        assert!(s1.contains("ySplit=\"1\""));
        // Numeric column B gets right-aligned header style s="4"; text column C gets s="3".
        assert!(s1.contains("<c r=\"B1\" s=\"4\" t=\"inlineStr\">"));
        assert!(s1.contains("<c r=\"C1\" s=\"3\" t=\"inlineStr\">"));
        // Escaped XML entities in text cells.
        assert!(s1.contains("Followers &amp; &quot;VIP&quot;"));
        assert!(s1.contains("ok &lt;tag&gt;"));
        assert!(s1.contains("<c r=\"B2\"><v>1234</v></c>"));
        assert!(s1.contains("<c r=\"B3\"><v>12.500000</v></c>"));

        let s2 = read_entry(&mut archive, "xl/worksheets/sheet2.xml");
        // Daily chart uses Date header and Excel serial date style s="1" (2024-01-01 = 45292).
        assert!(s2.contains("<t xml:space=\"preserve\">Date</t>"));
        assert!(s2.contains("<c r=\"A2\" s=\"1\"><v>45292</v></c>"));
        assert!(s2.contains("<c r=\"B2\"><v>100</v></c>"));
        assert!(s2.contains("<c r=\"C2\"><v>5</v></c>"));

        let s3 = read_entry(&mut archive, "xl/worksheets/sheet3.xml");
        // Hourly chart uses Hour header and plain numbers.
        assert!(s3.contains("<t xml:space=\"preserve\">Hour</t>"));
        assert!(s3.contains("Views &amp; &lt;Clicks&gt;"));
        assert!(s3.contains("<c r=\"A2\"><v>0</v></c>"));
        assert!(s3.contains("<c r=\"B2\"><v>12</v></c>"));
    }

    #[test]
    fn sheets_from_channel_statistics_exports_overview_and_non_empty_graphs() {
        let channel = ChannelStatistics {
            member_count: StatisticalValue::from_current_and_previous(5000.0, 4800.0),
            enabled_notifications_percentage: 42.5,
            member_count_graph: StatisticalGraph {
                chart: sample_chart(false),
                zoom_token: String::new(),
                error: String::new(),
            },
            ..ChannelStatistics::default()
        };

        let sheets = sheets_from_statistics(&AnyStatistics::Channel(Box::new(channel)), None);
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "Overview");
        assert_eq!(sheets[1].name, "Growth");

        let bytes = serialize_xlsx(&sheets).expect("channel stats xlsx should serialize");
        let archive = ZipArchive::new(Cursor::new(bytes)).expect("valid zip");
        assert_eq!(archive.len(), 7);
    }
}
