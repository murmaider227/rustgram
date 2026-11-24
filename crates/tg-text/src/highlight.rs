//! Basic Prism-style syntax highlighter for `<pre>` code blocks with a language tag.
//!
//! Port of `Telegram/lib_spellcheck/spellcheck/spellcheck_highlight_syntax.cpp` and
//! the core Prism grammar rules (`Telegram/ThirdParty/libprisma`).
//!
//! Produces both UTF-8 [`HighlightedSpan`] slices (for direct `tg-ui` painting) and
//! UTF-16 [`EntityInText`] (`EntityType::Colorized`) entries matching tdesktop's
//! 1..=8 syntax color slot mapping:
//!
//! | Slot | Prism token types |
//! |------|-------------------|
//! | `1`  | `comment`, `block-comment`, `prolog`, `doctype`, `cdata` |
//! | `2`  | `punctuation` |
//! | `3`  | `property`, `tag`, `boolean`, `number`, `constant`, `symbol`, `deleted` |
//! | `4`  | `selector`, `attr-name`, `string`, `char`, `builtin` |
//! | `5`  | `operator`, `entity`, `url` |
//! | `6`  | `atrule`, `attr-value`, `keyword`, `function` |
//! | `7`  | `class-name` |
//! | `8`  | `inserted` |

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use crate::entity::{EntitiesInText, EntityInText, EntityType, TextWithEntities};
use crate::utf16::OffsetMapper;

/// Maximum cached highlighted code blocks (`kCacheLimit = 256` in `spellcheck_highlight_syntax.cpp`).
pub const HIGHLIGHT_CACHE_LIMIT: usize = 256;

/// Syntax token category mapped to tdesktop's 1..=8 `EntityType::Colorized` slot indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxTokenKind {
    /// Comments, doc comments, prolog, doctype, CDATA (slot 1).
    Comment = 1,
    /// Braces, brackets, parens, commas, semicolons, colons (slot 2).
    Punctuation = 2,
    /// Numbers, booleans, constants, tags, properties, symbols, diff deletions (slot 3).
    Literal = 3,
    /// Strings, chars, builtins, selectors, attribute names (slot 4).
    String = 4,
    /// Operators, entities, URLs (slot 5).
    Operator = 5,
    /// Language keywords, functions, at-rules, attribute values (slot 6).
    Keyword = 6,
    /// Type / class / struct / trait names (slot 7).
    ClassName = 7,
    /// Diff insertions (slot 8).
    Inserted = 8,
}

impl SyntaxTokenKind {
    /// Returns the 1-based tdesktop `EntityType::Colorized` slot index (`1..=8`).
    #[inline]
    #[must_use]
    pub const fn color_index(self) -> u8 {
        self as u8
    }

    /// Converts a 1-based color slot index (`1..=8`) back to [`SyntaxTokenKind`].
    #[inline]
    #[must_use]
    pub const fn from_color_index(index: u8) -> Option<Self> {
        match index {
            1 => Some(Self::Comment),
            2 => Some(Self::Punctuation),
            3 => Some(Self::Literal),
            4 => Some(Self::String),
            5 => Some(Self::Operator),
            6 => Some(Self::Keyword),
            7 => Some(Self::ClassName),
            8 => Some(Self::Inserted),
            _ => None,
        }
    }

    /// Maps a Prism token type name to its tdesktop color slot (`spellcheck_highlight_syntax.cpp:102-130`).
    #[must_use]
    pub fn from_prism_token(name: &str) -> Option<Self> {
        match name {
            "comment" | "block-comment" | "prolog" | "doctype" | "cdata" => Some(Self::Comment),
            "punctuation" => Some(Self::Punctuation),
            "property" | "tag" | "boolean" | "number" | "constant" | "symbol" | "deleted" => {
                Some(Self::Literal)
            }
            "selector" | "attr-name" | "string" | "char" | "builtin" => Some(Self::String),
            "operator" | "entity" | "url" => Some(Self::Operator),
            "atrule" | "attr-value" | "keyword" | "function" => Some(Self::Keyword),
            "class-name" => Some(Self::ClassName),
            "inserted" => Some(Self::Inserted),
            _ => None,
        }
    }

    /// Encodes this token kind into the `EntityType::Colorized` payload string
    /// (tdesktop encodes single UTF-16 code unit `QChar(ushort(color))` for `1..=8`).
    #[must_use]
    pub fn to_entity_data(self) -> String {
        let ch = char::from(self.color_index());
        let mut s = String::with_capacity(1);
        s.push(ch);
        s
    }

    /// Decodes a `EntityType::Colorized` payload string (accepts both tdesktop's raw `char(1..=8)`
    /// format and ASCII `"1"`..`"8"`).
    #[must_use]
    pub fn from_entity_data(data: &str) -> Option<Self> {
        let mut chars = data.chars();
        let first = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let code = first as u32;
        if (1..=8).contains(&code) {
            return Self::from_color_index(code as u8);
        }
        if ('1'..='8').contains(&first) {
            return Self::from_color_index((first as u8) - b'0');
        }
        None
    }
}

/// One highlighted UTF-8 byte span inside a source code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedSpan {
    /// UTF-8 byte range inside the code block.
    pub range: Range<usize>,
    /// Assigned syntax token category.
    pub kind: SyntaxTokenKind,
}

