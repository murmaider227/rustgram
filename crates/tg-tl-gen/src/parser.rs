//! Parser for Telegram `.tl` schema files.
//!
//! Port of the `.tl` schema parser in `lib_tl/tl/generate_tl.py`.

use std::collections::HashSet;

use crate::ast::{Category, Definition, FlagInner, Param, ParamType, ParseError, Schema, TypeRef};
use crate::crc32::compute_constructor_id;

/// Built-in core type declarations skipped by the schema parser (same set as `codegen_scheme.py`).
const BUILTIN_SKIP_LINES: &[&str] = &[
    "int ? = Int;",
    "long ? = Long;",
    "double ? = Double;",
    "string ? = String;",
    "vector {t:Type} # [ t ] = Vector t;",
    "vector#1cb5c415 {t:Type} # [ t ] = Vector t;",
    "int128 4*[ int ] = Int128;",
    "int256 8*[ int ] = Int256;",
];

/// Built-in lowercase type names that are NOT bare constructor references.
fn is_builtin_lowercase(name: &str) -> bool {
    matches!(
        name,
        "int" | "long" | "double" | "string" | "bytes" | "int128" | "int256" | "true" | "#"
    )
}

/// Parses a `.tl` schema source string into a [`Schema`].
pub fn parse_schema(input: &str) -> Result<Schema, ParseError> {
    let mut definitions = Vec::new();
    let mut layer: Option<i32> = None;
    let mut category = Category::Type;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;

        // Check for `// LAYER N` marker before stripping comments.
        if let Some(comment_pos) = raw_line.find("//") {
            let comment = raw_line[comment_pos + 2..].trim();
            if let Some(rest) = comment.strip_prefix("LAYER ")
                && let Ok(n) = rest.trim().parse::<i32>()
            {
                layer = Some(n);
            }
        }

        let without_comment = match raw_line.find("//") {
            Some(pos) => &raw_line[..pos],
            None => raw_line,
        };
        let line = without_comment.trim();
        if line.is_empty() {
            continue;
        }

        if line == "---types---" {
            category = Category::Type;
            continue;
        }
        if line == "---functions---" {
            category = Category::Function;
            continue;
        }

        if BUILTIN_SKIP_LINES.contains(&line) {
            continue;
        }

        let def = parse_definition(line, category, line_no)?;
        definitions.push(def);
    }

    Ok(Schema { definitions, layer })
}

/// Parses a single non-empty, comment-stripped TL definition line.
fn parse_definition(
    line: &str,
    category: Category,
    line_no: usize,
) -> Result<Definition, ParseError> {
    let err = |msg: String| ParseError {
        line: line_no,
        message: msg,
    };

    let without_semi = line
        .strip_suffix(';')
        .ok_or_else(|| err(format!("missing trailing `;` in `{line}`")))?
        .trim();

    let (lhs, rhs) = without_semi
        .split_once('=')
        .ok_or_else(|| err(format!("missing `=` in `{line}`")))?;
    let lhs = lhs.trim();
    let rhs = rhs.trim();
    if rhs.is_empty() {
        return Err(err(format!("empty result type in `{line}`")));
    }

    let mut tokens = lhs.split_whitespace();
    let head = tokens
        .next()
        .ok_or_else(|| err(format!("missing constructor name in `{line}`")))?;

    let (full_name, explicit_id) = match head.split_once('#') {
        Some((n, hex)) => {
            if hex.is_empty() || hex.len() > 8 {
                return Err(err(format!("invalid constructor id `#{hex}` in `{line}`")));
            }
            let id = u32::from_str_radix(hex, 16)
                .map_err(|_| err(format!("non-hex constructor id `#{hex}` in `{line}`")))?;
            (n, Some(id))
        }
        None => (head, None),
    };

    let (namespace, name) = split_namespace(full_name, line_no)?;

    let mut generic_params: Vec<String> = Vec::new();
    let mut flag_fields: HashSet<String> = HashSet::new();
    let mut params: Vec<Param> = Vec::new();

    for tok in tokens {
        // Generic parameter declaration: `{X:Type}`
        if tok.starts_with('{') && tok.ends_with('}') {
            let inner = &tok[1..tok.len() - 1];
            let (gp_name, gp_kind) = inner
                .split_once(':')
                .ok_or_else(|| err(format!("malformed generic parameter `{tok}`")))?;
            if gp_kind != "Type" || gp_name.is_empty() {
                return Err(err(format!("unsupported generic parameter `{tok}`")));
            }
            generic_params.push(gp_name.to_string());
            continue;
        }

        let (pname, ptype_str) = tok
            .split_once(':')
            .ok_or_else(|| err(format!("expected `name:type` parameter, got `{tok}`")))?;
        if pname.is_empty() || ptype_str.is_empty() {
            return Err(err(format!("empty parameter name or type in `{tok}`")));
        }

        let ty = if ptype_str == "#" {
            flag_fields.insert(pname.to_string());
            ParamType::Flags
        } else if let Some((cond, inner_str)) = ptype_str.split_once('?') {
            let (flag_field, bit_str) = cond
                .split_once('.')
                .ok_or_else(|| err(format!("malformed flag condition `{cond}` in `{tok}`")))?;
            if !flag_fields.contains(flag_field) {
                return Err(err(format!(
                    "undeclared flag field `{flag_field}` in `{tok}`"
                )));
            }
            let bit: u32 = bit_str
                .parse()
                .map_err(|_| err(format!("invalid flag bit `{bit_str}` in `{tok}`")))?;
            if bit >= 32 {
                return Err(err(format!("flag bit {bit} out of range 0..32 in `{tok}`")));
            }
            let inner = if inner_str == "true" {
                FlagInner::True
            } else {
                FlagInner::Type(parse_type_ref(inner_str, &generic_params, line_no)?)
            };
            ParamType::Flag {
                flag_field: flag_field.to_string(),
                bit,
                inner,
            }
        } else {
            ParamType::Normal(parse_type_ref(ptype_str, &generic_params, line_no)?)
        };

        params.push(Param {
            name: pname.to_string(),
            ty,
        });
    }

    let result_type = parse_type_ref(rhs, &generic_params, line_no)?;

    let mut def = Definition {
        category,
        namespace,
        name,
        id: explicit_id.unwrap_or(0),
        explicit_id,
        generic_params,
        params,
        result_type,
        line: line_no,
    };

    if explicit_id.is_none() {
        def.id = compute_constructor_id(&def);
    }

    Ok(def)
}

