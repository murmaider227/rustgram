//! Build script for `tg-style`.
//!
//! Port of `Telegram/codegen/codegen/style/` (`parsed_file.cpp`, `module.cpp`,
//! `structure_types.cpp`, `generator.cpp`) and `Telegram/codegen/codegen/common/`
//! (`clean_file.cpp`, `basic_tokenized_file.cpp`).
//!
//! Parses all `.style` files and `ui/colors.palette` under `assets/styles/`,
//! validates every referenced icon asset under `assets/icons/`, packs the icon
//! assets into `$OUT_DIR/icons.pak`, and emits `$OUT_DIR/generated_styles.rs`
//! and `$OUT_DIR/icon_assets.rs`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 1. CleanFile: comment stripping (port of codegen/common/clean_file.cpp)
// ---------------------------------------------------------------------------

struct CleanFile {
    cleaned: Vec<u8>,
    single_line_comments: Vec<String>,
}

fn clean_file(content: &[u8], path: &Path) -> Result<CleanFile, String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CommentState {
        None,
        SingleLine,
        MultiLine,
    }

    let mut state = CommentState::None;
    let mut inside_string = false;
    let mut result = Vec::with_capacity(content.len());
    let mut comments: Vec<String> = Vec::new();
    let mut line_number: usize = 0;

    let mut i = 0;
    let len = content.len();
    let mut offset = 0;

    while i < len {
        let ch = content[i];
        let next = if i + 1 < len { content[i + 1] } else { 0 };

        if state == CommentState::None && ch == b'"' {
            // Count preceding backslashes to handle escaped quotes accurately.
            let mut bs = 0usize;
            let mut j = i;
            while j > 0 && content[j - 1] == b'\\' {
                bs += 1;
                j -= 1;
            }
            if bs.is_multiple_of(2) {
                inside_string = !inside_string;
            }
        }

        if inside_string {
            if ch == b'\n' {
                line_number += 1;
            }
            i += 1;
            continue;
        }

        match state {
            CommentState::None => {
                if ch == b'/' && next == b'/' {
                    if i > offset {
                        result.extend_from_slice(&content[offset..i]);
                    }
                    offset = i;
                    state = CommentState::SingleLine;
                    i += 2;
                } else if ch == b'/' && next == b'*' {
                    if i > offset {
                        result.extend_from_slice(&content[offset..i]);
                    }
                    offset = i;
                    state = CommentState::MultiLine;
                    i += 2;
                } else {
                    if ch == b'\n' {
                        line_number += 1;
                    }
                    i += 1;
                }
            }
            CommentState::SingleLine => {
                if (ch == b'\r' && next == b'\n') || ch == b'\n' {
                    if comments.len() <= line_number {
                        comments.resize(line_number + 1, String::new());
                    }
                    let raw = &content[offset..i];
                    let stripped = raw.strip_prefix(b"//").unwrap_or(raw);
                    comments[line_number] = String::from_utf8_lossy(stripped).trim().to_string();
                    result.push(b' ');
                    if ch == b'\r' && next == b'\n' {
                        result.extend_from_slice(b"\r\n");
                        i += 2;
                    } else {
                        result.push(b'\n');
                        i += 1;
                    }
                    offset = i;
                    line_number += 1;
                    state = CommentState::None;
                } else {
                    i += 1;
                }
            }
            CommentState::MultiLine => {
                if ch == b'*' && next == b'/' {
                    i += 2;
                    result.push(b' ');
                    offset = i;
                    state = CommentState::None;
                } else if ch == b'\r' && next == b'\n' {
                    result.push(b' ');
                    result.extend_from_slice(b"\r\n");
                    i += 2;
                    offset = i;
                    line_number += 1;
                } else if ch == b'\n' {
                    result.push(b' ');
                    result.push(b'\n');
                    i += 1;
                    offset = i;
                    line_number += 1;
                } else {
                    i += 1;
                }
            }
        }
    }

    if comments.len() <= line_number {
        comments.resize(line_number + 1, String::new());
    }

    if state == CommentState::MultiLine {
        return Err(format!(
            "{}: unterminated multi-line comment",
            path.display()
        ));
    }
    if state == CommentState::SingleLine {
        let raw = &content[offset..len];
        let stripped = raw.strip_prefix(b"//").unwrap_or(raw);
        comments[line_number] = String::from_utf8_lossy(stripped).trim().to_string();
        result.push(b' ');
    } else if len > offset {
        result.extend_from_slice(&content[offset..len]);
    }

    Ok(CleanFile {
        cleaned: result,
        single_line_comments: comments,
    })
}

// ---------------------------------------------------------------------------
// 2. BasicTokenizedFile: lexer (port of codegen/common/basic_tokenized_file.cpp)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokKind {
    Int,
    Double,
    String,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Semicolon,
    Comma,
    Dot,
    Hash,
    Plus,
    Minus,
    Equals,
    And,
    Or,
    Name,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokKind,
    text: String,
    line: usize,
}

struct TokenStream {
    path: PathBuf,
    tokens: Vec<Token>,
    comments: Vec<String>,
    pos: usize,
}