/// Resolves common language aliases to canonical Prism grammar identifiers
/// (`LookupAlias` in `spellcheck_highlight_syntax.cpp:73-79` plus standard Prism aliases).
#[must_use]
pub fn lookup_language_alias(language: &str) -> &'static str {
    match language.trim().to_ascii_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "py" | "python" | "python3" | "py3" => "python",
        "js" | "javascript" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "typescript" | "tsx" | "mts" => "typescript",
        "c" | "h" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hxx" | "hh" | "objc" | "objective-c" => "cpp",
        "cs" | "csharp" | "c#" => "csharp",
        "java" => "java",
        "kt" | "kotlin" | "kts" => "kotlin",
        "swift" => "swift",
        "go" | "golang" => "go",
        "rb" | "ruby" => "ruby",
        "php" => "php",
        "sh" | "bash" | "zsh" | "shell" | "shell-session" | "console" => "bash",
        "ps1" | "powershell" | "pwsh" => "powershell",
        "sql" | "mysql" | "pgsql" | "postgresql" | "sqlite" => "sql",
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "toml" | "ini" | "cfg" | "conf" => "toml",
        "html" | "htm" | "xml" | "svg" | "xhtml" | "markup" => "markup",
        "css" | "scss" | "sass" | "less" => "css",
        "diff" | "patch" => "diff",
        "md" | "markdown" => "markdown",
        "lua" => "lua",
        "zig" => "zig",
        "dart" => "dart",
        "dockerfile" | "docker" => "docker",
        _ => "",
    }
}

/// Returns `true` if `language` has a built-in highlighter grammar.
#[must_use]
pub fn is_supported_language(language: &str) -> bool {
    !lookup_language_alias(language).is_empty()
}

/// Language-specific lexer configuration derived from Prism grammars.
#[derive(Debug, Clone, Copy)]
struct LangSpec {
    canonical: &'static str,
    slash_comments: bool,
    block_comments: bool,
    hash_comments: bool,
    dash_comments: bool,
    single_quote_string: bool,
    backtick_string: bool,
    triple_quotes: bool,
    case_insensitive_keywords: bool,
    keywords: &'static [&'static str],
    builtins: &'static [&'static str],
    constants: &'static [&'static str],
    type_introducers: &'static [&'static str],
}

const RUST_SPEC: LangSpec = LangSpec {
    canonical: "rust",
    slash_comments: true,
    block_comments: true,
    hash_comments: false,
    dash_comments: false,
    single_quote_string: false,
    backtick_string: false,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "as",
        "async",
        "await",
        "break",
        "const",
        "continue",
        "crate",
        "dyn",
        "else",
        "enum",
        "extern",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "match",
        "mod",
        "move",
        "mut",
        "pub",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "type",
        "unsafe",
        "use",
        "where",
        "while",
        "macro_rules",
    ],
    builtins: &[
        "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
        "u64", "u128", "usize", "f32", "f64", "String", "Vec", "Option", "Result", "Box", "Arc",
        "Rc", "HashMap", "BTreeMap", "HashSet",
    ],
    constants: &["true", "false", "Some", "None", "Ok", "Err"],
    type_introducers: &["struct", "enum", "trait", "type", "impl"],
};

const PYTHON_SPEC: LangSpec = LangSpec {
    canonical: "python",
    slash_comments: false,
    block_comments: false,
    hash_comments: true,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: false,
    triple_quotes: true,
    case_insensitive_keywords: false,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
        "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    builtins: &[
        "abs",
        "all",
        "any",
        "bool",
        "bytes",
        "dict",
        "enumerate",
        "filter",
        "float",
        "int",
        "isinstance",
        "len",
        "list",
        "map",
        "max",
        "min",
        "open",
        "print",
        "range",
        "repr",
        "reversed",
        "set",
        "sorted",
        "str",
        "sum",
        "super",
        "tuple",
        "type",
        "zip",
        "self",
        "cls",
    ],
    constants: &["True", "False", "None"],
    type_introducers: &["class"],
};

const JS_SPEC: LangSpec = LangSpec {
    canonical: "javascript",
    slash_comments: true,
    block_comments: true,
    hash_comments: false,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: true,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "package",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    builtins: &[
        "Array", "Boolean", "Date", "Error", "Function", "JSON", "Map", "Math", "Number", "Object",
        "Promise", "RegExp", "Set", "String", "Symbol", "console", "document", "window", "any",
        "boolean", "never", "number", "string", "unknown",
    ],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    type_introducers: &["class", "interface", "type", "extends", "implements"],
};

const CPP_SPEC: LangSpec = LangSpec {
    canonical: "cpp",
    slash_comments: true,
    block_comments: true,
    hash_comments: false,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: false,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "alignas",
        "alignof",
        "auto",
        "break",
        "case",
        "catch",
        "class",
        "co_await",
        "co_return",
        "co_yield",
        "concept",
        "const",
        "consteval",
        "constexpr",
        "constinit",
        "continue",
        "decltype",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "explicit",
        "export",
        "extern",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "operator",
        "private",
        "protected",
        "public",
        "register",
        "requires",
        "return",
        "sizeof",
        "static",
        "static_assert",
        "static_cast",
        "dynamic_cast",
        "reinterpret_cast",
        "const_cast",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "typedef",
        "typeid",
        "typename",
        "union",
        "using",
        "virtual",
        "volatile",
        "while",
    ],
    builtins: &[
        "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void",
        "wchar_t", "size_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t",
        "uint32_t", "uint64_t", "std", "string", "vector",
    ],
    constants: &["true", "false", "nullptr", "NULL"],
    type_introducers: &["class", "struct", "enum", "union", "typename", "namespace"],
};

const GO_SPEC: LangSpec = LangSpec {
    canonical: "go",
    slash_comments: true,
    block_comments: true,
    hash_comments: false,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: true,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    builtins: &[
        "any",
        "append",
        "bool",
        "byte",
        "cap",
        "close",
        "comparable",
        "copy",
        "delete",
        "error",
        "float32",
        "float64",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "len",
        "make",
        "new",
        "panic",
        "recover",
        "rune",
        "string",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uintptr",
    ],
    constants: &["true", "false", "nil", "iota"],
    type_introducers: &["type", "struct", "interface"],
};