/// Splits an optionally namespaced identifier (`ns.name` or `name`).
fn split_namespace(full: &str, line_no: usize) -> Result<(Option<String>, String), ParseError> {
    if full.is_empty() {
        return Err(ParseError {
            line: line_no,
            message: "empty identifier".to_string(),
        });
    }
    match full.rsplit_once('.') {
        Some((ns, name)) => {
            if ns.is_empty() || name.is_empty() || ns.contains('.') {
                return Err(ParseError {
                    line: line_no,
                    message: format!("invalid namespaced identifier `{full}`"),
                });
            }
            Ok((Some(ns.to_string()), name.to_string()))
        }
        None => Ok((None, full.to_string())),
    }
}

/// Parses a TL type expression such as `int`, `Vector<InputUser>`, `vector<future_salt>`,
/// `%InputPeer`, `!X`, `storage.FileType`, or `Vector<Vector<TlsBlock>>`.
pub(crate) fn parse_type_ref(
    raw: &str,
    generic_params: &[String],
    line_no: usize,
) -> Result<TypeRef, ParseError> {
    let err = |msg: String| ParseError {
        line: line_no,
        message: msg,
    };

    let mut s = raw.trim();
    if s.is_empty() {
        return Err(err("empty type reference".to_string()));
    }

    let mut generic_ref = false;
    if let Some(rest) = s.strip_prefix('!') {
        generic_ref = true;
        s = rest;
    }

    let mut explicit_bare = false;
    if let Some(rest) = s.strip_prefix('%') {
        explicit_bare = true;
        s = rest;
    }

    // Handle parameterized types like `Vector<T>` or `vector<T>`.
    if let Some(lt) = s.find('<') {
        if !s.ends_with('>') {
            return Err(err(format!("unclosed `<` in type `{raw}`")));
        }
        let outer = &s[..lt];
        let inner = &s[lt + 1..s.len() - 1];
        let (namespace, name) = split_namespace(outer, line_no)?;
        let bare = explicit_bare || name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        let arg = parse_type_ref(inner, generic_params, line_no)?;
        return Ok(TypeRef {
            namespace,
            name,
            bare,
            generic_ref,
            generic_arg: Some(Box::new(arg)),
        });
    }

    // Space-separated vector return syntax (e.g. `Vector T` in rare schemas)
    if let Some((outer, inner)) = s.split_once(' ') {
        let outer = outer.trim();
        let inner = inner.trim();
        let (namespace, name) = split_namespace(outer, line_no)?;
        let bare = explicit_bare || name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        let arg = parse_type_ref(inner, generic_params, line_no)?;
        return Ok(TypeRef {
            namespace,
            name,
            bare,
            generic_ref,
            generic_arg: Some(Box::new(arg)),
        });
    }

    let (namespace, name) = split_namespace(s, line_no)?;
    if generic_ref && !generic_params.iter().any(|g| g == &name) {
        return Err(err(format!(
            "undeclared generic type parameter `!{name}` in `{raw}`"
        )));
    }

    let bare = explicit_bare
        || (!is_builtin_lowercase(&name)
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()));

    Ok(TypeRef {
        namespace,
        name,
        bare,
        generic_ref,
        generic_arg: None,
    })
}