impl TokenStream {
    fn tokenize(path: &Path) -> Result<Self, String> {
        let raw = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let cleaned = clean_file(&raw, path)?;
        let bytes = &cleaned.cleaned;
        let len = bytes.len();

        let mut raw_tokens: Vec<(TokKind, String, usize, bool)> = Vec::new();
        let mut i = 0usize;
        let mut line = 1usize;

        while i < len {
            let mut had_ws = false;
            while i < len && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                had_ws = true;
                i += 1;
            }
            if i >= len {
                break;
            }

            let ch = bytes[i];
            let start_line = line;

            if ch == b'"' {
                i += 1;
                let mut val = Vec::new();
                let mut closed = false;
                while i < len {
                    let c = bytes[i];
                    if c == b'"' {
                        i += 1;
                        closed = true;
                        break;
                    }
                    if c == b'\n' {
                        return Err(format!(
                            "{}:{start_line}: unterminated string literal",
                            path.display()
                        ));
                    }
                    if c == b'\\' {
                        i += 1;
                        if i >= len || bytes[i] == b'\n' {
                            return Err(format!(
                                "{}:{start_line}: unterminated string escape",
                                path.display()
                            ));
                        }
                        match bytes[i] {
                            b'n' => val.push(b'\n'),
                            b't' => val.push(b'\t'),
                            b'"' => val.push(b'"'),
                            b'\\' => val.push(b'\\'),
                            other => val.push(other),
                        }
                        i += 1;
                    } else {
                        val.push(c);
                        i += 1;
                    }
                }
                if !closed {
                    return Err(format!(
                        "{}:{start_line}: unterminated string literal",
                        path.display()
                    ));
                }
                let s = String::from_utf8(val).map_err(|_| {
                    format!("{}:{start_line}: invalid UTF-8 in string", path.display())
                })?;
                raw_tokens.push((TokKind::String, s, start_line, had_ws));
            } else if ch.is_ascii_alphanumeric() || ch == b'_' {
                let start = i;
                let mut only_digits = true;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    if !bytes[i].is_ascii_digit() {
                        only_digits = false;
                    }
                    i += 1;
                }
                let text = String::from_utf8_lossy(&bytes[start..i]).into_owned();
                let kind = if only_digits {
                    TokKind::Int
                } else {
                    TokKind::Name
                };
                raw_tokens.push((kind, text, start_line, had_ws));
            } else {
                let kind = match ch {
                    b'(' => TokKind::LParen,
                    b')' => TokKind::RParen,
                    b'{' => TokKind::LBrace,
                    b'}' => TokKind::RBrace,
                    b'[' => TokKind::LBracket,
                    b']' => TokKind::RBracket,
                    b':' => TokKind::Colon,
                    b';' => TokKind::Semicolon,
                    b',' => TokKind::Comma,
                    b'.' => TokKind::Dot,
                    b'#' => TokKind::Hash,
                    b'+' => TokKind::Plus,
                    b'-' => TokKind::Minus,
                    b'=' => TokKind::Equals,
                    b'&' => TokKind::And,
                    b'|' => TokKind::Or,
                    other => {
                        return Err(format!(
                            "{}:{start_line}: unexpected character '{}'",
                            path.display(),
                            other as char
                        ));
                    }
                };
                i += 1;
                raw_tokens.push((kind, (ch as char).to_string(), start_line, had_ws));
            }
        }

        // Combine Int + Dot (+ optional Int) or Dot + Int into Double when adjacent without whitespace
        let mut tokens = Vec::with_capacity(raw_tokens.len());
        let mut idx = 0usize;
        while idx < raw_tokens.len() {
            let (kind, ref text, l, _) = raw_tokens[idx];
            if kind == TokKind::Int
                && idx + 1 < raw_tokens.len()
                && raw_tokens[idx + 1].0 == TokKind::Dot
                && !raw_tokens[idx + 1].3
            {
                let mut combined = text.clone();
                combined.push('.');
                idx += 2;
                if idx < raw_tokens.len() && raw_tokens[idx].0 == TokKind::Int && !raw_tokens[idx].3
                {
                    combined.push_str(&raw_tokens[idx].1);
                    idx += 1;
                }
                tokens.push(Token {
                    kind: TokKind::Double,
                    text: combined,
                    line: l,
                });
            } else if kind == TokKind::Dot
                && idx + 1 < raw_tokens.len()
                && raw_tokens[idx + 1].0 == TokKind::Int
                && !raw_tokens[idx + 1].3
            {
                let mut combined = String::from(".");
                combined.push_str(&raw_tokens[idx + 1].1);
                idx += 2;
                tokens.push(Token {
                    kind: TokKind::Double,
                    text: combined,
                    line: l,
                });
            } else {
                tokens.push(Token {
                    kind,
                    text: text.clone(),
                    line: l,
                });
                idx += 1;
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            tokens,
            comments: cleaned.single_line_comments,
            pos: 0,
        })
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn next_any(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn next_if(&mut self, kind: TokKind) -> Option<Token> {
        if self.pos < self.tokens.len() && self.tokens[self.pos].kind == kind {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn put_back(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    fn expect(&mut self, kind: TokKind) -> Result<Token, String> {
        if let Some(tok) = self.next_if(kind) {
            Ok(tok)
        } else {
            Err(self.err(&format!("expected {kind:?}")))
        }
    }

    fn current_line(&self) -> usize {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].line
        } else if let Some(last) = self.tokens.last() {
            last.line
        } else {
            1
        }
    }

    fn prev_line_comment(&self) -> String {
        let line = if self.pos > 0 {
            self.tokens[self.pos - 1].line
        } else {
            1
        };
        self.comments
            .get(line.saturating_sub(1))
            .cloned()
            .unwrap_or_default()
    }

    fn err(&self, msg: &str) -> String {
        let line = self.current_line();
        let got = self
            .tokens
            .get(self.pos)
            .map(|t| format!("'{}' ({:?})", t.text, t.kind))
            .unwrap_or_else(|| "EOF".to_string());
        format!("{}:{line}: {msg} (got {got})", self.path.display())
    }
}

// ---------------------------------------------------------------------------
// 3. AST / Structure Types (port of codegen/style/structure_types.{h,cpp})
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Type {
    Int,
    Bool,
    Double,
    Pixels,
    String,
    Color,
    Point,
    Size,
    Align,
    Margins,
    Font,
    Icon,
    Struct(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IconModifier {
    Invert,
    FlipHorizontal,
    FlipVertical,
    RotateCw,
    RotateCcw,
    Size(u32, u32),
}

#[derive(Clone, Debug)]
enum IconMaskSpec {
    Png {
        rel_path: String,
        modifiers: Vec<IconModifier>,
    },
    Svg {
        rel_path: String,
        size_override: Option<(u32, u32)>,
        modifiers: Vec<IconModifier>,
    },
    SolidRect {
        width: i32,
        height: i32,
    },
}

#[derive(Clone, Debug)]
struct MonoIconVal {
    mask: IconMaskSpec,
    color: String, // palette key name (e.g. "windowFg", "transparent")
    padding: (i32, i32, i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldStatus {
    Uninitialized,
    Implicit,
    ImplicitOtherModule,
    Explicit,
}

#[derive(Clone, Debug)]
struct FieldVal {
    name: String,
    value: Value,
    status: FieldStatus,
}

#[derive(Clone, Debug)]
enum Value {
    Int(i32),
    Bool(bool),
    Double(f64),
    Pixels(i32),
    String(String),
    Color {
        rgba: [u8; 4],
        palette_name: String,
        fallback: Option<String>,
    },
    Point(i32, i32),
    Size(i32, i32),
    Align(String),
    Margins(i32, i32, i32, i32),
    Font {
        family: String,
        size: i32,
        flags: u8,
    },
    Icon(Vec<MonoIconVal>),
    Struct {
        struct_name: String,
        fields: Vec<FieldVal>,
    },
}

impl Value {
    fn ty(&self) -> Type {
        match self {
            Value::Int(_) => Type::Int,
            Value::Bool(_) => Type::Bool,
            Value::Double(_) => Type::Double,
            Value::Pixels(_) => Type::Pixels,
            Value::String(_) => Type::String,
            Value::Color { .. } => Type::Color,
            Value::Point(_, _) => Type::Point,
            Value::Size(_, _) => Type::Size,
            Value::Align(_) => Type::Align,
            Value::Margins(_, _, _, _) => Type::Margins,
            Value::Font { .. } => Type::Font,
            Value::Icon(_) => Type::Icon,
            Value::Struct { struct_name, .. } => Type::Struct(struct_name.clone()),
        }
    }

    fn default_for(ty: &Type, module: &Module, all_modules: &HashMap<String, Module>) -> Self {
        match ty {
            Type::Int => Value::Int(0),
            Type::Bool => Value::Bool(false),
            Type::Double => Value::Double(0.0),
            Type::Pixels => Value::Pixels(0),
            Type::String => Value::String(String::new()),
            Type::Color => Value::Color {
                rgba: [0, 0, 0, 255],
                palette_name: "windowFg".to_string(),
                fallback: None,
            },
            Type::Point => Value::Point(0, 0),
            Type::Size => Value::Size(0, 0),
            Type::Align => Value::Align("topleft".to_string()),
            Type::Margins => Value::Margins(0, 0, 0, 0),
            Type::Font => Value::Font {
                family: String::new(),
                size: 13,
                flags: 0,
            },
            Type::Icon => Value::Icon(Vec::new()),
            Type::Struct(name) => {
                if let Some(def) = module.find_struct(name, all_modules) {
                    let fields = def
                        .fields
                        .iter()
                        .map(|f| FieldVal {
                            name: f.name.clone(),
                            value: Value::default_for(&f.ty, module, all_modules),
                            status: FieldStatus::Uninitialized,
                        })
                        .collect();
                    Value::Struct {
                        struct_name: name.clone(),
                        fields,
                    }
                } else {
                    Value::Struct {
                        struct_name: name.clone(),
                        fields: Vec::new(),
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct StructFieldDef {
    name: String,
    ty: Type,
}

#[derive(Clone, Debug)]
struct StructDef {
    name: String,
    fields: Vec<StructFieldDef>,
}

#[derive(Clone, Debug)]
struct VariableDef {
    name: String,
    value: Value,
    #[allow(dead_code)]
    description: String,
}

#[derive(Clone, Debug)]
struct Module {
    includes: Vec<String>,
    structs: Vec<StructDef>,
    variables: Vec<VariableDef>,
    struct_idx: HashMap<String, usize>,
    var_idx: HashMap<String, usize>,
}

impl Module {
    fn new() -> Self {
        Self {
            includes: Vec::new(),
            structs: Vec::new(),
            variables: Vec::new(),
            struct_idx: HashMap::new(),
            var_idx: HashMap::new(),
        }
    }

    fn find_struct<'a>(
        &'a self,
        name: &str,
        all: &'a HashMap<String, Module>,
    ) -> Option<&'a StructDef> {
        if let Some(&idx) = self.struct_idx.get(name) {
            return Some(&self.structs[idx]);
        }
        for inc in &self.includes {
            if let Some(m) = all.get(inc)
                && let Some(s) = m.find_struct(name, all)
            {
                return Some(s);
            }
        }
        None
    }

    fn find_variable<'a>(
        &'a self,
        name: &str,
        all: &'a HashMap<String, Module>,
    ) -> Option<(&'a VariableDef, bool)> {
        if let Some(&idx) = self.var_idx.get(name) {
            return Some((&self.variables[idx], true));
        }
        for inc in &self.includes {
            if let Some(m) = all.get(inc)
                && let Some((v, _)) = m.find_variable(name, all)
            {
                return Some((v, false));
            }
        }
        None
    }

    fn add_struct(&mut self, def: StructDef, all: &HashMap<String, Module>) -> Result<(), String> {
        if self.find_struct(&def.name, all).is_some() {
            return Err(format!("struct '{}' already defined", def.name));
        }
        let idx = self.structs.len();
        self.struct_idx.insert(def.name.clone(), idx);
        self.structs.push(def);
        Ok(())
    }

    fn add_variable(
        &mut self,
        var: VariableDef,
        all: &HashMap<String, Module>,
    ) -> Result<(), String> {
        if self.find_variable(&var.name, all).is_some() {
            return Err(format!("variable '{}' already defined", var.name));
        }
        let idx = self.variables.len();
        self.var_idx.insert(var.name.clone(), idx);
        self.variables.push(var);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 4. Parser (port of codegen/style/parsed_file.cpp)
// ---------------------------------------------------------------------------

struct ParseContext {
    styles_root: PathBuf,
    icons_root: PathBuf,
    modules: HashMap<String, Module>,
    include_stack: Vec<String>,
    referenced_pngs: BTreeSet<String>,
    referenced_svgs: BTreeSet<String>,
}

impl ParseContext {
    fn parse_module(&mut self, rel_path: &str) -> Result<String, String> {
        let norm = rel_path.replace('\\', "/");
        if self.modules.contains_key(&norm) {
            return Ok(norm);
        }
        if self.include_stack.contains(&norm) {
            return Err(format!("include cycle detected involving '{norm}'"));
        }

        let full_path = self.styles_root.join(&norm);
        if !full_path.exists() {
            return Err(format!("style file not found: {}", full_path.display()));
        }
        let is_palette = norm.ends_with(".palette");
        let mut ts = TokenStream::tokenize(&full_path)?;
        let mut module = Module::new();

        self.include_stack.push(norm.clone());

        while !ts.at_end() {
            let start = match ts.next_if(TokKind::Name) {
                Some(t) => t,
                None => {
                    return Err(ts.err("expected 'using', struct definition, or variable"));
                }
            };

            if start.text == "using" {
                let inc_tok = ts.expect(TokKind::String)?;
                ts.expect(TokKind::Semicolon)?;
                let resolved_inc = self.resolve_include(&norm, &inc_tok.text)?;
                let canonical = self.parse_module(&resolved_inc)?;
                module.includes.push(canonical);
            } else if ts.next_if(TokKind::LBrace).is_some() {
                if is_palette {
                    return Err(ts.err("struct definitions are not allowed in .palette files"));
                }
                let def = self.read_struct(&mut ts, &start.text, &module)?;
                module
                    .add_struct(def, &self.modules)
                    .map_err(|e| ts.err(&e))?;
            } else if ts.next_if(TokKind::Colon).is_some() {
                let var = self.read_variable(&mut ts, &start.text, &module, is_palette)?;
                module
                    .add_variable(var, &self.modules)
                    .map_err(|e| ts.err(&e))?;
            } else {
                return Err(ts.err("expected '{' or ':' after identifier"));
            }
        }

        self.include_stack.pop();
        self.modules.insert(norm.clone(), module);
        Ok(norm)
    }

    fn resolve_include(&self, current_rel: &str, include_name: &str) -> Result<String, String> {
        let inc_clean = include_name.replace('\\', "/");
        if let Some(parent) = Path::new(current_rel).parent()
            && !parent.as_os_str().is_empty()
        {
            let candidate = parent.join(&inc_clean);
            if self.styles_root.join(&candidate).exists() {
                return Ok(candidate.to_string_lossy().replace('\\', "/"));
            }
        }
        if self.styles_root.join(&inc_clean).exists() {
            return Ok(inc_clean);
        }
        Err(format!(
            "included file '{include_name}' not found (from '{current_rel}')"
        ))
    }

    fn read_struct(
        &self,
        ts: &mut TokenStream,
        name: &str,
        module: &Module,
    ) -> Result<StructDef, String> {
        let mut fields = Vec::new();
        loop {
            if let Some(fname) = ts.next_if(TokKind::Name) {
                ts.expect(TokKind::Colon)?;
                let ty = self.read_type(ts, module)?;
                ts.expect(TokKind::Semicolon)?;
                fields.push(StructFieldDef {
                    name: fname.text,
                    ty,
                });
            } else {
                ts.expect(TokKind::RBrace)?;
                if fields.is_empty() {
                    return Err(ts.err("struct must have at least one field"));
                }
                break;
            }
        }
        Ok(StructDef {
            name: name.to_string(),
            fields,
        })
    }

    fn read_type(&self, ts: &mut TokenStream, module: &Module) -> Result<Type, String> {
        let tok = ts.expect(TokKind::Name)?;
        match tok.text.as_str() {
            "int" => Ok(Type::Int),
            "bool" => Ok(Type::Bool),
            "double" => Ok(Type::Double),
            "pixels" => Ok(Type::Pixels),
            "string" => Ok(Type::String),
            "color" => Ok(Type::Color),
            "point" => Ok(Type::Point),
            "size" => Ok(Type::Size),
            "align" => Ok(Type::Align),
            "margins" => Ok(Type::Margins),
            "font" => Ok(Type::Font),
            "icon" => Ok(Type::Icon),
            other => {
                if module.find_struct(other, &self.modules).is_some() {
                    Ok(Type::Struct(other.to_string()))
                } else {
                    Err(ts.err(&format!("unknown type '{other}'")))
                }
            }
        }
    }

    fn read_variable(
        &mut self,
        ts: &mut TokenStream,
        name: &str,
        module: &Module,
        is_palette: bool,
    ) -> Result<VariableDef, String> {
        let (mut value, is_copy) = self.read_value(ts, module, is_palette)?;
        if is_palette {
            match &mut value {
                Value::Color {
                    palette_name,
                    fallback,
                    ..
                } => {
                    if is_copy {
                        // In a palette file, `boxBg: windowBg;` creates a new palette entry
                        // named `boxBg` whose fallback is `windowBg`.
                        *fallback = Some(palette_name.clone());
                    }
                    *palette_name = name.to_string();
                }
                _ => {
                    return Err(ts.err("palette file may only contain color variables"));
                }
            }
        }
        let is_struct_literal = matches!(value, Value::Struct { .. }) && !is_copy;
        let description = if !is_struct_literal {
            ts.expect(TokKind::Semicolon)?;
            ts.prev_line_comment()
        } else {
            String::new()
        };
        Ok(VariableDef {
            name: name.to_string(),
            value,
            description,
        })
    }

    fn read_value(
        &mut self,
        ts: &mut TokenStream,
        module: &Module,
        is_palette: bool,
    ) -> Result<(Value, bool), String> {
        if let Some(v) = self.try_read_color(ts, module, is_palette)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_point(ts, module)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_size(ts, module)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_bool(ts) {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_align(ts)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_margins(ts, module)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_font(ts, module)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_icon(ts, module, is_palette)? {
            return Ok((v, false));
        }
        if let Some((v, is_copy)) = self.try_read_numeric(ts, module)? {
            return Ok((v, is_copy));
        }
        if let Some(v) = self.try_read_string(ts)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_struct_value(ts, module)? {
            return Ok((v, false));
        }
        if let Some(v) = self.try_read_copy(ts, module)? {
            return Ok((v, true));
        }
        Err(ts.err("expected variable value"))
    }

    fn try_read_color(
        &self,
        ts: &mut TokenStream,
        module: &Module,
        is_palette: bool,
    ) -> Result<Option<Value>, String> {
        if ts.next_if(TokKind::Hash).is_some() {
            if !is_palette {
                return Err(ts.err("hex colors are only allowed in .palette files"));
            }
            let tok = ts
                .next_any()
                .ok_or_else(|| ts.err("expected hex color literal"))?;
            if !matches!(tok.kind, TokKind::Int | TokKind::Name) {
                return Err(ts.err("expected hex color literal"));
            }
            let rgba = parse_hex_color(&tok.text)
                .ok_or_else(|| ts.err(&format!("invalid hex color '#{}'", tok.text)))?;
            let fallback = if ts.next_if(TokKind::Or).is_some() {
                let fb = ts.expect(TokKind::Name)?;
                if module.var_idx.contains_key(&fb.text) {
                    Some(fb.text)
                } else {
                    return Err(ts.err(&format!("fallback color '{}' not found", fb.text)));
                }
            } else {
                None
            };
            return Ok(Some(Value::Color {
                rgba,
                palette_name: String::new(),
                fallback,
            }));
        }
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "transparent" {
                return Ok(Some(Value::Color {
                    rgba: [255, 255, 255, 0],
                    palette_name: "transparent".to_string(),
                    fallback: None,
                }));
            }
            if tok.text == "white" {
                return Ok(Some(Value::Color {
                    rgba: [255, 255, 255, 255],
                    palette_name: "white".to_string(),
                    fallback: None,
                }));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_point(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "point" {
                ts.expect(TokKind::LParen)?;
                let x = self.expect_pixels(ts, module, "point x")?;
                ts.expect(TokKind::Comma)?;
                let y = self.expect_pixels(ts, module, "point y")?;
                ts.expect(TokKind::RParen)?;
                return Ok(Some(Value::Point(x, y)));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_size(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "size" {
                ts.expect(TokKind::LParen)?;
                let w = self.expect_pixels(ts, module, "size width")?;
                ts.expect(TokKind::Comma)?;
                let h = self.expect_pixels(ts, module, "size height")?;
                ts.expect(TokKind::RParen)?;
                return Ok(Some(Value::Size(w, h)));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_bool(&self, ts: &mut TokenStream) -> Option<Value> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "true" {
                return Some(Value::Bool(true));
            }
            if tok.text == "false" {
                return Some(Value::Bool(false));
            }
            ts.put_back();
        }
        None
    }

    fn try_read_align(&self, ts: &mut TokenStream) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "align" {
                ts.expect(TokKind::LParen)?;
                let val = ts.expect(TokKind::Name)?;
                ts.expect(TokKind::RParen)?;
                if !val
                    .text
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    return Err(ts.err(&format!("invalid align value '{}'", val.text)));
                }
                return Ok(Some(Value::Align(val.text)));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_margins(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "margins" {
                ts.expect(TokKind::LParen)?;
                let l = self.expect_pixels(ts, module, "margins left")?;
                ts.expect(TokKind::Comma)?;
                let t = self.expect_pixels(ts, module, "margins top")?;
                ts.expect(TokKind::Comma)?;
                let r = self.expect_pixels(ts, module, "margins right")?;
                ts.expect(TokKind::Comma)?;
                let b = self.expect_pixels(ts, module, "margins bottom")?;
                ts.expect(TokKind::RParen)?;
                return Ok(Some(Value::Margins(l, t, r, b)));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_font(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "font" {
                ts.expect(TokKind::LParen)?;
                let mut flags: u8 = 0;
                let mut family = String::new();
                let mut size: Option<i32> = None;

                loop {
                    if let Some(ftok) = ts.next_if(TokKind::Name) {
                        match ftok.text.as_str() {
                            "bold" | "semibold" => {
                                flags |= 0x01;
                                continue;
                            }
                            "italic" => {
                                flags |= 0x02;
                                continue;
                            }
                            "underline" => {
                                flags |= 0x04;
                                continue;
                            }
                            _ => ts.put_back(),
                        }
                    }
                    if let Some(Value::String(s)) = self.try_read_string_or_copy(ts, module)? {
                        family = s;
                    } else if let Some((v, _)) = self.try_read_numeric(ts, module)? {
                        match v {
                            Value::Pixels(px) => size = Some(px),
                            _ => return Err(ts.err("expected px value for font size")),
                        }
                    } else if ts.next_if(TokKind::RParen).is_some() {
                        break;
                    } else {
                        return Err(ts.err("expected font flag, family, size, or ')'"));
                    }
                }
                let size = size.ok_or_else(|| ts.err("font missing px size"))?;
                return Ok(Some(Value::Font {
                    family,
                    size,
                    flags,
                }));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn try_read_icon(
        &mut self,
        ts: &mut TokenStream,
        module: &Module,
        is_palette: bool,
    ) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::Name) {
            if tok.text == "icon" {
                let mut parts = Vec::new();
                if ts.next_if(TokKind::LBrace).is_some() {
                    loop {
                        if ts.next_if(TokKind::RBrace).is_some() {
                            break;
                        } else if ts.next_if(TokKind::LBrace).is_some() {
                            let part = self.read_mono_icon(ts, module, is_palette)?;
                            ts.expect(TokKind::RBrace)?;
                            parts.push(part);
                            let _ = ts.next_if(TokKind::Comma);
                        } else {
                            return Err(ts.err("expected icon part '{' or '}'"));
                        }
                    }
                } else if ts.next_if(TokKind::LParen).is_some() {
                    let part = self.read_mono_icon(ts, module, is_palette)?;
                    ts.expect(TokKind::RParen)?;
                    parts.push(part);
                }
                return Ok(Some(Value::Icon(parts)));
            }
            ts.put_back();
        }
        Ok(None)
    }

    fn read_mono_icon(
        &mut self,
        ts: &mut TokenStream,
        module: &Module,
        is_palette: bool,
    ) -> Result<MonoIconVal, String> {
        let (first, _) = self.read_value(ts, module, is_palette)?;
        let mask = match first {
            Value::String(spec) => self.resolve_icon_spec(ts, &spec)?,
            Value::Size(width, height) => IconMaskSpec::SolidRect { width, height },
            _ => return Err(ts.err("expected icon filename string or size(w, h)")),
        };
        ts.expect(TokKind::Comma)?;
        let (color_val, _) = self.read_value(ts, module, is_palette)?;
        let color = match color_val {
            Value::Color { palette_name, .. } => palette_name,
            _ => return Err(ts.err("expected color for icon part")),
        };
        let padding = if ts.next_if(TokKind::Comma).is_some() {
            let (pad_val, _) = self.read_value(ts, module, is_palette)?;
            match pad_val {
                Value::Margins(l, t, r, b) => (l, t, r, b),
                Value::Point(x, y) => (x, y, 0, 0),
                _ => return Err(ts.err("expected margins or point for icon offset")),
            }
        } else {
            (0, 0, 0, 0)
        };
        Ok(MonoIconVal {
            mask,
            color,
            padding,
        })
    }

    fn resolve_icon_spec(&mut self, ts: &TokenStream, spec: &str) -> Result<IconMaskSpec, String> {
        let mut split = spec.split('-');
        let raw_path = split.next().unwrap_or("");
        let rel_path = raw_path.trim_start_matches('/').to_string();

        let mut modifiers = Vec::new();
        let mut size_override = None;
        for m in split {
            match m {
                "invert" => modifiers.push(IconModifier::Invert),
                "flip_horizontal" => modifiers.push(IconModifier::FlipHorizontal),
                "flip_vertical" => modifiers.push(IconModifier::FlipVertical),
                "rotate_cw" => modifiers.push(IconModifier::RotateCw),
                "rotate_ccw" => modifiers.push(IconModifier::RotateCcw),
                other => {
                    if let Some((w, h)) = parse_size_modifier(other) {
                        size_override = Some((w, h));
                        modifiers.push(IconModifier::Size(w, h));
                    } else {
                        return Err(ts.err(&format!("unknown icon modifier '{other}' in '{spec}'")));
                    }
                }
            }
        }

        let png_1x = self.icons_root.join(format!("{rel_path}.png"));
        let svg = self.icons_root.join(format!("{rel_path}.svg"));
        if png_1x.exists() {
            self.referenced_pngs.insert(rel_path.clone());
            Ok(IconMaskSpec::Png {
                rel_path,
                modifiers,
            })
        } else if svg.exists() {
            // SVG modifiers other than Size are not used by tdesktop, keep any present
            let non_size_mods: Vec<IconModifier> = modifiers
                .into_iter()
                .filter(|m| !matches!(m, IconModifier::Size(_, _)))
                .collect();
            self.referenced_svgs.insert(rel_path.clone());
            Ok(IconMaskSpec::Svg {
                rel_path,
                size_override,
                modifiers: non_size_mods,
            })
        } else {
            Err(ts.err(&format!(
                "icon asset not found for '{spec}' (looked for {}.png / {}.svg)",
                self.icons_root.join(&rel_path).display(),
                self.icons_root.join(&rel_path).display()
            )))
        }
    }

    fn try_read_positive_literal(&self, ts: &mut TokenStream) -> Result<Option<Value>, String> {
        let Some(tok) = ts.next_any() else {
            return Ok(None);
        };
        match tok.kind {
            TokKind::Int => {
                let v = tok
                    .text
                    .parse::<i32>()
                    .map_err(|e| ts.err(&format!("invalid int '{}': {e}", tok.text)))?;
                Ok(Some(Value::Int(v)))
            }
            TokKind::Double => {
                let v = tok
                    .text
                    .parse::<f64>()
                    .map_err(|e| ts.err(&format!("invalid double '{}': {e}", tok.text)))?;
                Ok(Some(Value::Double(v)))
            }
            TokKind::Name => {
                if let Some(num_str) = tok.text.strip_suffix("px")
                    && !num_str.is_empty()
                    && num_str.bytes().all(|b| b.is_ascii_digit())
                {
                    let v = num_str
                        .parse::<i32>()
                        .map_err(|e| ts.err(&format!("invalid pixels '{}': {e}", tok.text)))?;
                    return Ok(Some(Value::Pixels(v)));
                }
                ts.put_back();
                Ok(None)
            }
            _ => {
                ts.put_back();
                Ok(None)
            }
        }
    }

    fn try_read_numeric_atom(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<(Value, bool)>, String> {
        if let Some(v) = self.try_read_positive_literal(ts)? {
            return Ok(Some((v, false)));
        }
        let saved = ts.pos;
        if let Some(v) = self.try_read_copy(ts, module)? {
            if matches!(v, Value::Int(_) | Value::Double(_) | Value::Pixels(_)) {
                return Ok(Some((v, true)));
            }
            ts.pos = saved;
        }
        Ok(None)
    }

    fn try_read_numeric(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<(Value, bool)>, String> {
        if let Some((mut val, mut is_copy)) = self.try_read_numeric_atom(ts, module)? {
            loop {
                let op = if ts.next_if(TokKind::Plus).is_some() {
                    1i8
                } else if ts.next_if(TokKind::Minus).is_some() {
                    -1i8
                } else {
                    break;
                };
                let (rhs, _) = self
                    .try_read_numeric_atom(ts, module)?
                    .ok_or_else(|| ts.err("expected numeric value after '+' or '-'"))?;
                is_copy = false;
                val = match (val, rhs) {
                    (Value::Pixels(a), Value::Pixels(b)) => {
                        Value::Pixels(if op > 0 { a + b } else { a - b })
                    }
                    (Value::Int(a), Value::Int(b)) => {
                        Value::Int(if op > 0 { a + b } else { a - b })
                    }
                    (Value::Double(a), Value::Double(b)) => {
                        Value::Double(if op > 0 { a + b } else { a - b })
                    }
                    (a, b) => {
                        return Err(ts.err(&format!(
                            "type mismatch in arithmetic: {:?} vs {:?}",
                            a.ty(),
                            b.ty()
                        )));
                    }
                };
            }
            return Ok(Some((val, is_copy)));
        }
        if ts.next_if(TokKind::Minus).is_some() {
            let (inner, _) = self
                .try_read_numeric(ts, module)?
                .ok_or_else(|| ts.err("expected numeric value after '-'"))?;
            let negated = match inner {
                Value::Pixels(v) => Value::Pixels(-v),
                Value::Int(v) => Value::Int(-v),
                Value::Double(v) => Value::Double(-v),
                _ => return Err(ts.err("cannot negate non-numeric value")),
            };
            return Ok(Some((negated, false)));
        }
        Ok(None)
    }

    fn expect_pixels(
        &self,
        ts: &mut TokenStream,
        module: &Module,
        ctx: &str,
    ) -> Result<i32, String> {
        match self.try_read_numeric(ts, module)? {
            Some((Value::Pixels(px), _)) => Ok(px),
            _ => Err(ts.err(&format!("expected px value for {ctx}"))),
        }
    }

    fn try_read_string(&self, ts: &mut TokenStream) -> Result<Option<Value>, String> {
        if let Some(tok) = ts.next_if(TokKind::String) {
            if !tok.text.is_ascii() {
                return Err(ts.err("non-ASCII string literal not supported"));
            }
            return Ok(Some(Value::String(tok.text)));
        }
        Ok(None)
    }

    fn try_read_string_or_copy(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        if let Some(v) = self.try_read_string(ts)? {
            return Ok(Some(v));
        }
        let saved = ts.pos;
        if let Some(v) = self.try_read_copy(ts, module)? {
            if matches!(v, Value::String(_)) {
                return Ok(Some(v));
            }
            ts.pos = saved;
        }
        Ok(None)
    }

    fn try_read_struct_value(
        &mut self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        let Some(tok) = ts.next_if(TokKind::Name) else {
            return Ok(None);
        };
        if module.find_struct(&tok.text, &self.modules).is_none() {
            ts.put_back();
            return Ok(None);
        }
        let mut result = Value::default_for(&Type::Struct(tok.text.clone()), module, &self.modules);

        if ts.next_if(TokKind::LParen).is_some() {
            loop {
                let parent_tok = ts.expect(TokKind::Name)?;
                self.apply_struct_parent(ts, module, &mut result, &parent_tok.text)?;
                if ts.next_if(TokKind::RParen).is_some() {
                    break;
                }
                ts.expect(TokKind::Comma)?;
            }
        }

        ts.expect(TokKind::LBrace)?;
        loop {
            if let Some(field_name) = ts.next_if(TokKind::Name) {
                ts.expect(TokKind::Colon)?;
                let field_var = self.read_variable(ts, &field_name.text, module, false)?;
                assign_struct_field(ts, &mut result, field_var)?;
            } else {
                ts.expect(TokKind::RBrace)?;
                break;
            }
        }
        Ok(Some(result))
    }

    fn apply_struct_parent(
        &self,
        ts: &TokenStream,
        module: &Module,
        result: &mut Value,
        parent_name: &str,
    ) -> Result<(), String> {
        let (parent, from_same_module) = module
            .find_variable(parent_name, &self.modules)
            .ok_or_else(|| ts.err(&format!("struct parent '{parent_name}' not found")))?;
        if parent.value.ty() != result.ty() {
            return Err(ts.err(&format!(
                "struct parent '{parent_name}' has type {:?} while child has {:?}",
                parent.value.ty(),
                result.ty()
            )));
        }
        let (
            Value::Struct {
                fields: src_fields, ..
            },
            Value::Struct {
                fields: dst_fields, ..
            },
        ) = (&parent.value, result)
        else {
            return Err(ts.err("internal struct mismatch"));
        };
        for (src, dst) in src_fields.iter().zip(dst_fields.iter_mut()) {
            if src.status == FieldStatus::Explicit || dst.status == FieldStatus::Uninitialized {
                dst.value = src.value.clone();
                dst.status = if from_same_module {
                    FieldStatus::Implicit
                } else {
                    FieldStatus::ImplicitOtherModule
                };
            }
        }
        Ok(())
    }

    fn try_read_copy(
        &self,
        ts: &mut TokenStream,
        module: &Module,
    ) -> Result<Option<Value>, String> {
        let Some(tok) = ts.next_if(TokKind::Name) else {
            return Ok(None);
        };
        let Some((var, _)) = module.find_variable(&tok.text, &self.modules) else {
            ts.put_back();
            return Ok(None);
        };
        let mut current = var.value.clone();
        while ts.next_if(TokKind::Dot).is_some() {
            let field = ts.expect(TokKind::Name)?;
            current = match current {
                Value::Size(w, h) => match field.text.as_str() {
                    "width" => Value::Pixels(w),
                    "height" => Value::Pixels(h),
                    other => return Err(ts.err(&format!("size has no field '{other}'"))),
                },
                Value::Point(x, y) => match field.text.as_str() {
                    "x" => Value::Pixels(x),
                    "y" => Value::Pixels(y),
                    other => return Err(ts.err(&format!("point has no field '{other}'"))),
                },
                Value::Margins(l, t, r, b) => match field.text.as_str() {
                    "left" => Value::Pixels(l),
                    "top" => Value::Pixels(t),
                    "right" => Value::Pixels(r),
                    "bottom" => Value::Pixels(b),
                    "leftRight" => Value::Pixels(l + r),
                    "topBottom" => Value::Pixels(t + b),
                    other => return Err(ts.err(&format!("margins has no field '{other}'"))),
                },
                Value::Struct { fields, .. } => {
                    if let Some(f) = fields.into_iter().find(|f| f.name == field.text) {
                        f.value
                    } else {
                        return Err(ts.err(&format!("unknown struct field '{}'", field.text)));
                    }
                }
                _ => {
                    return Err(ts.err(&format!(
                        "cannot access field '{}' on {:?}",
                        field.text,
                        current.ty()
                    )));
                }
            };
        }
        Ok(Some(current))
    }
}

fn assign_struct_field(
    ts: &TokenStream,
    target: &mut Value,
    field: VariableDef,
) -> Result<(), String> {
    let Value::Struct {
        struct_name,
        fields,
    } = target
    else {
        return Err(ts.err("internal error: target is not a struct"));
    };
    for existing in fields.iter_mut() {
        if existing.name == field.name {
            if existing.value.ty() != field.value.ty() {
                return Err(ts.err(&format!(
                    "field '{}' in '{}' expects {:?} but got {:?}",
                    field.name,
                    struct_name,
                    existing.value.ty(),
                    field.value.ty()
                )));
            }
            existing.value = field.value;
            existing.status = FieldStatus::Explicit;
            return Ok(());
        }
    }
    Err(ts.err(&format!(
        "unknown field '{}' in struct '{struct_name}'",
        field.name
    )))
}

fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let lower = s.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    if bytes.len() != 6 && bytes.len() != 8 {
        return None;
    }
    let hex_nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    };
    let hex_byte =
        |hi: u8, lo: u8| -> Option<u8> { Some((hex_nibble(hi)? << 4) | hex_nibble(lo)?) };
    let r = hex_byte(bytes[0], bytes[1])?;
    let g = hex_byte(bytes[2], bytes[3])?;
    let b = hex_byte(bytes[4], bytes[5])?;
    let a = if bytes.len() == 8 {
        hex_byte(bytes[6], bytes[7])?
    } else {
        255
    };
    Some([r, g, b, a])
}

fn parse_size_modifier(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let w: u32 = w.parse().ok()?;
    let h: u32 = h.parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

// ---------------------------------------------------------------------------
// 5. PNG IHDR validation & Icon Packing
// ---------------------------------------------------------------------------

fn read_png_dimensions(bytes: &[u8], path: &Path) -> Result<(u32, u32), String> {
    const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.len() < 24 || bytes[0..8] != SIG || &bytes[12..16] != b"IHDR" {
        return Err(format!("invalid PNG header in {}", path.display()));
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        return Err(format!("zero PNG dimensions in {}", path.display()));
    }
    Ok((w, h))
}

fn pack_icon_assets(
    icons_root: &Path,
    pngs: &BTreeSet<String>,
    svgs: &BTreeSet<String>,
    out_dir: &Path,
) -> Result<(), String> {
    let mut pak = Vec::with_capacity(3 * 1024 * 1024);
    let mut entries: BTreeMap<String, (u32, u32)> = BTreeMap::new();

    for rel in pngs {
        let mut dims = [(0u32, 0u32); 3];
        for (idx, suffix) in ["", "@2x", "@3x"].iter().enumerate() {
            let key = format!("{rel}{suffix}.png");
            let full = icons_root.join(&key);
            let bytes = fs::read(&full)
                .map_err(|e| format!("missing PNG variant {}: {e}", full.display()))?;
            dims[idx] = read_png_dimensions(&bytes, &full)?;
            let offset = pak.len() as u32;
            let length = bytes.len() as u32;
            pak.extend_from_slice(&bytes);
            entries.insert(key, (offset, length));
        }
        let (w1, h1) = dims[0];
        let (w2, h2) = dims[1];
        let (w3, h3) = dims[2];
        if w1 * 2 != w2 || h1 * 2 != h2 || w1 * 3 != w3 || h1 * 3 != h3 {
            return Err(format!(
                "PNG dimension mismatch for '{rel}': 1x={w1}x{h1}, 2x={w2}x{h2}, 3x={w3}x{h3}"
            ));
        }
    }

    for rel in svgs {
        let key = format!("{rel}.svg");
        let full = icons_root.join(&key);
        let bytes =
            fs::read(&full).map_err(|e| format!("missing SVG asset {}: {e}", full.display()))?;
        let offset = pak.len() as u32;
        let length = bytes.len() as u32;
        pak.extend_from_slice(&bytes);
        entries.insert(key, (offset, length));
    }

    fs::write(out_dir.join("icons.pak"), &pak)
        .map_err(|e| format!("failed to write icons.pak: {e}"))?;

    let mut code = String::with_capacity(entries.len() * 64 + 512);
    writeln!(
        code,
        "static ICONS_PAK: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/icons.pak\"));"
    )
    .unwrap();
    writeln!(
        code,
        "/// Total number of bundled icon asset files (PNG 1x/@2x/@3x + SVG)."
    )
    .unwrap();
    writeln!(
        code,
        "pub const ICON_ASSET_COUNT: usize = {};",
        entries.len()
    )
    .unwrap();
    writeln!(code, "static ICON_INDEX: &[(&str, u32, u32)] = &[").unwrap();
    for (key, (offset, len)) in &entries {
        writeln!(code, "    ({key:?}, {offset}, {len}),").unwrap();
    }
    writeln!(code, "];").unwrap();
    writeln!(
        code,
        r#"
/// Look up raw bytes (PNG or SVG) of a bundled icon asset by relative path (e.g. `"simple_close.png"`, `"simple_close@2x.png"`, `"menu/zoom_in.svg"`).
#[must_use]
pub fn icon_asset_bytes(rel_path: &str) -> Option<&'static [u8]> {{
    let idx = ICON_INDEX.binary_search_by_key(&rel_path, |&(k, _, _)| k).ok()?;
    let (_, offset, len) = ICON_INDEX[idx];
    let start = offset as usize;
    let end = start + len as usize;
    ICONS_PAK.get(start..end)
}}

/// Iterator over all bundled icon asset keys (relative paths).
pub fn icon_asset_keys() -> impl Iterator<Item = &'static str> {{
    ICON_INDEX.iter().map(|&(k, _, _)| k)
}}
"#
    )
    .unwrap();

    fs::write(out_dir.join("icon_assets.rs"), code)
        .map_err(|e| format!("failed to write icon_assets.rs: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. Rust Code Generator (port of codegen/style/generator.cpp)
// ---------------------------------------------------------------------------

fn to_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                let prev = chars[i - 1];
                let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
                if prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_is_lower)
                {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn escape_ident(ident: &str) -> String {
    match ident {
        "as" | "async" | "await" | "break" | "const" | "continue" | "dyn" | "else" | "enum"
        | "extern" | "false" | "fn" | "for" | "gen" | "if" | "impl" | "in" | "let" | "loop"
        | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "static" | "struct"
        | "trait" | "true" | "type" | "unsafe" | "use" | "where" | "while" | "abstract"
        | "become" | "box" | "do" | "final" | "macro" | "override" | "priv" | "try" | "typeof"
        | "unsized" | "virtual" | "yield" => format!("r#{ident}"),
        "self" | "Self" | "super" | "crate" => format!("{ident}_"),
        _ => ident.to_string(),
    }
}

fn rust_field_ident(name: &str) -> String {
    escape_ident(&to_snake_case(name))
}

fn rust_var_ident(name: &str) -> String {
    escape_ident(&to_snake_case(name))
}

fn rust_type(ty: &Type) -> String {
    match ty {
        Type::Int => "i32".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Double => "f64".to_string(),
        Type::Pixels => "crate::types::Pixels".to_string(),
        Type::String => "&'static str".to_string(),
        Type::Color => "crate::color::ColorRef".to_string(),
        Type::Point => "crate::types::Point".to_string(),
        Type::Size => "crate::types::Size".to_string(),
        Type::Align => "crate::types::Align".to_string(),
        Type::Margins => "crate::types::Margins".to_string(),
        Type::Font => "crate::font::Font".to_string(),
        Type::Icon => "crate::icon::Icon".to_string(),
        Type::Struct(name) => format!("crate::structs::{name}"),
    }
}

fn rust_align(align: &str) -> Result<&'static str, String> {
    match align {
        "topleft" => Ok("crate::types::Align::TopLeft"),
        "top" => Ok("crate::types::Align::Top"),
        "topright" => Ok("crate::types::Align::TopRight"),
        "right" => Ok("crate::types::Align::Right"),
        "bottomright" => Ok("crate::types::Align::BottomRight"),
        "bottom" => Ok("crate::types::Align::Bottom"),
        "bottomleft" => Ok("crate::types::Align::BottomLeft"),
        "left" => Ok("crate::types::Align::Left"),
        "center" => Ok("crate::types::Align::Center"),
        "justify" => Ok("crate::types::Align::Justify"),
        other => Err(format!("unsupported align value '{other}'")),
    }
}

fn rust_icon_modifier(m: &IconModifier) -> String {
    match m {
        IconModifier::Invert => "crate::icon::IconModifier::Invert".to_string(),
        IconModifier::FlipHorizontal => "crate::icon::IconModifier::FlipHorizontal".to_string(),
        IconModifier::FlipVertical => "crate::icon::IconModifier::FlipVertical".to_string(),
        IconModifier::RotateCw => "crate::icon::IconModifier::RotateCw".to_string(),
        IconModifier::RotateCcw => "crate::icon::IconModifier::RotateCcw".to_string(),
        IconModifier::Size(w, h) => format!("crate::icon::IconModifier::Size({w}, {h})"),
    }
}

fn format_f64(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

fn rust_value(val: &Value) -> Result<String, String> {
    match val {
        Value::Int(v) => Ok(format!("{v}_i32")),
        Value::Bool(v) => Ok(if *v { "true" } else { "false" }.to_string()),
        Value::Double(v) => Ok(format!("{}_f64", format_f64(*v))),
        Value::Pixels(px) => Ok(format!("crate::types::Pixels::new({px})")),
        Value::String(s) => Ok(format!("{s:?}")),
        Value::Color { palette_name, .. } => {
            Ok(format!("crate::color::ColorRef::new({palette_name:?})"))
        }
        Value::Point(x, y) => Ok(format!("crate::types::Point::new({x}, {y})")),
        Value::Size(w, h) => Ok(format!("crate::types::Size::new({w}, {h})")),
        Value::Align(a) => Ok(rust_align(a)?.to_string()),
        Value::Margins(l, t, r, b) => Ok(format!("crate::types::Margins::new({l}, {t}, {r}, {b})")),
        Value::Font {
            family,
            size,
            flags,
        } => Ok(format!(
            "crate::font::Font::new({family:?}, crate::types::Pixels::new({size}), crate::font::FontFlags::from_bits({flags}))"
        )),
        Value::Icon(parts) => {
            if parts.is_empty() {
                return Ok("crate::icon::Icon::EMPTY".to_string());
            }
            let mut part_exprs = Vec::with_capacity(parts.len());
            for part in parts {
                let mask_expr = match &part.mask {
                    IconMaskSpec::Png {
                        rel_path,
                        modifiers,
                    } => {
                        let mods = modifiers
                            .iter()
                            .map(rust_icon_modifier)
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "crate::icon::IconMaskRef::Png {{ path: {rel_path:?}, modifiers: &[{mods}] }}"
                        )
                    }
                    IconMaskSpec::Svg {
                        rel_path,
                        size_override,
                        modifiers,
                    } => {
                        let sz = match size_override {
                            Some((w, h)) => format!("Some(({w}, {h}))"),
                            None => "None".to_string(),
                        };
                        let mods = modifiers
                            .iter()
                            .map(rust_icon_modifier)
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "crate::icon::IconMaskRef::Svg {{ path: {rel_path:?}, size_override: {sz}, modifiers: &[{mods}] }}"
                        )
                    }
                    IconMaskSpec::SolidRect { width, height } => {
                        format!(
                            "crate::icon::IconMaskRef::SolidRect {{ width: crate::types::Pixels::new({width}), height: crate::types::Pixels::new({height}) }}"
                        )
                    }
                };
                let (l, t, r, b) = part.padding;
                part_exprs.push(format!(
                    "crate::icon::MonoIcon {{ mask: {mask_expr}, color: crate::color::ColorRef::new({:?}), padding: crate::types::Margins::new({l}, {t}, {r}, {b}) }}",
                    part.color
                ));
            }
            Ok(format!(
                "crate::icon::Icon::new(&[{}])",
                part_exprs.join(", ")
            ))
        }
        Value::Struct {
            struct_name,
            fields,
        } => {
            let mut inits = Vec::with_capacity(fields.len());
            for f in fields {
                let fname = rust_field_ident(&f.name);
                let fval = rust_value(&f.value)?;
                inits.push(format!("{fname}: {fval}"));
            }
            Ok(format!(
                "crate::structs::{struct_name} {{ {} }}",
                inits.join(", ")
            ))
        }
    }
}

#[derive(Default)]
struct ModTree {
    /// Modules (files) whose items are emitted directly in this Rust module.
    direct_files: Vec<String>,
    children: BTreeMap<String, ModTree>,
}

fn module_path_for_file(rel_path: &str) -> Vec<String> {
    let without_ext = rel_path.strip_suffix(".style").unwrap_or(rel_path);
    let mut parts: Vec<String> = without_ext.split('/').map(|s| s.to_string()).collect();
    if parts.len() >= 2 && parts[parts.len() - 1] == parts[parts.len() - 2] {
        parts.pop();
    }
    parts
}

fn emit_mod_tree(
    node: &ModTree,
    modules: &HashMap<String, Module>,
    out: &mut String,
    indent: usize,
) -> Result<(), String> {
    let pad = " ".repeat(indent);

    for rel in &node.direct_files {
        let m = &modules[rel];
        writeln!(out, "{pad}// Source: assets/styles/{rel}").unwrap();

        // Re-export structs declared in this file
        for s in &m.structs {
            writeln!(out, "{pad}pub use crate::structs::{};", s.name).unwrap();
        }

        // Emit all variables declared in this file
        for v in &m.variables {
            let ident = rust_var_ident(&v.name);
            let ty = rust_type(&v.value.ty());
            let expr = rust_value(&v.value)?;
            writeln!(out, "{pad}/// Style variable `{}` from `{rel}`.", v.name).unwrap();
            writeln!(out, "{pad}pub static {ident}: {ty} = {expr};").unwrap();
        }
    }

    for (child_name, child) in &node.children {
        let mod_ident = escape_ident(child_name);
        writeln!(out, "{pad}/// Generated styles for `{child_name}`.").unwrap();
        writeln!(out, "{pad}pub mod {mod_ident} {{").unwrap();
        emit_mod_tree(child, modules, out, indent + 4)?;
        writeln!(out, "{pad}}}").unwrap();
    }
    Ok(())
}

fn collect_style_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("failed to read dir {}: {e}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("dir entry error: {e}"))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_style_files(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("style") {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| format!("strip_prefix error: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        panic!("tg-style build.rs failed: {e}");
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let styles_root = manifest_dir.join("assets").join("styles");
    let icons_root = manifest_dir.join("assets").join("icons");

    println!("cargo:rerun-if-changed=assets/styles");
    println!("cargo:rerun-if-changed=assets/icons");
    println!("cargo:rerun-if-changed=build.rs");

    let mut style_files = Vec::new();
    collect_style_files(&styles_root, &styles_root, &mut style_files)?;
    style_files.sort();

    let mut ctx = ParseContext {
        styles_root: styles_root.clone(),
        icons_root: icons_root.clone(),
        modules: HashMap::new(),
        include_stack: Vec::new(),
        referenced_pngs: BTreeSet::new(),
        referenced_svgs: BTreeSet::new(),
    };

    // Parse palette first so default colors are available
    ctx.parse_module("ui/colors.palette")?;

    // Parse every .style file
    for rel in &style_files {
        ctx.parse_module(rel)?;
    }

    // Validate & pack all referenced icon assets
    pack_icon_assets(
        &icons_root,
        &ctx.referenced_pngs,
        &ctx.referenced_svgs,
        &out_dir,
    )?;

    // Gather all declared structs (verify global uniqueness)
    let mut all_structs: BTreeMap<String, (&StructDef, String)> = BTreeMap::new();
    for rel in &style_files {
        let m = &ctx.modules[rel];
        for s in &m.structs {
            if let Some((_, prev_rel)) = all_structs.insert(s.name.clone(), (s, rel.clone())) {
                return Err(format!(
                    "duplicate struct '{}' in '{prev_rel}' and '{rel}'",
                    s.name
                ));
            }
        }
    }

    // Build module tree and check for variable collisions (both per-module and global snake_case)
    let mut tree = ModTree::default();
    let mut global_vars: BTreeMap<String, (Vec<String>, String, String)> = BTreeMap::new();
    let mut total_vars = 0usize;

    for rel in &style_files {
        let m = &ctx.modules[rel];
        let mod_path = module_path_for_file(rel);
        let mut cursor = &mut tree;
        for seg in &mod_path {
            cursor = cursor.children.entry(seg.clone()).or_default();
        }
        cursor.direct_files.push(rel.clone());

        for v in &m.variables {
            total_vars += 1;
            let snake = rust_var_ident(&v.name);
            if let Some((_, prev_orig, prev_rel)) = global_vars.insert(
                snake.clone(),
                (mod_path.clone(), v.name.clone(), rel.clone()),
            ) {
                return Err(format!(
                    "snake_case variable collision '{snake}': '{prev_orig}' ({prev_rel}) vs '{}' ({rel})",
                    v.name
                ));
            }
        }
    }

    // Emit generated_styles.rs
    let mut out = String::with_capacity(2 * 1024 * 1024);
    writeln!(
        out,
        "// @generated by crates/tg-style/build.rs — do not edit manually."
    )
    .unwrap();
    writeln!(
        out,
        "/// Number of `.style` files compiled into this crate."
    )
    .unwrap();
    writeln!(
        out,
        "pub const STYLE_FILE_COUNT: usize = {};",
        style_files.len()
    )
    .unwrap();
    writeln!(
        out,
        "/// Number of style structs declared across all `.style` files."
    )
    .unwrap();
    writeln!(
        out,
        "pub const STYLE_STRUCT_COUNT: usize = {};",
        all_structs.len()
    )
    .unwrap();
    writeln!(
        out,
        "/// Number of style variables generated across all `.style` files."
    )
    .unwrap();
    writeln!(out, "pub const STYLE_VARIABLE_COUNT: usize = {total_vars};").unwrap();

    // 1. Default palette colors from ui/colors.palette
    let palette = &ctx.modules["ui/colors.palette"];
    let mut palette_entries: BTreeMap<&str, [u8; 4]> = BTreeMap::new();
    palette_entries.insert("transparent", [255, 255, 255, 0]);
    palette_entries.insert("white", [255, 255, 255, 255]);
    for v in &palette.variables {
        if let Value::Color { rgba, .. } = v.value {
            palette_entries.insert(&v.name, rgba);
        }
    }
    writeln!(
        out,
        "/// Number of palette color entries (including `transparent` and `white`)."
    )
    .unwrap();
    writeln!(
        out,
        "pub const PALETTE_COLOR_COUNT: usize = {};",
        palette_entries.len()
    )
    .unwrap();
    writeln!(
        out,
        "pub(crate) static DEFAULT_PALETTE_COLORS: &[(&str, [u8; 4])] = &["
    )
    .unwrap();
    for (name, [r, g, b, a]) in &palette_entries {
        writeln!(out, "    ({name:?}, [{r}, {g}, {b}, {a}]),").unwrap();
    }
    writeln!(out, "];").unwrap();

    // 2. All declared style structs in `pub mod structs`
    writeln!(
        out,
        "\n/// All style struct types declared across the `.style` files."
    )
    .unwrap();
    writeln!(out, "pub mod structs {{").unwrap();
    for (name, (def, rel)) in &all_structs {
        writeln!(out, "    /// Style struct `{name}` declared in `{rel}`.").unwrap();
        writeln!(out, "    #[derive(Clone, Copy, Debug, PartialEq)]").unwrap();
        writeln!(out, "    pub struct {name} {{").unwrap();
        for f in &def.fields {
            let fname = rust_field_ident(&f.name);
            let fty = rust_type(&f.ty);
            writeln!(out, "        /// Field `{}`.", f.name).unwrap();
            writeln!(out, "        pub {fname}: {fty},").unwrap();
        }
        writeln!(out, "    }}").unwrap();
    }
    writeln!(out, "}}").unwrap();

    // 3. Hierarchical style modules
    writeln!(
        out,
        "\n/// Generated style modules mirroring the `.style` source tree."
    )
    .unwrap();
    writeln!(
        out,
        "#[allow(non_upper_case_globals, clippy::excessive_precision)]"
    )
    .unwrap();
    writeln!(out, "pub mod styles {{").unwrap();
    emit_mod_tree(&tree, &ctx.modules, &mut out, 4)?;
    writeln!(out, "}}").unwrap();

    // 4. Flat `st` namespace re-exporting all style variables (mirrors C++ `st::*`)
    writeln!(
        out,
        "\n/// Flat namespace re-exporting every generated style variable (mirrors tdesktop's `st::*`)."
    )
    .unwrap();
    writeln!(out, "#[allow(non_upper_case_globals)]").unwrap();
    writeln!(out, "pub mod st {{").unwrap();
    for (snake, (mod_path, _, _)) in &global_vars {
        let path_str = mod_path
            .iter()
            .map(|s| escape_ident(s))
            .collect::<Vec<_>>()
            .join("::");
        writeln!(out, "    pub use crate::styles::{path_str}::{snake};").unwrap();
    }
    writeln!(out, "}}").unwrap();

    // 5. Re-export top-level style directories at crate root
    writeln!(
        out,
        "\n// Top-level style directory re-exports (e.g. `tg_style::ui::widgets::*`)."
    )
    .unwrap();
    for top in tree.children.keys() {
        let ident = escape_ident(top);
        writeln!(out, "pub use crate::styles::{ident};").unwrap();
    }

    fs::write(out_dir.join("generated_styles.rs"), out)
        .map_err(|e| format!("failed to write generated_styles.rs: {e}"))?;
    Ok(())
}