const JAVA_CS_SPEC: LangSpec = LangSpec {
    canonical: "java",
    slash_comments: true,
    block_comments: true,
    hash_comments: false,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: false,
    triple_quotes: true,
    case_insensitive_keywords: false,
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "for",
        "foreach",
        "fun",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "internal",
        "is",
        "let",
        "namespace",
        "new",
        "object",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "return",
        "sealed",
        "static",
        "struct",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "try",
        "using",
        "val",
        "var",
        "virtual",
        "void",
        "when",
        "while",
        "yield",
    ],
    builtins: &[
        "boolean", "bool", "byte", "char", "decimal", "double", "float", "int", "long", "short",
        "string", "String", "List", "Map", "Set", "Object", "Int", "Unit",
    ],
    constants: &["true", "false", "null", "nil"],
    type_introducers: &["class", "interface", "enum", "struct", "record", "extends"],
};

const BASH_SPEC: LangSpec = LangSpec {
    canonical: "bash",
    slash_comments: false,
    block_comments: false,
    hash_comments: true,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: true,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "readonly", "return", "select", "then", "until", "while",
    ],
    builtins: &[
        "alias", "bg", "cd", "echo", "eval", "exec", "exit", "fg", "git", "grep", "jobs", "kill",
        "ls", "mkdir", "printf", "pwd", "read", "rm", "sed", "set", "shift", "source", "sudo",
        "test", "trap", "unset", "cargo", "curl",
    ],
    constants: &["true", "false"],
    type_introducers: &[],
};

const SQL_SPEC: LangSpec = LangSpec {
    canonical: "sql",
    slash_comments: false,
    block_comments: true,
    hash_comments: false,
    dash_comments: true,
    single_quote_string: true,
    backtick_string: true,
    triple_quotes: false,
    case_insensitive_keywords: true,
    keywords: &[
        "SELECT",
        "FROM",
        "WHERE",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "CREATE",
        "ALTER",
        "DROP",
        "TABLE",
        "INDEX",
        "VIEW",
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "OUTER",
        "FULL",
        "CROSS",
        "ON",
        "AS",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "LIKE",
        "BETWEEN",
        "EXISTS",
        "GROUP",
        "BY",
        "HAVING",
        "ORDER",
        "ASC",
        "DESC",
        "LIMIT",
        "OFFSET",
        "UNION",
        "ALL",
        "DISTINCT",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "PRIMARY",
        "KEY",
        "FOREIGN",
        "REFERENCES",
        "UNIQUE",
        "DEFAULT",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "WITH",
        "RETURNING",
    ],
    builtins: &[
        "COUNT",
        "SUM",
        "AVG",
        "MIN",
        "MAX",
        "COALESCE",
        "NULLIF",
        "CAST",
        "NOW",
        "INT",
        "INTEGER",
        "BIGINT",
        "TEXT",
        "VARCHAR",
        "BOOLEAN",
        "BLOB",
        "TIMESTAMP",
        "JSON",
        "JSONB",
    ],
    constants: &["TRUE", "FALSE", "NULL"],
    type_introducers: &[],
};

const RUBY_PHP_LUA_SPEC: LangSpec = LangSpec {
    canonical: "ruby",
    slash_comments: true,
    block_comments: true,
    hash_comments: true,
    dash_comments: false,
    single_quote_string: true,
    backtick_string: true,
    triple_quotes: false,
    case_insensitive_keywords: false,
    keywords: &[
        "and",
        "begin",
        "break",
        "case",
        "class",
        "def",
        "do",
        "echo",
        "elif",
        "else",
        "elseif",
        "elsif",
        "end",
        "ensure",
        "for",
        "foreach",
        "function",
        "if",
        "in",
        "local",
        "module",
        "namespace",
        "new",
        "next",
        "not",
        "or",
        "private",
        "protected",
        "public",
        "repeat",
        "require",
        "rescue",
        "return",
        "self",
        "static",
        "then",
        "try",
        "catch",
        "until",
        "use",
        "when",
        "while",
        "yield",
    ],
    builtins: &[
        "puts",
        "print",
        "require_relative",
        "array",
        "strlen",
        "pairs",
        "ipairs",
    ],
    constants: &["true", "false", "nil", "null", "TRUE", "FALSE", "NULL"],
    type_introducers: &["class", "module"],
};

fn spec_for_language(canonical: &str) -> Option<&'static LangSpec> {
    match canonical {
        "rust" | "zig" => Some(&RUST_SPEC),
        "python" => Some(&PYTHON_SPEC),
        "javascript" | "typescript" | "dart" => Some(&JS_SPEC),
        "c" | "cpp" => Some(&CPP_SPEC),
        "go" => Some(&GO_SPEC),
        "java" | "csharp" | "kotlin" | "swift" => Some(&JAVA_CS_SPEC),
        "bash" | "powershell" | "docker" => Some(&BASH_SPEC),
        "sql" => Some(&SQL_SPEC),
        "ruby" | "php" | "lua" => Some(&RUBY_PHP_LUA_SPEC),
        _ => None,
    }
}

/// Tokenizes `code` for `language` into non-overlapping UTF-8 [`HighlightedSpan`]s.
///
/// Unknown or empty languages return an empty `Vec`.
#[must_use]
pub fn highlight_code_spans(code: &str, language: &str) -> Vec<HighlightedSpan> {
    if code.is_empty() {
        return Vec::new();
    }
    let canonical = lookup_language_alias(language);
    if canonical.is_empty() {
        return Vec::new();
    }

    match canonical {
        "diff" => highlight_diff(code),
        "json" => highlight_json(code),
        "markup" => highlight_markup(code),
        "css" => highlight_css(code),
        "yaml" | "toml" => highlight_config(code, canonical == "yaml"),
        "markdown" => highlight_markdown_code(code),
        _ => {
            if let Some(spec) = spec_for_language(canonical) {
                highlight_programming_language(code, spec)
            } else {
                Vec::new()
            }
        }
    }
}

/// Tokenizes `code` for `language` into UTF-16 [`EntityInText`] (`EntityType::Colorized`)
/// entities relative to the start of `code`.
#[must_use]
pub fn highlight_code_entities(code: &str, language: &str) -> EntitiesInText {
    let spans = highlight_code_spans(code, language);
    if spans.is_empty() {
        return Vec::new();
    }
    let mapper = OffsetMapper::new(code);
    let mut entities = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some((off, len)) = mapper.utf8_range_to_utf16(span.range)
            && len > 0
            && let (Ok(off_i32), Ok(len_i32)) = (i32::try_from(off), i32::try_from(len))
        {
            entities.push(EntityInText::with_data(
                EntityType::Colorized,
                off_i32,
                len_i32,
                span.kind.to_entity_data(),
            ));
        }
    }
    entities
}

#[derive(Default)]
struct HighlightCache {
    map: HashMap<u64, EntitiesInText>,
    order: VecDeque<u64>,
}

fn cache_key(code: &str, language: &str) -> u64 {
    // Deterministic FNV-1a 64-bit hash for fast in-process cache lookup.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in language.bytes() {
        hash ^= u64::from(b.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash ^= 0xff;
    hash = hash.wrapping_mul(0x0100_0000_01b3);
    for b in code.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn highlight_cache() -> &'static Mutex<HighlightCache> {
    static CACHE: OnceLock<Mutex<HighlightCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HighlightCache::default()))
}

fn highlight_cached_entities(code: &str, language: &str) -> EntitiesInText {
    let key = cache_key(code, language);
    {
        let cache = highlight_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.map.get(&key) {
            return hit.clone();
        }
    }
    let computed = highlight_code_entities(code, language);
    let mut cache = highlight_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let std::collections::hash_map::Entry::Vacant(entry) = cache.map.entry(key) {
        entry.insert(computed.clone());
        cache.order.push_back(key);
        while cache.order.len() > HIGHLIGHT_CACHE_LIMIT {
            if let Some(old) = cache.order.pop_front() {
                cache.map.remove(&old);
            }
        }
    }
    computed
}

/// Highlights every `EntityType::Pre` block with a non-empty language tag inside `text`,
/// inserting `EntityType::Colorized` entities (shifted to each `<pre>` block's UTF-16 offset).
///
/// Mirrors `Spellchecker::TryHighlightSyntax` in `spellcheck_highlight_syntax.cpp:280-315`.
/// Returns the number of `Colorized` entities inserted.
pub fn try_highlight_syntax(text: &mut TextWithEntities) -> usize {
    let pre_blocks: Vec<(i32, i32, String)> = text
        .entities
        .iter()
        .filter_map(|e| {
            let lang = e.pre_language()?;
            Some((e.offset(), e.length(), lang.to_owned()))
        })
        .collect();

    if pre_blocks.is_empty() {
        return 0;
    }

    let mut inserted = 0usize;
    let mut additions = Vec::new();

    for (offset, length, language) in pre_blocks {
        // Avoid duplicating colorized entities if already highlighted at this `<pre>` range.
        let end = offset.saturating_add(length);
        let already_colored = text.entities.iter().any(|e| {
            e.entity_type() == EntityType::Colorized && e.offset() >= offset && e.offset() < end
        });
        if already_colored {
            continue;
        }
        let Some(slice) = text.slice_entity(&EntityInText::new(EntityType::Pre, offset, length))
        else {
            continue;
        };
        let local_entities = highlight_cached_entities(slice, &language);
        for mut entity in local_entities {
            if entity.offset().saturating_add(entity.length()) > length {
                break;
            }
            entity.shift_right(offset);
            additions.push(entity);
            inserted += 1;
        }
    }

    if !additions.is_empty() {
        text.entities.extend(additions);
        text.sort_entities();
    }
    inserted
}

// =============================================================================
// Lexers for specific language families
// =============================================================================

fn push_span(out: &mut Vec<HighlightedSpan>, start: usize, end: usize, kind: SyntaxTokenKind) {
    if end > start {
        out.push(HighlightedSpan {
            range: start..end,
            kind,
        });
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_punctuation(b: u8) -> bool {
    matches!(
        b,
        b'{' | b'}' | b'[' | b']' | b'(' | b')' | b',' | b';' | b'.' | b':'
    )
}

fn is_operator(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'='
            | b'!'
            | b'<'
            | b'>'
            | b'&'
            | b'|'
            | b'^'
            | b'~'
            | b'?'
    )
}

fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        if bytes[i] == b'\n' && quote != b'`' {
            return i;
        }
        i += 1;
    }
    bytes.len()
}

fn scan_triple_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 3;
    while i + 2 < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == quote && bytes[i + 1] == quote && bytes[i + 2] == quote {
            return i + 3;
        }
        i += 1;
    }
    bytes.len()
}

fn scan_number(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    if bytes[i] == b'0' && i + 1 < bytes.len() {
        match bytes[i + 1] {
            b'x' | b'X' => {
                i += 2;
                while i < bytes.len() && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
                return scan_number_suffix(bytes, i);
            }
            b'b' | b'B' => {
                i += 2;
                while i < bytes.len() && matches!(bytes[i], b'0' | b'1' | b'_') {
                    i += 1;
                }
                return scan_number_suffix(bytes, i);
            }
            b'o' | b'O' => {
                i += 2;
                while i < bytes.len() && ((b'0'..=b'7').contains(&bytes[i]) || bytes[i] == b'_') {
                    i += 1;
                }
                return scan_number_suffix(bytes, i);
            }
            _ => {}
        }
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < bytes.len() && matches!(bytes[j], b'+' | b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            i = j + 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
    }
    scan_number_suffix(bytes, i)
}

fn scan_number_suffix(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i
}

fn matches_word_list(word: &str, list: &[&str], case_insensitive: bool) -> bool {
    if case_insensitive {
        list.iter().any(|kw| kw.eq_ignore_ascii_case(word))
    } else {
        list.contains(&word)
    }
}

fn highlight_programming_language(code: &str, spec: &LangSpec) -> Vec<HighlightedSpan> {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut expect_type_name = false;

    while i < len {
        let b = bytes[i];

        // Skip whitespace (preserving `expect_type_name` across spaces).
        if b.is_ascii_whitespace() {
            if b == b'\n' {
                expect_type_name = false;
            }
            i += 1;
            continue;
        }

        // Line comments: `//`, `#`, `--`
        if spec.slash_comments && b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push_span(&mut out, i, end, SyntaxTokenKind::Comment);
            i = end;
            continue;
        }
        if spec.dash_comments && b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push_span(&mut out, i, end, SyntaxTokenKind::Comment);
            i = end;
            continue;
        }
        if spec.hash_comments && b == b'#' {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push_span(&mut out, i, end, SyntaxTokenKind::Comment);
            i = end;
            continue;
        }

        // Block comments: `/* ... */`
        if spec.block_comments && b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let mut j = i + 2;
            while j + 1 < len {
                if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    j += 2;
                    break;
                }
                j += 1;
            }
            if j + 1 == len {
                j = len;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Comment);
            i = j;
            continue;
        }

        // C/C++ preprocessor directive (`#include`, `#define`, etc.)
        if spec.canonical == "cpp" && b == b'#' {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_whitespace() && bytes[j] != b'\n' {
                j += 1;
            }
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Keyword);
            i = j;
            continue;
        }

        // Rust attribute (`#[...]`) or Python/Java decorator (`@ident`)
        if b == b'@' && i + 1 < len && is_ident_start(bytes[i + 1]) {
            let mut j = i + 2;
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Keyword);
            i = j;
            continue;
        }

        // Shell / PHP variable (`$VAR`, `${VAR}`)
        if matches!(spec.canonical, "bash" | "ruby") && b == b'$' {
            let mut j = i + 1;
            if j < len && bytes[j] == b'{' {
                j += 1;
                while j < len && bytes[j] != b'}' && bytes[j] != b'\n' {
                    j += 1;
                }
                if j < len && bytes[j] == b'}' {
                    j += 1;
                }
            } else {
                while j < len && is_ident_continue(bytes[j]) {
                    j += 1;
                }
            }
            if j > i + 1 {
                push_span(&mut out, i, j, SyntaxTokenKind::Literal);
                i = j;
                continue;
            }
        }

        // Triple-quoted strings (`"""..."""` or `'''...'''`)
        if spec.triple_quotes
            && (b == b'"' || (spec.single_quote_string && b == b'\''))
            && i + 2 < len
            && bytes[i + 1] == b
            && bytes[i + 2] == b
        {
            let end = scan_triple_quoted(bytes, i, b);
            push_span(&mut out, i, end, SyntaxTokenKind::String);
            i = end;
            expect_type_name = false;
            continue;
        }

        // Rust raw strings `r#"..."#` / `r"..."`
        if spec.canonical == "rust"
            && b == b'r'
            && i + 1 < len
            && matches!(bytes[i + 1], b'#' | b'"')
        {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while j < len && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < len && bytes[j] == b'"' {
                j += 1;
                let mut end = len;
                while j < len {
                    if bytes[j] == b'"' {
                        let mut h = 0usize;
                        while h < hashes && j + 1 + h < len && bytes[j + 1 + h] == b'#' {
                            h += 1;
                        }
                        if h == hashes {
                            end = j + 1 + h;
                            break;
                        }
                    }
                    j += 1;
                }
                push_span(&mut out, i, end, SyntaxTokenKind::String);
                i = end;
                expect_type_name = false;
                continue;
            }
        }

        // Double-quoted strings
        if b == b'"' {
            let end = scan_quoted(bytes, i, b'"');
            push_span(&mut out, i, end, SyntaxTokenKind::String);
            i = end;
            expect_type_name = false;
            continue;
        }

        // Single-quoted strings / Rust char literals vs lifetimes
        if b == b'\'' {
            if spec.single_quote_string {
                let end = scan_quoted(bytes, i, b'\'');
                push_span(&mut out, i, end, SyntaxTokenKind::String);
                i = end;
                expect_type_name = false;
                continue;
            } else if spec.canonical == "rust" {
                // Distinguish `'a` lifetime from `'x'` / `'\n'` char literal.
                if i + 2 < len && bytes[i + 1] != b'\\' && bytes[i + 2] == b'\'' {
                    push_span(&mut out, i, i + 3, SyntaxTokenKind::String);
                    i += 3;
                    continue;
                } else if i + 3 < len && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                    push_span(&mut out, i, i + 4, SyntaxTokenKind::String);
                    i += 4;
                    continue;
                } else if i + 1 < len && is_ident_start(bytes[i + 1]) {
                    let mut j = i + 2;
                    while j < len && is_ident_continue(bytes[j]) {
                        j += 1;
                    }
                    push_span(&mut out, i, j, SyntaxTokenKind::Literal);
                    i = j;
                    continue;
                }
            }
        }

        // Backtick strings (JS template literals, Go raw strings, SQL/Bash)
        if spec.backtick_string && b == b'`' {
            let end = scan_quoted(bytes, i, b'`');
            push_span(&mut out, i, end, SyntaxTokenKind::String);
            i = end;
            expect_type_name = false;
            continue;
        }

        // Numbers
        if b.is_ascii_digit() {
            let end = scan_number(bytes, i);
            push_span(&mut out, i, end, SyntaxTokenKind::Literal);
            i = end;
            expect_type_name = false;
            continue;
        }

        // Identifiers, keywords, builtins, constants, macros, functions, class names
        if is_ident_start(b) {
            let mut j = i + 1;
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            let word = &code[i..j];

            // Look ahead past spaces for `(` or `!` (Rust macro).
            let mut k = j;
            while k < len && bytes[k] == b' ' {
                k += 1;
            }
            let is_rust_macro = spec.canonical == "rust" && j < len && bytes[j] == b'!';
            let is_call = k < len && bytes[k] == b'(';

            if is_rust_macro {
                push_span(&mut out, i, j + 1, SyntaxTokenKind::Keyword);
                i = j + 1;
                expect_type_name = false;
                continue;
            }

            if matches_word_list(word, spec.keywords, spec.case_insensitive_keywords) {
                push_span(&mut out, i, j, SyntaxTokenKind::Keyword);
                expect_type_name =
                    matches_word_list(word, spec.type_introducers, spec.case_insensitive_keywords);
            } else if matches_word_list(word, spec.constants, spec.case_insensitive_keywords) {
                push_span(&mut out, i, j, SyntaxTokenKind::Literal);
                expect_type_name = false;
            } else if matches_word_list(word, spec.builtins, spec.case_insensitive_keywords) {
                push_span(&mut out, i, j, SyntaxTokenKind::String);
                expect_type_name = false;
            } else if expect_type_name {
                push_span(&mut out, i, j, SyntaxTokenKind::ClassName);
                expect_type_name = false;
            } else if is_call {
                push_span(&mut out, i, j, SyntaxTokenKind::Keyword);
                expect_type_name = false;
            } else if word.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && word.chars().any(|c| c.is_ascii_lowercase())
            {
                // PascalCase type/class identifier.
                push_span(&mut out, i, j, SyntaxTokenKind::ClassName);
            } else if word.len() >= 2
                && word
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
            {
                // SCREAMING_SNAKE_CASE constant.
                push_span(&mut out, i, j, SyntaxTokenKind::Literal);
            }

            i = j;
            continue;
        }

        // Operators (coalesce consecutive operator chars like `==`, `->`, `=>`, `::`).
        if is_operator(b) {
            let mut j = i + 1;
            while j < len && is_operator(bytes[j]) && j - i < 3 {
                j += 1;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Operator);
            i = j;
            continue;
        }

        // Punctuation
        if is_punctuation(b) {
            push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
            i += 1;
            continue;
        }

        // Advance cleanly over any UTF-8 multi-byte character.
        let ch_len = code[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    out
}

fn highlight_json(code: &str) -> Vec<HighlightedSpan> {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < len {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push_span(&mut out, i, end, SyntaxTokenKind::Comment);
            i = end;
            continue;
        }
        if b == b'"' {
            let end = scan_quoted(bytes, i, b'"');
            let mut k = end;
            while k < len && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            let kind = if k < len && bytes[k] == b':' {
                SyntaxTokenKind::Literal // JSON property key (Prism `"property" -> 3`)
            } else {
                SyntaxTokenKind::String
            };
            push_span(&mut out, i, end, kind);
            i = end;
            continue;
        }
        if b.is_ascii_digit() || (b == b'-' && i + 1 < len && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            if b == b'-' {
                i += 1;
            }
            let end = scan_number(bytes, i);
            push_span(&mut out, start, end, SyntaxTokenKind::Literal);
            i = end;
            continue;
        }
        if is_ident_start(b) {
            let mut j = i + 1;
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            let word = &code[i..j];
            if matches!(word, "true" | "false" | "null") {
                push_span(&mut out, i, j, SyntaxTokenKind::Literal);
            }
            i = j;
            continue;
        }
        if is_punctuation(b) {
            push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
            i += 1;
            continue;
        }
        let ch_len = code[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    out
}

fn highlight_diff(code: &str) -> Vec<HighlightedSpan> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in code.split_inclusive('\n') {
        let line_trimmed_len = line.trim_end_matches(['\r', '\n']).len();
        let end = offset + line_trimmed_len;
        if let Some(first) = line.as_bytes().first() {
            match *first {
                b'+' if !line.starts_with("+++") => {
                    push_span(&mut out, offset, end, SyntaxTokenKind::Inserted);
                }
                b'-' if !line.starts_with("---") => {
                    push_span(&mut out, offset, end, SyntaxTokenKind::Literal);
                }
                b'@' if line.starts_with("@@") => {
                    push_span(&mut out, offset, end, SyntaxTokenKind::Keyword);
                }
                b'+' | b'-' => {
                    push_span(&mut out, offset, end, SyntaxTokenKind::Comment);
                }
                _ => {}
            }
        }
        offset += line.len();
    }
    out
}

fn highlight_markup(code: &str) -> Vec<HighlightedSpan> {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < len {
        if bytes[i] == b'<' {
            // HTML comment `<!-- ... -->`
            if code[i..].starts_with("<!--") {
                let end = code[i + 4..].find("-->").map_or(len, |p| i + 4 + p + 3);
                push_span(&mut out, i, end, SyntaxTokenKind::Comment);
                i = end;
                continue;
            }
            // Opening `<` or `</`
            let tag_open_start = i;
            i += 1;
            if i < len && matches!(bytes[i], b'/' | b'!' | b'?') {
                i += 1;
            }
            push_span(&mut out, tag_open_start, i, SyntaxTokenKind::Punctuation);

            // Tag name
            let name_start = i;
            while i < len && (is_ident_continue(bytes[i]) || matches!(bytes[i], b'-' | b':')) {
                i += 1;
            }
            push_span(&mut out, name_start, i, SyntaxTokenKind::Literal);

            // Attributes until `>`
            while i < len && bytes[i] != b'>' {
                if bytes[i].is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'>' {
                    push_span(&mut out, i, i + 2, SyntaxTokenKind::Punctuation);
                    i += 2;
                    break;
                }
                if bytes[i] == b'=' {
                    push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
                    i += 1;
                    continue;
                }
                if matches!(bytes[i], b'"' | b'\'') {
                    let end = scan_quoted(bytes, i, bytes[i]);
                    push_span(&mut out, i, end, SyntaxTokenKind::Keyword);
                    i = end;
                    continue;
                }
                if is_ident_start(bytes[i]) {
                    let attr_start = i;
                    while i < len
                        && (is_ident_continue(bytes[i]) || matches!(bytes[i], b'-' | b':'))
                    {
                        i += 1;
                    }
                    push_span(&mut out, attr_start, i, SyntaxTokenKind::String);
                    continue;
                }
                i += 1;
            }
            if i < len && bytes[i] == b'>' {
                push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
                i += 1;
            }
            continue;
        }
        let ch_len = code[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    out
}

fn highlight_css(code: &str) -> Vec<HighlightedSpan> {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut in_block = false;

    while i < len {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let end = code[i + 2..].find("*/").map_or(len, |p| i + 2 + p + 2);
            push_span(&mut out, i, end, SyntaxTokenKind::Comment);
            i = end;
            continue;
        }
        if b == b'{' {
            in_block = true;
            push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
            i += 1;
            continue;
        }
        if b == b'}' {
            in_block = false;
            push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
            i += 1;
            continue;
        }
        if matches!(b, b'"' | b'\'') {
            let end = scan_quoted(bytes, i, b);
            push_span(&mut out, i, end, SyntaxTokenKind::String);
            i = end;
            continue;
        }
        if b == b'@' {
            let mut j = i + 1;
            while j < len && (is_ident_continue(bytes[j]) || bytes[j] == b'-') {
                j += 1;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Keyword);
            i = j;
            continue;
        }
        if b == b'#' && in_block {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            push_span(&mut out, i, j, SyntaxTokenKind::Literal);
            i = j;
            continue;
        }
        if b.is_ascii_digit() {
            let mut end = scan_number(bytes, i);
            if end < len && bytes[end] == b'%' {
                end += 1;
            }
            push_span(&mut out, i, end, SyntaxTokenKind::Literal);
            i = end;
            continue;
        }
        if is_ident_start(b) || b == b'.' || b == b'#' || b == b'-' {
            let mut j = i + 1;
            while j < len && (is_ident_continue(bytes[j]) || bytes[j] == b'-') {
                j += 1;
            }
            let kind = if !in_block {
                SyntaxTokenKind::String // selector -> 4
            } else {
                let mut k = j;
                while k < len && bytes[k] == b' ' {
                    k += 1;
                }
                if k < len && bytes[k] == b':' {
                    SyntaxTokenKind::Literal // property -> 3
                } else {
                    SyntaxTokenKind::Keyword // value/function -> 6
                }
            };
            push_span(&mut out, i, j, kind);
            i = j;
            continue;
        }
        if is_punctuation(b) {
            push_span(&mut out, i, i + 1, SyntaxTokenKind::Punctuation);
            i += 1;
            continue;
        }
        let ch_len = code[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    out
}

fn highlight_config(code: &str, is_yaml: bool) -> Vec<HighlightedSpan> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    let sep = if is_yaml { ':' } else { '=' };

    for line in code.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let content_end = offset + line.trim_end_matches(['\r', '\n']).len();

        if trimmed.starts_with('#') {
            push_span(
                &mut out,
                offset + lead,
                content_end,
                SyntaxTokenKind::Comment,
            );
        } else if !is_yaml && trimmed.starts_with('[') {
            push_span(
                &mut out,
                offset + lead,
                content_end,
                SyntaxTokenKind::ClassName,
            );
        } else if let Some(pos) = trimmed.find(sep) {
            let key_start = offset + lead;
            let key_end = key_start + pos;
            push_span(&mut out, key_start, key_end, SyntaxTokenKind::Literal);
            push_span(&mut out, key_end, key_end + 1, SyntaxTokenKind::Punctuation);
            let val_slice = &trimmed[pos + 1..].trim_end_matches(['\r', '\n']);
            let val_trimmed = val_slice.trim_start();
            let val_start = key_end + 1 + (val_slice.len() - val_trimmed.len());
            if !val_trimmed.is_empty() {
                if val_trimmed.starts_with('"') || val_trimmed.starts_with('\'') {
                    push_span(&mut out, val_start, content_end, SyntaxTokenKind::String);
                } else if matches!(val_trimmed, "true" | "false" | "null" | "~")
                    || val_trimmed
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_digit)
                {
                    push_span(&mut out, val_start, content_end, SyntaxTokenKind::Literal);
                } else {
                    push_span(&mut out, val_start, content_end, SyntaxTokenKind::String);
                }
            }
        }
        offset += line.len();
    }

    out
}

fn highlight_markdown_code(code: &str) -> Vec<HighlightedSpan> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in code.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let end = offset + trimmed.len();
        if trimmed.starts_with('#') {
            push_span(&mut out, offset, end, SyntaxTokenKind::Keyword);
        } else if trimmed.starts_with('>') {
            push_span(&mut out, offset, end, SyntaxTokenKind::Comment);
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            push_span(&mut out, offset, offset + 1, SyntaxTokenKind::Punctuation);
        }
        offset += line.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_prism_slots_match_tdesktop() {
        assert_eq!(lookup_language_alias("patch"), "diff");
        assert_eq!(lookup_language_alias("RS"), "rust");
        assert_eq!(lookup_language_alias("py"), "python");
        assert_eq!(lookup_language_alias("tsx"), "typescript");
        assert_eq!(lookup_language_alias("unknown_lang"), "");
        assert!(is_supported_language("rust"));
        assert!(!is_supported_language("brainfuck_custom"));

        assert_eq!(
            SyntaxTokenKind::from_prism_token("comment"),
            Some(SyntaxTokenKind::Comment)
        );
        assert_eq!(
            SyntaxTokenKind::from_prism_token("keyword"),
            Some(SyntaxTokenKind::Keyword)
        );
        assert_eq!(
            SyntaxTokenKind::from_prism_token("class-name"),
            Some(SyntaxTokenKind::ClassName)
        );
        assert_eq!(
            SyntaxTokenKind::from_prism_token("inserted"),
            Some(SyntaxTokenKind::Inserted)
        );
        for slot in 1..=8u8 {
            let kind = SyntaxTokenKind::from_color_index(slot).unwrap();
            let encoded = kind.to_entity_data();
            assert_eq!(SyntaxTokenKind::from_entity_data(&encoded), Some(kind));
            assert_eq!(
                SyntaxTokenKind::from_entity_data(&slot.to_string()),
                Some(kind)
            );
        }
    }

    #[test]
    fn highlights_rust_code_tokens() {
        let src = "// greet\npub fn hello(n: u32) -> String {\n    let x = 42;\n    format!(\"hi {n} {x}\")\n}";
        let spans = highlight_code_spans(src, "rust");
        let by_slice = |needle: &str| {
            spans
                .iter()
                .find(|s| &src[s.range.clone()] == needle)
                .map(|s| s.kind)
        };
        assert_eq!(by_slice("// greet"), Some(SyntaxTokenKind::Comment));
        assert_eq!(by_slice("pub"), Some(SyntaxTokenKind::Keyword));
        assert_eq!(by_slice("fn"), Some(SyntaxTokenKind::Keyword));
        assert_eq!(by_slice("hello"), Some(SyntaxTokenKind::Keyword));
        assert_eq!(by_slice("u32"), Some(SyntaxTokenKind::String));
        assert_eq!(by_slice("42"), Some(SyntaxTokenKind::Literal));
        assert_eq!(by_slice("format!"), Some(SyntaxTokenKind::Keyword));
        assert_eq!(by_slice("\"hi {n} {x}\""), Some(SyntaxTokenKind::String));
    }

    #[test]
    fn highlights_python_json_and_diff() {
        let py = "# comment\ndef add(a, b):\n    return True\n";
        let py_spans = highlight_code_spans(py, "py");
        assert!(
            py_spans
                .iter()
                .any(|s| &py[s.range.clone()] == "# comment" && s.kind == SyntaxTokenKind::Comment)
        );
        assert!(
            py_spans
                .iter()
                .any(|s| &py[s.range.clone()] == "def" && s.kind == SyntaxTokenKind::Keyword)
        );
        assert!(
            py_spans
                .iter()
                .any(|s| &py[s.range.clone()] == "True" && s.kind == SyntaxTokenKind::Literal)
        );

        let json = r#"{"name": "rustgram", "layer": 233, "ok": true}"#;
        let j_spans = highlight_code_spans(json, "json");
        assert!(j_spans.iter().any(|s| &json[s.range.clone()] == "\"name\"" && s.kind == SyntaxTokenKind::Literal));
        assert!(
            j_spans
                .iter()
                .any(|s| &json[s.range.clone()] == "\"rustgram\""
                    && s.kind == SyntaxTokenKind::String)
        );
        assert!(
            j_spans
                .iter()
                .any(|s| &json[s.range.clone()] == "233" && s.kind == SyntaxTokenKind::Literal)
        );

        let diff = "@@ -1,2 +1,2 @@\n-old line\n+new line\n";
        let d_spans = highlight_code_spans(diff, "patch");
        assert!(
            d_spans.iter().any(
                |s| &diff[s.range.clone()] == "-old line" && s.kind == SyntaxTokenKind::Literal
            )
        );
        assert!(
            d_spans
                .iter()
                .any(|s| &diff[s.range.clone()] == "+new line"
                    && s.kind == SyntaxTokenKind::Inserted)
        );
    }

    #[test]
    fn try_highlight_syntax_populates_utf16_colorized_entities_and_is_idempotent() {
        let prefix = "Hi 🚀\n";
        let code = "let ok = true;";
        let full = format!("{prefix}{code}");
        let pre_off = crate::utf16_len(prefix) as i32;
        let pre_len = crate::utf16_len(code) as i32;
        let mut msg = TextWithEntities::with_entities(
            full,
            vec![EntityInText::with_data(
                EntityType::Pre,
                pre_off,
                pre_len,
                "rs",
            )],
        );

        let count1 = try_highlight_syntax(&mut msg);
        assert!(count1 >= 3);
        // Verify UTF-16 offset accounts for surrogate-pair rocket emoji in prefix.
        let let_ent = msg
            .entities
            .iter()
            .find(|e| {
                e.entity_type() == EntityType::Colorized && msg.slice_entity(e) == Some("let")
            })
            .expect("let token highlighted");
        assert_eq!(let_ent.offset(), pre_off);
        assert_eq!(
            SyntaxTokenKind::from_entity_data(let_ent.data()),
            Some(SyntaxTokenKind::Keyword)
        );

        // Second call must be idempotent.
        let count2 = try_highlight_syntax(&mut msg);
        assert_eq!(count2, 0);
    }
}
