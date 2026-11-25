//! Rust code generator for parsed `.tl` schemas.
//!
//! Port of code-generation concepts from `lib_tl/tl/generate_tl.py`, targeting idiomatic Rust
//! traits (`Serializable`, `Deserializable`, `Identifiable`, `RemoteCall`) and modules
//! (`types`, `enums`, `functions`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use crate::ast::{Category, Definition, FlagInner, Param, ParamType, Schema, TypeRef};

/// Configuration controlling how Rust code is generated for a schema.
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Module path prefix where `types`, `enums`, and `functions` live
    /// (e.g. `"crate"` for `api.tl` or `"crate::mtproto"` for `mtproto.tl`).
    pub module_prefix: String,
    /// Set of `(full_ctor_name, param_name)` pairs whose TL `string` parameter carries
    /// arbitrary binary bytes and should be generated as `Vec<u8>` instead of `String`.
    pub bytes_fields: HashSet<(String, String)>,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            module_prefix: "crate".to_string(),
            bytes_fields: HashSet::new(),
        }
    }
}

/// Rust keywords that can be escaped with `r#`.
const RAW_IDENT_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern", "false",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while",
    "abstract", "become", "box", "do", "final", "macro", "override", "priv", "try", "typeof",
    "unsized", "virtual", "yield", "gen",
];

/// Rust keywords that CANNOT be escaped with `r#` and must receive a trailing `_`.
const NON_RAW_KEYWORDS: &[&str] = &["self", "Self", "super", "crate"];

/// Converts a TL identifier segment (without namespace) to PascalCase while preserving
/// internal acronyms (e.g. `resPQ` -> `ResPQ`, `req_pq_multi` -> `ReqPqMulti`,
/// `server_DH_params_ok` -> `ServerDHParamsOk`).
pub fn to_pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for part in s.split(['_', '.']) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Escapes a Rust field identifier (snake_case, lowercasing any uppercase letters).
pub fn rust_field_ident(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if NON_RAW_KEYWORDS.contains(&lower.as_str()) {
        format!("{lower}_")
    } else if RAW_IDENT_KEYWORDS.contains(&lower.as_str()) {
        format!("r#{lower}")
    } else {
        lower
    }
}

/// Computes an enum variant name from the enum's short name and constructor's short name.
///
/// Strips the enum's PascalCase prefix when it leaves a valid uppercase-starting suffix
/// (e.g. `User` + `userEmpty` -> `Empty`, `User` + `user` -> `User`,
/// `InputPeer` + `inputPeerSelf` -> `Self_`).
pub fn enum_variant_name(enum_short: &str, ctor_short: &str) -> String {
    let ep = to_pascal_case(enum_short);
    let cp = to_pascal_case(ctor_short);
    let cand = if cp.starts_with(&ep) && cp.len() > ep.len() {
        let rest = &cp[ep.len()..];
        if rest.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            rest.to_string()
        } else {
            cp
        }
    } else {
        cp
    };
    if NON_RAW_KEYWORDS.contains(&cand.as_str())
        || RAW_IDENT_KEYWORDS.contains(&cand.as_str())
        || cand.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        format!("{cand}_")
    } else {
        cand
    }
}

/// Returns `true` if a definition's parameters contain any fields stored on the Rust struct
/// (i.e. anything other than `flags:#`).
fn has_struct_fields(def: &Definition) -> bool {
    def.params.iter().any(|p| !matches!(p.ty, ParamType::Flags))
}

/// Precomputed schema metadata used during code generation.
struct SchemaIndex<'a> {
    /// Result full name -> list of constructors belonging to that boxed enum.
    enums_by_result: BTreeMap<String, Vec<&'a Definition>>,
    /// Constructor full name -> definition (for resolving bare constructor refs like `future_salt`).
    ctors_by_full_name: HashMap<String, &'a Definition>,
    /// Strongly connected component ID for each recursive result type (only types in cycles).
    recursive_scc: HashMap<String, usize>,
}

impl<'a> SchemaIndex<'a> {
    fn build(schema: &'a Schema) -> Self {
        let mut enums_by_result: BTreeMap<String, Vec<&'a Definition>> = BTreeMap::new();
        let mut ctors_by_full_name: HashMap<String, &'a Definition> = HashMap::new();

        for def in &schema.definitions {
            if def.category == Category::Type {
                let res = def.result_type.full_name();
                if res == "Bool" {
                    continue;
                }
                enums_by_result.entry(res).or_default().push(def);
                ctors_by_full_name.insert(def.full_name(), def);
            }
        }

        // Build direct (non-Vec) dependency graph between result types to detect infinite-size cycles.
        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (res, ctors) in &enums_by_result {
            let entry = deps.entry(res.clone()).or_default();
            for ctor in ctors {
                for param in &ctor.params {
                    let ty = match &param.ty {
                        ParamType::Normal(t) => Some(t),
                        ParamType::Flag {
                            inner: FlagInner::Type(t),
                            ..
                        } => Some(t),
                        _ => None,
                    };
                    if let Some(t) = ty {
                        if t.is_vector() || t.is_builtin_scalar() || t.generic_ref {
                            continue;
                        }
                        let target = if t.bare {
                            if let Some(c) = ctors_by_full_name.get(&t.full_name()) {
                                c.result_type.full_name()
                            } else {
                                t.full_name()
                            }
                        } else {
                            t.full_name()
                        };
                        if enums_by_result.contains_key(&target) {
                            entry.insert(target);
                        }
                    }
                }
            }
        }

        let recursive_scc = find_recursive_sccs(&deps);

        Self {
            enums_by_result,
            ctors_by_full_name,
            recursive_scc,
        }
    }

    /// Returns `true` if a field of type `ty` inside a constructor of `owner_result`
    /// closes a recursive type cycle and therefore needs `Box<...>`.
    fn needs_box(&self, owner_result: &str, ty: &TypeRef) -> bool {
        if ty.is_vector() || ty.is_builtin_scalar() || ty.generic_ref {
            return false;
        }
        let Some(&owner_scc) = self.recursive_scc.get(owner_result) else {
            return false;
        };
        let target = if ty.bare {
            if let Some(c) = self.ctors_by_full_name.get(&ty.full_name()) {
                c.result_type.full_name()
            } else {
                ty.full_name()
            }
        } else {
            ty.full_name()
        };
        self.recursive_scc.get(&target) == Some(&owner_scc)
    }

    /// Resolves a bare constructor reference (either lowercase constructor name like `future_salt`
    /// or explicit `%ResultType` for a single-constructor result type) to its [`Definition`].
    fn resolve_bare_ctor(&self, ty: &TypeRef) -> Option<&'a Definition> {
        let full = ty.full_name();
        if let Some(&def) = self.ctors_by_full_name.get(&full) {
            return Some(def);
        }
        if let Some(ctors) = self.enums_by_result.get(&full)
            && ctors.len() == 1
        {
            return Some(ctors[0]);
        }
        None
    }
}

/// Tarjan's SCC algorithm; returns a map from node -> SCC id for all nodes in cycles.
fn find_recursive_sccs(deps: &BTreeMap<String, BTreeSet<String>>) -> HashMap<String, usize> {
    struct State<'a> {
        deps: &'a BTreeMap<String, BTreeSet<String>>,
        next_index: usize,
        indices: HashMap<String, usize>,
        lowlink: HashMap<String, usize>,
        on_stack: HashSet<String>,
        stack: Vec<String>,
        sccs: Vec<Vec<String>>,
    }

    fn strongconnect(v: &str, st: &mut State<'_>) {
        let v_idx = st.next_index;
        st.next_index += 1;
        st.indices.insert(v.to_string(), v_idx);
        st.lowlink.insert(v.to_string(), v_idx);
        st.stack.push(v.to_string());
        st.on_stack.insert(v.to_string());

        if let Some(neighbors) = st.deps.get(v) {
            for w in neighbors {
                if !st.indices.contains_key(w) {
                    strongconnect(w, st);
                    let lw = st.lowlink[w];
                    let lv = st.lowlink.get_mut(v).expect("visited");
                    if lw < *lv {
                        *lv = lw;
                    }
                } else if st.on_stack.contains(w) {
                    let iw = st.indices[w];
                    let lv = st.lowlink.get_mut(v).expect("visited");
                    if iw < *lv {
                        *lv = iw;
                    }
                }
            }
        }

        if st.lowlink[v] == st.indices[v] {
            let mut scc = Vec::new();
            while let Some(w) = st.stack.pop() {
                st.on_stack.remove(&w);
                let done = w == v;
                scc.push(w);
                if done {
                    break;
                }
            }
            let is_cycle = scc.len() > 1
                || (scc.len() == 1 && st.deps.get(&scc[0]).is_some_and(|ns| ns.contains(&scc[0])));
            if is_cycle {
                st.sccs.push(scc);
            }
        }
    }

    let mut st = State {
        deps,
        next_index: 0,
        indices: HashMap::new(),
        lowlink: HashMap::new(),
        on_stack: HashSet::new(),
        stack: Vec::new(),
        sccs: Vec::new(),
    };

    for node in deps.keys() {
        if !st.indices.contains_key(node) {
            strongconnect(node, &mut st);
        }
    }

    let mut result = HashMap::new();
    for (scc_id, members) in st.sccs.into_iter().enumerate() {
        for m in members {
            result.insert(m, scc_id);
        }
    }
    result
}

/// Formats the Rust type path for a [`TypeRef`].
fn rust_type_path(
    ty: &TypeRef,
    def: &Definition,
    param_name: Option<&str>,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
) -> String {
    if ty.generic_ref || def.generic_params.iter().any(|g| g == &ty.name) {
        return ty.name.clone();
    }

    if ty.is_vector() {
        let inner = ty
            .generic_arg
            .as_deref()
            .map(|a| rust_type_path(a, def, None, index, config))
            .unwrap_or_else(|| "crate::RawObject".to_string());
        return format!("Vec<{inner}>");
    }

    if ty.namespace.is_none() {
        match ty.name.as_str() {
            "int" => return "i32".to_string(),
            "long" => return "i64".to_string(),
            "double" => return "f64".to_string(),
            "bytes" => return "Vec<u8>".to_string(),
            "string" => {
                if let Some(pn) = param_name
                    && config
                        .bytes_fields
                        .contains(&(def.full_name(), pn.to_string()))
                {
                    return "Vec<u8>".to_string();
                }
                return "String".to_string();
            }
            "int128" => return "[u8; 16]".to_string(),
            "int256" => return "[u8; 32]".to_string(),
            "Bool" => return "bool".to_string(),
            "Object" => return "crate::RawObject".to_string(),
            _ => {}
        }
    }

    let prefix = &config.module_prefix;
    if ty.bare
        && let Some(ctor) = index.resolve_bare_ctor(ty)
    {
        return type_struct_path(prefix, ctor);
    }

    enum_type_path(prefix, ty.namespace.as_deref(), &ty.name)
}

fn type_struct_path(prefix: &str, ctor: &Definition) -> String {
    let name = to_pascal_case(&ctor.name);
    match &ctor.namespace {
        Some(ns) => format!("{prefix}::types::{ns}::{name}"),
        None => format!("{prefix}::types::{name}"),
    }
}

fn enum_type_path(prefix: &str, ns: Option<&str>, short_name: &str) -> String {
    let name = to_pascal_case(short_name);
    match ns {
        Some(ns) => format!("{prefix}::enums::{ns}::{name}"),
        None => format!("{prefix}::enums::{name}"),
    }
}

fn func_struct_path(prefix: &str, func: &Definition) -> String {
    let name = to_pascal_case(&func.name);
    match &func.namespace {
        Some(ns) => format!("{prefix}::functions::{ns}::{name}"),
        None => format!("{prefix}::functions::{name}"),
    }
}

/// Emits serialization code for a value expression `expr` of TL type `ty`.
fn emit_serialize_expr(ty: &TypeRef, expr: &str, buf_var: &str, out: &mut String, indent: &str) {
    if ty.is_vector() && ty.bare {
        // Bare `vector<T>` (no 0x1cb5c415 constructor ID).
        let _ = writeln!(out, "{indent}crate::serialize_bare_vec({expr}, {buf_var});");
    } else {
        let _ = writeln!(
            out,
            "{indent}crate::Serializable::serialize({expr}, {buf_var});"
        );
    }
}

/// Returns a deserialization expression for TL type `ty` (producing `Result<T, DeserializeError>`).
fn deserialize_expr(
    ty: &TypeRef,
    def: &Definition,
    param_name: Option<&str>,
    boxed_field: bool,
    buf_var: &str,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
) -> String {
    let inner = if ty.is_vector() && ty.bare {
        let elem_ty = ty
            .generic_arg
            .as_deref()
            .map(|a| rust_type_path(a, def, None, index, config))
            .unwrap_or_else(|| "crate::RawObject".to_string());
        format!("crate::deserialize_bare_vec::<{elem_ty}>({buf_var})?")
    } else {
        let rust_ty = rust_type_path(ty, def, param_name, index, config);
        format!("<{rust_ty} as crate::Deserializable>::deserialize({buf_var})?")
    };

    if boxed_field {
        let rust_ty = rust_type_path(ty, def, param_name, index, config);
        format!(
            "{{ {buf_var}.charge_alloc(core::mem::size_of::<{rust_ty}>())?; Box::new({inner}) }}"
        )
    } else {
        inner
    }
}

/// Generates the complete Rust source for a single parsed [`Schema`] containing
/// `pub mod types`, `pub mod enums`, and `pub mod functions` (plus `pub const LAYER` if present).
pub fn generate_schema(schema: &Schema, config: &GeneratorConfig) -> String {
    let index = SchemaIndex::build(schema);
    let mut out = String::with_capacity(256 * 1024);

    out.push_str("// @generated by tg-tl-gen. Do not edit manually.\n\n");

    if let Some(layer) = schema.layer {
        let _ = writeln!(out, "/// Telegram API schema layer number.");
        let _ = writeln!(out, "pub const LAYER: i32 = {layer};\n");
    }

    // Group constructors, enums, and functions by namespace.
    let mut types_by_ns: BTreeMap<Option<String>, Vec<&Definition>> = BTreeMap::new();
    let mut funcs_by_ns: BTreeMap<Option<String>, Vec<&Definition>> = BTreeMap::new();
    for def in &schema.definitions {
        match def.category {
            Category::Type => {
                if def.result_type.full_name() == "Bool" {
                    continue;
                }
                types_by_ns
                    .entry(def.namespace.clone())
                    .or_default()
                    .push(def);
            }
            Category::Function => {
                funcs_by_ns
                    .entry(def.namespace.clone())
                    .or_default()
                    .push(def);
            }
        }
    }

    let mut enums_by_ns: BTreeMap<Option<String>, Vec<(String, Vec<&Definition>)>> =
        BTreeMap::new();
    for (res_full, ctors) in &index.enums_by_result {
        let (ns, short) = match res_full.rsplit_once('.') {
            Some((ns, s)) => (Some(ns.to_string()), s.to_string()),
            None => (None, res_full.clone()),
        };
        enums_by_ns
            .entry(ns)
            .or_default()
            .push((short, ctors.clone()));
    }

    // 1. `pub mod types`
    out.push_str("/// Bare TL type constructors.\n");
    out.push_str("#[allow(clippy::upper_case_acronyms, clippy::large_enum_variant)]\n");
    out.push_str("pub mod types {\n");
    for (ns, ctors) in &types_by_ns {
        if let Some(ns_name) = ns {
            let _ = writeln!(out, "    /// Constructors in the `{ns_name}` namespace.");
            let _ = writeln!(out, "    pub mod {ns_name} {{");
            for ctor in ctors {
                emit_type_struct(ctor, &index, config, &mut out, "        ");
            }
            out.push_str("    }\n");
        } else {
            for ctor in ctors {
                emit_type_struct(ctor, &index, config, &mut out, "    ");
            }
        }
    }
    out.push_str("}\n\n");

    // 2. `pub mod enums`
    out.push_str("/// Boxed TL sum types.\n");
    out.push_str("#[allow(clippy::upper_case_acronyms, clippy::large_enum_variant)]\n");
    out.push_str("pub mod enums {\n");
    for (ns, enums) in &enums_by_ns {
        if let Some(ns_name) = ns {
            let _ = writeln!(out, "    /// Boxed types in the `{ns_name}` namespace.");
            let _ = writeln!(out, "    pub mod {ns_name} {{");
            for (short, ctors) in enums {
                emit_enum(
                    Some(ns_name.as_str()),
                    short,
                    ctors,
                    config,
                    &mut out,
                    "        ",
                );
            }
            out.push_str("    }\n");
        } else {
            for (short, ctors) in enums {
                emit_enum(None, short, ctors, config, &mut out, "    ");
            }
        }
    }
    out.push_str("}\n\n");

    // 3. `pub mod functions`
    out.push_str("/// TL RPC function requests.\n");
    out.push_str("#[allow(clippy::upper_case_acronyms, clippy::large_enum_variant)]\n");
    out.push_str("pub mod functions {\n");
    for (ns, funcs) in &funcs_by_ns {
        if let Some(ns_name) = ns {
            let _ = writeln!(out, "    /// RPC functions in the `{ns_name}` namespace.");
            let _ = writeln!(out, "    pub mod {ns_name} {{");
            for func in funcs {
                emit_function_struct(func, &index, config, &mut out, "        ");
            }
            out.push_str("    }\n");
        } else {
            for func in funcs {
                emit_function_struct(func, &index, config, &mut out, "    ");
            }
        }
    }
    out.push_str("}\n");

    out
}

/// Emits a bare constructor struct + `Identifiable` + `Serializable` + `Deserializable` + `From`.
fn emit_type_struct(
    def: &Definition,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    out: &mut String,
    indent: &str,
) {
    let struct_name = to_pascal_case(&def.name);
    let full_name = def.full_name();
    let owner_result = def.result_type.full_name();
    let has_fields = has_struct_fields(def);

    let _ = writeln!(
        out,
        "{indent}/// Bare TL constructor `{full_name}#{:08x}`.",
        def.id
    );
    let _ = writeln!(out, "{indent}#[derive(Debug, Clone, PartialEq)]");
    if !has_fields {
        let _ = writeln!(out, "{indent}pub struct {struct_name};");
    } else {
        let _ = writeln!(out, "{indent}pub struct {struct_name} {{");
        for param in &def.params {
            emit_struct_field(param, def, Some(&owner_result), index, config, out, indent);
        }
        let _ = writeln!(out, "{indent}}}");
    }

    let _ = writeln!(out, "{indent}impl crate::Identifiable for {struct_name} {{");
    let _ = writeln!(
        out,
        "{indent}    const CONSTRUCTOR_ID: u32 = 0x{:08x};",
        def.id
    );
    let _ = writeln!(out, "{indent}}}");

    let _ = writeln!(out, "{indent}impl {struct_name} {{");
    let _ = writeln!(out, "{indent}    /// Canonical TL constructor name.");
    let _ = writeln!(
        out,
        "{indent}    pub const NAME: &'static str = \"{full_name}\";"
    );
    let _ = writeln!(
        out,
        "{indent}    /// Returns the canonical TL constructor name."
    );
    let _ = writeln!(
        out,
        "{indent}    pub const fn name(&self) -> &'static str {{ Self::NAME }}"
    );
    let _ = writeln!(out, "{indent}}}");

    // Bare Serializable (no constructor ID prefix).
    let _ = writeln!(out, "{indent}impl crate::Serializable for {struct_name} {{");
    if !has_fields && def.params.is_empty() {
        let _ = writeln!(
            out,
            "{indent}    fn serialize(&self, _buf: &mut Vec<u8>) {{}}"
        );
    } else {
        let _ = writeln!(
            out,
            "{indent}    fn serialize(&self, __buf: &mut Vec<u8>) {{"
        );
        emit_serialize_params(def, out, &format!("{indent}        "));
        let _ = writeln!(out, "{indent}    }}");
    }
    let _ = writeln!(out, "{indent}}}");

    // Bare Deserializable (no constructor ID prefix).
    let _ = writeln!(
        out,
        "{indent}impl crate::Deserializable for {struct_name} {{"
    );
    if !has_fields && def.params.is_empty() {
        let _ = writeln!(
            out,
            "{indent}    fn deserialize(__buf: &mut crate::Cursor<'_>) -> Result<Self, crate::DeserializeError> {{ __buf.charge_elements(1)?; Ok(Self) }}"
        );
    } else {
        let _ = writeln!(
            out,
            "{indent}    fn deserialize(__buf: &mut crate::Cursor<'_>) -> Result<Self, crate::DeserializeError> {{"
        );
        let _ = writeln!(
            out,
            "{indent}        let mut __depth = __buf.enter_depth()?;"
        );
        let _ = writeln!(out, "{indent}        let __buf = &mut *__depth;");
        let _ = writeln!(out, "{indent}        __buf.charge_elements(1)?;");
        emit_deserialize_params(
            def,
            Some(&owner_result),
            has_fields,
            index,
            config,
            out,
            &format!("{indent}        "),
        );
        let _ = writeln!(out, "{indent}    }}");
    }
    let _ = writeln!(out, "{indent}}}");

    // `From<types::Ctor> for enums::Result`
    let enum_path = enum_type_path(
        &config.module_prefix,
        def.result_type.namespace.as_deref(),
        &def.result_type.name,
    );
    let variant = enum_variant_name(&def.result_type.name, &def.name);
    let _ = writeln!(out, "{indent}impl From<{struct_name}> for {enum_path} {{");
    if has_fields {
        let _ = writeln!(
            out,
            "{indent}    fn from(v: {struct_name}) -> Self {{ Self::{variant}(v) }}"
        );
    } else {
        let _ = writeln!(
            out,
            "{indent}    fn from(_: {struct_name}) -> Self {{ Self::{variant} }}"
        );
    }
    let _ = writeln!(out, "{indent}}}");
}

/// Emits a boxed sum-type enum + `Serializable` + `Deserializable` + helpers.
fn emit_enum(
    ns: Option<&str>,
    short_name: &str,
    ctors: &[&Definition],
    config: &GeneratorConfig,
    out: &mut String,
    indent: &str,
) {
    let enum_name = to_pascal_case(short_name);
    let full_name = match ns {
        Some(n) => format!("{n}.{short_name}"),
        None => short_name.to_string(),
    };

    let _ = writeln!(out, "{indent}/// Boxed TL type `{full_name}`.");
    let _ = writeln!(out, "{indent}#[derive(Debug, Clone, PartialEq)]");
    let _ = writeln!(out, "{indent}pub enum {enum_name} {{");
    for ctor in ctors {
        let variant = enum_variant_name(short_name, &ctor.name);
        let _ = writeln!(
            out,
            "{indent}    /// Variant for `{}` (`0x{:08x}`).",
            ctor.full_name(),
            ctor.id
        );
        if has_struct_fields(ctor) {
            let path = type_struct_path(&config.module_prefix, ctor);
            let _ = writeln!(out, "{indent}    {variant}({path}),");
        } else {
            let _ = writeln!(out, "{indent}    {variant},");
        }
    }
    let _ = writeln!(out, "{indent}}}");

    // Inherent helpers: `constructor_id()` and `name()`.
    let _ = writeln!(out, "{indent}impl {enum_name} {{");
    let _ = writeln!(
        out,
        "{indent}    /// Returns the 32-bit constructor ID of the active variant."
    );
    let _ = writeln!(
        out,
        "{indent}    pub const fn constructor_id(&self) -> u32 {{"
    );
    let _ = writeln!(out, "{indent}        match self {{");
    for ctor in ctors {
        let variant = enum_variant_name(short_name, &ctor.name);
        if has_struct_fields(ctor) {
            let _ = writeln!(
                out,
                "{indent}            Self::{variant}(_) => 0x{:08x},",
                ctor.id
            );
        } else {
            let _ = writeln!(
                out,
                "{indent}            Self::{variant} => 0x{:08x},",
                ctor.id
            );
        }
    }
    let _ = writeln!(out, "{indent}        }}");
    let _ = writeln!(out, "{indent}    }}");

    let _ = writeln!(
        out,
        "{indent}    /// Returns the canonical TL constructor name of the active variant."
    );
    let _ = writeln!(
        out,
        "{indent}    pub const fn name(&self) -> &'static str {{"
    );
    let _ = writeln!(out, "{indent}        match self {{");
    for ctor in ctors {
        let variant = enum_variant_name(short_name, &ctor.name);
        let cname = ctor.full_name();
        if has_struct_fields(ctor) {
            let _ = writeln!(
                out,
                "{indent}            Self::{variant}(_) => \"{cname}\","
            );
        } else {
            let _ = writeln!(out, "{indent}            Self::{variant} => \"{cname}\",");
        }
    }
    let _ = writeln!(out, "{indent}        }}");
    let _ = writeln!(out, "{indent}    }}");
    let _ = writeln!(out, "{indent}}}");

    // Boxed Serializable (writes constructor ID, then bare payload).
    let _ = writeln!(out, "{indent}impl crate::Serializable for {enum_name} {{");
    let _ = writeln!(
        out,
        "{indent}    fn serialize(&self, __buf: &mut Vec<u8>) {{"
    );
    let _ = writeln!(out, "{indent}        match self {{");
    for ctor in ctors {
        let variant = enum_variant_name(short_name, &ctor.name);
        if has_struct_fields(ctor) {
            let _ = writeln!(out, "{indent}            Self::{variant}(v) => {{");
            let _ = writeln!(
                out,
                "{indent}                __buf.extend_from_slice(&0x{:08x}u32.to_le_bytes());",
                ctor.id
            );
            let _ = writeln!(
                out,
                "{indent}                crate::Serializable::serialize(v, __buf);"
            );
            let _ = writeln!(out, "{indent}            }}");
        } else {
            let _ = writeln!(
                out,
                "{indent}            Self::{variant} => __buf.extend_from_slice(&0x{:08x}u32.to_le_bytes()),",
                ctor.id
            );
        }
    }
    let _ = writeln!(out, "{indent}        }}");
    let _ = writeln!(out, "{indent}    }}");
    let _ = writeln!(out, "{indent}}}");

    // Boxed Deserializable (reads constructor ID, dispatches to bare constructor).
    let _ = writeln!(out, "{indent}impl crate::Deserializable for {enum_name} {{");
    let _ = writeln!(
        out,
        "{indent}    fn deserialize(__buf: &mut crate::Cursor<'_>) -> Result<Self, crate::DeserializeError> {{"
    );
    let _ = writeln!(out, "{indent}        let __id = __buf.read_u32()?;");
    let _ = writeln!(out, "{indent}        match __id {{");
    for ctor in ctors {
        let variant = enum_variant_name(short_name, &ctor.name);
        if has_struct_fields(ctor) {
            let path = type_struct_path(&config.module_prefix, ctor);
            let _ = writeln!(
                out,
                "{indent}            0x{:08x} => Ok(Self::{variant}(<{path} as crate::Deserializable>::deserialize(__buf)?)),",
                ctor.id
            );
        } else {
            let _ = writeln!(
                out,
                "{indent}            0x{:08x} => {{ __buf.charge_elements(1)?; Ok(Self::{variant}) }},",
                ctor.id
            );
        }
    }
    let _ = writeln!(
        out,
        "{indent}            id => Err(crate::DeserializeError::UnexpectedConstructor {{ id }}),"
    );
    let _ = writeln!(out, "{indent}        }}");
    let _ = writeln!(out, "{indent}    }}");
    let _ = writeln!(out, "{indent}}}");

    // For single-variant enums, also generate lossless `From<enums::T> for types::Ctor`.
    if ctors.len() == 1 {
        let ctor = ctors[0];
        let variant = enum_variant_name(short_name, &ctor.name);
        let path = type_struct_path(&config.module_prefix, ctor);
        let _ = writeln!(out, "{indent}impl From<{enum_name}> for {path} {{");
        if has_struct_fields(ctor) {
            let _ = writeln!(
                out,
                "{indent}    fn from(v: {enum_name}) -> Self {{ match v {{ {enum_name}::{variant}(x) => x }} }}"
            );
        } else {
            let _ = writeln!(
                out,
                "{indent}    fn from(_: {enum_name}) -> Self {{ {path} }}"
            );
        }
        let _ = writeln!(out, "{indent}}}");
    }
}

/// Emits an RPC function struct + `Identifiable` + `Serializable` + `Deserializable` + `RemoteCall`.
fn emit_function_struct(
    def: &Definition,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    out: &mut String,
    indent: &str,
) {
    let struct_name = to_pascal_case(&def.name);
    let full_name = def.full_name();
    let has_fields = has_struct_fields(def);
    let is_generic = !def.generic_params.is_empty();

    let generic_decl = if is_generic {
        format!("<{}>", def.generic_params.join(", "))
    } else {
        String::new()
    };
    let generic_ser_bounds = if is_generic {
        format!(
            "<{}>",
            def.generic_params
                .iter()
                .map(|g| format!("{g}: crate::Serializable"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    let generic_de_bounds = if is_generic {
        format!(
            "<{}>",
            def.generic_params
                .iter()
                .map(|g| format!("{g}: crate::Deserializable"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    let generic_rc_bounds = if is_generic {
        format!(
            "<{}>",
            def.generic_params
                .iter()
                .map(|g| format!("{g}: crate::RemoteCall"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };

    let _ = writeln!(
        out,
        "{indent}/// TL RPC function `{full_name}#{:08x}`.",
        def.id
    );
    let _ = writeln!(out, "{indent}#[derive(Debug, Clone, PartialEq)]");
    if !has_fields {
        let _ = writeln!(out, "{indent}pub struct {struct_name}{generic_decl};");
    } else {
        let _ = writeln!(out, "{indent}pub struct {struct_name}{generic_decl} {{");
        for param in &def.params {
            emit_struct_field(param, def, None, index, config, out, indent);
        }
        let _ = writeln!(out, "{indent}}}");
    }

    let _ = writeln!(
        out,
        "{indent}impl{generic_decl} crate::Identifiable for {struct_name}{generic_decl} {{"
    );
    let _ = writeln!(
        out,
        "{indent}    const CONSTRUCTOR_ID: u32 = 0x{:08x};",
        def.id
    );
    let _ = writeln!(out, "{indent}}}");

    let _ = writeln!(
        out,
        "{indent}impl{generic_decl} {struct_name}{generic_decl} {{"
    );
    let _ = writeln!(out, "{indent}    /// Canonical TL function name.");
    let _ = writeln!(
        out,
        "{indent}    pub const NAME: &'static str = \"{full_name}\";"
    );
    let _ = writeln!(
        out,
        "{indent}    /// Returns the canonical TL function name."
    );
    let _ = writeln!(
        out,
        "{indent}    pub const fn name(&self) -> &'static str {{ Self::NAME }}"
    );
    let _ = writeln!(out, "{indent}}}");

    // Functions always serialize WITH their constructor ID prefix.
    let _ = writeln!(
        out,
        "{indent}impl{generic_ser_bounds} crate::Serializable for {struct_name}{generic_decl} {{"
    );
    let _ = writeln!(
        out,
        "{indent}    fn serialize(&self, __buf: &mut Vec<u8>) {{"
    );
    let _ = writeln!(
        out,
        "{indent}        __buf.extend_from_slice(&0x{:08x}u32.to_le_bytes());",
        def.id
    );
    emit_serialize_params(def, out, &format!("{indent}        "));
    let _ = writeln!(out, "{indent}    }}");
    let _ = writeln!(out, "{indent}}}");

    // Functions deserialize by verifying their constructor ID prefix first.
    let _ = writeln!(
        out,
        "{indent}impl{generic_de_bounds} crate::Deserializable for {struct_name}{generic_decl} {{"
    );
    let _ = writeln!(
        out,
        "{indent}    fn deserialize(__buf: &mut crate::Cursor<'_>) -> Result<Self, crate::DeserializeError> {{"
    );
    let _ = writeln!(
        out,
        "{indent}        let mut __depth = __buf.enter_depth()?;"
    );
    let _ = writeln!(out, "{indent}        let __buf = &mut *__depth;");
    let _ = writeln!(out, "{indent}        let __id = __buf.read_u32()?;");
    let _ = writeln!(out, "{indent}        if __id != 0x{:08x} {{", def.id);
    let _ = writeln!(
        out,
        "{indent}            return Err(crate::DeserializeError::UnexpectedConstructor {{ id: __id }});"
    );
    let _ = writeln!(out, "{indent}        }}");
    let _ = writeln!(out, "{indent}        __buf.charge_elements(1)?;");
    if !has_fields && def.params.is_empty() {
        let _ = writeln!(out, "{indent}        Ok(Self)");
    } else {
        emit_deserialize_params(
            def,
            None,
            has_fields,
            index,
            config,
            out,
            &format!("{indent}        "),
        );
    }
    let _ = writeln!(out, "{indent}    }}");
    let _ = writeln!(out, "{indent}}}");

    // RemoteCall implementation.
    let return_ty = if is_generic
        && def
            .generic_params
            .iter()
            .any(|g| g == &def.result_type.name)
    {
        format!("{}::Return", def.result_type.name)
    } else {
        rust_type_path(&def.result_type, def, None, index, config)
    };
    let _ = writeln!(
        out,
        "{indent}impl{generic_rc_bounds} crate::RemoteCall for {struct_name}{generic_decl} {{"
    );
    let _ = writeln!(out, "{indent}    type Return = {return_ty};");
    let _ = writeln!(out, "{indent}}}");
}

/// Emits a single public struct field declaration (skipping `flags:#` parameters).
fn emit_struct_field(
    param: &Param,
    def: &Definition,
    owner_result: Option<&str>,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    out: &mut String,
    indent: &str,
) {
    let field_ident = rust_field_ident(&param.name);
    match &param.ty {
        ParamType::Flags => {}
        ParamType::Normal(ty) => {
            let mut rust_ty = rust_type_path(ty, def, Some(&param.name), index, config);
            if owner_result.is_some_and(|r| index.needs_box(r, ty)) {
                rust_ty = format!("Box<{rust_ty}>");
            }
            let _ = writeln!(out, "{indent}    /// TL parameter `{}`.", param.name);
            let _ = writeln!(out, "{indent}    pub {field_ident}: {rust_ty},");
        }
        ParamType::Flag { inner, .. } => match inner {
            FlagInner::True => {
                let _ = writeln!(out, "{indent}    /// TL flag `{}`.", param.name);
                let _ = writeln!(out, "{indent}    pub {field_ident}: bool,");
            }
            FlagInner::Type(ty) => {
                let mut rust_ty = rust_type_path(ty, def, Some(&param.name), index, config);
                if owner_result.is_some_and(|r| index.needs_box(r, ty)) {
                    rust_ty = format!("Box<{rust_ty}>");
                }
                let _ = writeln!(
                    out,
                    "{indent}    /// Optional TL parameter `{}`.",
                    param.name
                );
                let _ = writeln!(out, "{indent}    pub {field_ident}: Option<{rust_ty}>,");
            }
        },
    }
}

/// Maps flag field name -> bit index -> list of `(rust_field_ident, is_true_flag)`.
type FlagBitMap<'a> = HashMap<&'a str, BTreeMap<u32, Vec<(String, bool)>>>;

/// Emits parameter serialization statements in exact TL parameter order.
fn emit_serialize_params(def: &Definition, out: &mut String, indent: &str) {
    // Group conditional parameters by `(flag_field, bit)` to handle shared bits cleanly.
    let mut flag_bits: FlagBitMap<'_> = HashMap::new();
    for param in &def.params {
        if let ParamType::Flag {
            flag_field,
            bit,
            inner,
        } = &param.ty
        {
            let is_true = matches!(inner, FlagInner::True);
            flag_bits
                .entry(flag_field.as_str())
                .or_default()
                .entry(*bit)
                .or_default()
                .push((rust_field_ident(&param.name), is_true));
        }
    }

    for param in &def.params {
        match &param.ty {
            ParamType::Flags => {
                let fname = param.name.as_str();
                let bits = flag_bits.get(fname);
                let has_any = bits.is_some_and(|b| !b.is_empty());
                if has_any {
                    let _ = writeln!(out, "{indent}let mut __flag_{fname}: u32 = 0;");
                    if let Some(bit_map) = bits {
                        for (&bit, users) in bit_map {
                            let typed: Vec<&str> = users
                                .iter()
                                .filter(|(_, is_true)| !*is_true)
                                .map(|(n, _)| n.as_str())
                                .collect();
                            let cond = if typed.is_empty() {
                                // Only `flags.N?true` bool field(s) use this bit.
                                users
                                    .iter()
                                    .map(|(n, _)| format!("self.{n}"))
                                    .collect::<Vec<_>>()
                                    .join(" || ")
                            } else {
                                // One or more `Option<T>` fields use this bit; set the bit
                                // iff all typed fields sharing this bit are `Some`.
                                typed
                                    .iter()
                                    .map(|n| format!("self.{n}.is_some()"))
                                    .collect::<Vec<_>>()
                                    .join(" && ")
                            };
                            let _ = writeln!(
                                out,
                                "{indent}if {cond} {{ __flag_{fname} |= 1u32 << {bit}; }}"
                            );
                        }
                    }
                    let _ = writeln!(
                        out,
                        "{indent}__buf.extend_from_slice(&__flag_{fname}.to_le_bytes());"
                    );
                } else {
                    let _ = writeln!(out, "{indent}__buf.extend_from_slice(&0u32.to_le_bytes());");
                }
            }
            ParamType::Normal(ty) => {
                let field = rust_field_ident(&param.name);
                emit_serialize_expr(ty, &format!("&self.{field}"), "__buf", out, indent);
            }
            ParamType::Flag {
                flag_field,
                bit,
                inner,
            } => {
                if let FlagInner::Type(ty) = inner {
                    let field = rust_field_ident(&param.name);
                    let _ = writeln!(
                        out,
                        "{indent}if (__flag_{flag_field} & (1u32 << {bit})) != 0 && let Some(ref __v) = self.{field} {{"
                    );
                    emit_serialize_expr(ty, "__v", "__buf", out, &format!("{indent}    "));
                    let _ = writeln!(out, "{indent}}}");
                }
            }
        }
    }
}

/// Emits parameter deserialization statements + `Ok(Self { ... })` in exact TL parameter order.
fn emit_deserialize_params(
    def: &Definition,
    owner_result: Option<&str>,
    has_fields: bool,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    out: &mut String,
    indent: &str,
) {
    let mut used_flags: HashSet<&str> = HashSet::new();
    for param in &def.params {
        if let ParamType::Flag { flag_field, .. } = &param.ty {
            used_flags.insert(flag_field.as_str());
        }
    }

    let mut field_idents = Vec::new();
    for param in &def.params {
        match &param.ty {
            ParamType::Flags => {
                let fname = param.name.as_str();
                if used_flags.contains(fname) {
                    let _ = writeln!(out, "{indent}let __flag_{fname} = __buf.read_u32()?;");
                } else {
                    let _ = writeln!(out, "{indent}let _ = __buf.read_u32()?;");
                }
            }
            ParamType::Normal(ty) => {
                let field = rust_field_ident(&param.name);
                let boxed = owner_result.is_some_and(|r| index.needs_box(r, ty));
                let expr =
                    deserialize_expr(ty, def, Some(&param.name), boxed, "__buf", index, config);
                let _ = writeln!(out, "{indent}let {field} = {expr};");
                field_idents.push(field);
            }
            ParamType::Flag {
                flag_field,
                bit,
                inner,
            } => {
                let field = rust_field_ident(&param.name);
                match inner {
                    FlagInner::True => {
                        let _ = writeln!(
                            out,
                            "{indent}let {field} = (__flag_{flag_field} & (1u32 << {bit})) != 0;"
                        );
                    }
                    FlagInner::Type(ty) => {
                        let boxed = owner_result.is_some_and(|r| index.needs_box(r, ty));
                        let expr = deserialize_expr(
                            ty,
                            def,
                            Some(&param.name),
                            boxed,
                            "__buf",
                            index,
                            config,
                        );
                        let _ = writeln!(
                            out,
                            "{indent}let {field} = if (__flag_{flag_field} & (1u32 << {bit})) != 0 {{ Some({expr}) }} else {{ None }};"
                        );
                    }
                }
                field_idents.push(field);
            }
        }
    }

    if !has_fields {
        let _ = writeln!(out, "{indent}Ok(Self)");
    } else {
        let _ = writeln!(out, "{indent}Ok(Self {{");
        for f in &field_idents {
            let _ = writeln!(out, "{indent}    {f},");
        }
        let _ = writeln!(out, "{indent}}})");
    }
}

/// Generates a sorted static constructor-ID lookup table and `pub fn from_constructor_id`.
///
/// Uses binary search over a sorted slice instead of a giant `match` to keep compile times fast.
pub fn generate_constructor_id_table(schemas: &[&Schema], extra: &[(u32, &str)]) -> String {
    let mut entries: BTreeMap<u32, String> = BTreeMap::new();
    for &(id, name) in extra {
        entries.insert(id, name.to_string());
    }
    for schema in schemas {
        for def in &schema.definitions {
            entries.entry(def.id).or_insert_with(|| def.full_name());
        }
    }

    let mut out = String::with_capacity(entries.len() * 48);
    out.push_str("// @generated by tg-tl-gen. Do not edit manually.\n\n");
    out.push_str("static CONSTRUCTOR_NAMES: &[(u32, &str)] = &[\n");
    for (id, name) in &entries {
        let _ = writeln!(out, "    (0x{id:08x}, \"{name}\"),");
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// Looks up the canonical TL constructor or function name for a 32-bit constructor ID.\n",
    );
    out.push_str("pub fn from_constructor_id(id: u32) -> Option<&'static str> {\n");
    out.push_str("    CONSTRUCTOR_NAMES\n");
    out.push_str("        .binary_search_by_key(&id, |&(k, _)| k)\n");
    out.push_str("        .ok()\n");
    out.push_str("        .map(|idx| CONSTRUCTOR_NAMES[idx].1)\n");
    out.push_str("}\n");
    out
}

/// Generates an exhaustive round-trip serialization/deserialization test covering every
/// constructor (bare and boxed) and every function in `schema`.
pub fn generate_roundtrip_test(
    schema: &Schema,
    config: &GeneratorConfig,
    mod_name: &str,
    generic_dummy_call: &str,
) -> String {
    let index = SchemaIndex::build(schema);

    // Compute a finite-depth "base" constructor for each enum via fixed-point iteration,
    // preferring constructors with minimal nesting depth so sample construction always terminates.
    let mut enum_depth: HashMap<String, usize> = HashMap::new();
    let mut enum_base_ctor: HashMap<String, &Definition> = HashMap::new();

    for _ in 0..=index.enums_by_result.len() {
        let mut changed = false;
        for (res, ctors) in &index.enums_by_result {
            for ctor in ctors {
                // Only unconditional (non-Option) non-Vec non-builtin fields contribute to min depth.
                let mut max_child_depth: Option<usize> = Some(0);
                for param in &ctor.params {
                    if let ParamType::Normal(ty) = &param.ty {
                        if ty.is_vector() || ty.is_builtin_scalar() || ty.generic_ref {
                            continue;
                        }
                        let target = if ty.bare {
                            if let Some(c) = index.resolve_bare_ctor(ty) {
                                c.result_type.full_name()
                            } else {
                                ty.full_name()
                            }
                        } else {
                            ty.full_name()
                        };
                        if let Some(&d) = enum_depth.get(&target) {
                            max_child_depth = max_child_depth.map(|cur| cur.max(d + 1));
                        } else {
                            max_child_depth = None;
                            break;
                        }
                    }
                }
                if let Some(d) = max_child_depth {
                    let is_better = enum_depth.get(res).is_none_or(|&prev| d < prev);
                    if is_better {
                        enum_depth.insert(res.clone(), d);
                        enum_base_ctor.insert(res.clone(), ctor);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut out = String::with_capacity(512 * 1024);
    out.push_str("// @generated by tg-tl-gen. Do not edit manually.\n");
    let _ = writeln!(out, "#[allow(clippy::approx_constant, dead_code)]");
    let _ = writeln!(out, "mod {mod_name} {{");
    out.push_str("    use crate::{Deserializable, Serializable};\n\n");

    let mut sample_fn_names: HashMap<String, String> = HashMap::new();
    for (idx, res_full) in index.enums_by_result.keys().enumerate() {
        sample_fn_names.insert(res_full.clone(), format!("sample_enum_{idx}"));
    }
    let sample_fn_name = |res_full: &str| -> String {
        sample_fn_names
            .get(res_full)
            .cloned()
            .expect("enum sample fn registered")
    };

    // Emit one minimal sample builder function per enum (`#[inline(never)]` keeps LLVM fast).
    for (res_full, ctors) in &index.enums_by_result {
        let (ns, short) = match res_full.rsplit_once('.') {
            Some((n, s)) => (Some(n), s),
            None => (None, res_full.as_str()),
        };
        let enum_path = enum_type_path(&config.module_prefix, ns, short);
        let fn_name = sample_fn_name(res_full);
        let base_ctor = enum_base_ctor.get(res_full).copied().unwrap_or(ctors[0]);
        let variant = enum_variant_name(short, &base_ctor.name);
        let _ = writeln!(out, "    #[inline(never)]");
        let _ = writeln!(out, "    fn {fn_name}() -> {enum_path} {{");
        if has_struct_fields(base_ctor) {
            let expr = build_sample_struct(
                base_ctor,
                Some(res_full),
                false,
                &index,
                config,
                &sample_fn_name,
                generic_dummy_call,
            );
            let _ = writeln!(out, "        {enum_path}::{variant}({expr})");
        } else {
            let _ = writeln!(out, "        {enum_path}::{variant}");
        }
        out.push_str("    }\n");
    }

    // Emit chunked helper functions for all constructors so LLVM optimizes small functions quickly.
    const CHUNK_SIZE: usize = 25;
    let ctor_defs: Vec<&Definition> = schema
        .definitions
        .iter()
        .filter(|d| d.category == Category::Type && d.result_type.full_name() != "Bool")
        .collect();
    let ctor_chunk_count = ctor_defs.len().div_ceil(CHUNK_SIZE);

    for (chunk_idx, chunk) in ctor_defs.chunks(CHUNK_SIZE).enumerate() {
        let _ = writeln!(out, "\n    #[inline(never)]");
        let _ = writeln!(out, "    fn roundtrip_ctor_chunk_{chunk_idx}() -> usize {{");
        let _ = writeln!(out, "        let mut count = 0usize;");
        for def in chunk {
            let res_full = def.result_type.full_name();
            let enum_path = enum_type_path(
                &config.module_prefix,
                def.result_type.namespace.as_deref(),
                &def.result_type.name,
            );
            let has_flag_params = def
                .params
                .iter()
                .any(|p| matches!(p.ty, ParamType::Flag { .. }));
            let modes: &[bool] = if has_flag_params {
                &[false, true]
            } else {
                &[false]
            };
            for &populate_flags in modes {
                let bare_expr = build_sample_struct(
                    def,
                    Some(&res_full),
                    populate_flags,
                    &index,
                    config,
                    &sample_fn_name,
                    generic_dummy_call,
                );
                let struct_path = type_struct_path(&config.module_prefix, def);
                out.push_str("        {\n");
                let _ = writeln!(out, "            let bare: {struct_path} = {bare_expr};");
                out.push_str("            let bytes = bare.to_bytes();\n");
                let _ = writeln!(
                    out,
                    "            let decoded = {struct_path}::from_bytes(&bytes).expect(\"bare roundtrip {}\");",
                    def.full_name()
                );
                out.push_str("            assert_eq!(bare, decoded);\n");
                let _ = writeln!(out, "            let boxed: {enum_path} = bare.into();");
                out.push_str("            let boxed_bytes = boxed.to_bytes();\n");
                let _ = writeln!(
                    out,
                    "            let decoded_boxed = {enum_path}::from_bytes(&boxed_bytes).expect(\"boxed roundtrip {}\");",
                    def.full_name()
                );
                out.push_str("            assert_eq!(boxed, decoded_boxed);\n");
                let _ = writeln!(
                    out,
                    "            assert_eq!(boxed.constructor_id(), 0x{:08x});",
                    def.id
                );
                let _ = writeln!(
                    out,
                    "            assert_eq!(boxed.name(), \"{}\");",
                    def.full_name()
                );
                out.push_str("        }\n");
            }
            out.push_str("        count += 1;\n");
        }
        out.push_str("        count\n    }\n");
    }

    out.push_str("\n    #[test]\n");
    out.push_str("    fn roundtrip_all_constructors() {\n");
    out.push_str("        let mut total = 0usize;\n");
    for chunk_idx in 0..ctor_chunk_count {
        let _ = writeln!(out, "        total += roundtrip_ctor_chunk_{chunk_idx}();");
    }
    let _ = writeln!(out, "        assert_eq!(total, {});", ctor_defs.len());
    out.push_str("    }\n");

    // Emit chunked helper functions for all RPC functions.
    let func_defs: Vec<&Definition> = schema
        .definitions
        .iter()
        .filter(|d| d.category == Category::Function)
        .collect();
    let func_chunk_count = func_defs.len().div_ceil(CHUNK_SIZE);

    for (chunk_idx, chunk) in func_defs.chunks(CHUNK_SIZE).enumerate() {
        let _ = writeln!(out, "\n    #[inline(never)]");
        let _ = writeln!(out, "    fn roundtrip_func_chunk_{chunk_idx}() -> usize {{");
        let _ = writeln!(out, "        let mut count = 0usize;");
        for def in chunk {
            let has_flag_params = def
                .params
                .iter()
                .any(|p| matches!(p.ty, ParamType::Flag { .. }));
            let modes: &[bool] = if has_flag_params {
                &[false, true]
            } else {
                &[false]
            };
            let fn_path = func_struct_path(&config.module_prefix, def);
            let full_ty = if def.generic_params.is_empty() {
                fn_path.clone()
            } else {
                format!("{fn_path}<{generic_dummy_call}>")
            };
            for &populate_flags in modes {
                let expr = build_sample_struct(
                    def,
                    None,
                    populate_flags,
                    &index,
                    config,
                    &sample_fn_name,
                    generic_dummy_call,
                );
                out.push_str("        {\n");
                let _ = writeln!(out, "            let req: {full_ty} = {expr};");
                out.push_str("            let bytes = req.to_bytes();\n");
                let _ = writeln!(
                    out,
                    "            let decoded = <{full_ty}>::from_bytes(&bytes).expect(\"func roundtrip {}\");",
                    def.full_name()
                );
                out.push_str("            assert_eq!(req, decoded);\n");
                let _ = writeln!(
                    out,
                    "            assert_eq!(req.name(), \"{}\");",
                    def.full_name()
                );
                out.push_str("        }\n");
            }
            out.push_str("        count += 1;\n");
        }
        out.push_str("        count\n    }\n");
    }

    out.push_str("\n    #[test]\n");
    out.push_str("    fn roundtrip_all_functions() {\n");
    out.push_str("        let mut total = 0usize;\n");
    for chunk_idx in 0..func_chunk_count {
        let _ = writeln!(out, "        total += roundtrip_func_chunk_{chunk_idx}();");
    }
    let _ = writeln!(out, "        assert_eq!(total, {});", func_defs.len());
    out.push_str("    }\n");
    out.push_str("}\n");

    out
}

fn build_sample_struct(
    def: &Definition,
    owner_result: Option<&str>,
    populate_flags: bool,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    sample_fn_name: &dyn Fn(&str) -> String,
    generic_dummy_call: &str,
) -> String {
    let path = match def.category {
        Category::Type => type_struct_path(&config.module_prefix, def),
        Category::Function => func_struct_path(&config.module_prefix, def),
    };
    if !has_struct_fields(def) {
        return path;
    }

    let mut out = format!("{path} {{ ");
    for param in &def.params {
        match &param.ty {
            ParamType::Flags => {}
            ParamType::Normal(ty) => {
                let field = rust_field_ident(&param.name);
                let val = build_sample_value(
                    ty,
                    def,
                    Some(&param.name),
                    owner_result.is_some_and(|r| index.needs_box(r, ty)),
                    index,
                    config,
                    sample_fn_name,
                    generic_dummy_call,
                );
                let _ = write!(out, "{field}: {val}, ");
            }
            ParamType::Flag { inner, .. } => {
                let field = rust_field_ident(&param.name);
                match inner {
                    FlagInner::True => {
                        let _ = write!(out, "{field}: {populate_flags}, ");
                    }
                    FlagInner::Type(ty) => {
                        if populate_flags {
                            let val = build_sample_value(
                                ty,
                                def,
                                Some(&param.name),
                                owner_result.is_some_and(|r| index.needs_box(r, ty)),
                                index,
                                config,
                                sample_fn_name,
                                generic_dummy_call,
                            );
                            let _ = write!(out, "{field}: Some({val}), ");
                        } else {
                            let _ = write!(out, "{field}: None, ");
                        }
                    }
                }
            }
        }
    }
    out.push('}');
    out
}

#[allow(clippy::too_many_arguments)]
fn build_sample_value(
    ty: &TypeRef,
    def: &Definition,
    param_name: Option<&str>,
    boxed_field: bool,
    index: &SchemaIndex<'_>,
    config: &GeneratorConfig,
    sample_fn_name: &dyn Fn(&str) -> String,
    generic_dummy_call: &str,
) -> String {
    let raw = if ty.generic_ref || def.generic_params.iter().any(|g| g == &ty.name) {
        format!("{generic_dummy_call} {{ nonce: [0x11; 16] }}")
    } else if ty.is_vector() {
        if let Some(arg) = ty.generic_arg.as_deref() {
            if arg.is_builtin_scalar() && arg.name != "Object" {
                let elem = build_sample_value(
                    arg,
                    def,
                    None,
                    false,
                    index,
                    config,
                    sample_fn_name,
                    generic_dummy_call,
                );
                format!("vec![{elem}]")
            } else {
                "Vec::new()".to_string()
            }
        } else {
            "Vec::new()".to_string()
        }
    } else if ty.namespace.is_none() {
        match ty.name.as_str() {
            "int" => "42i32".to_string(),
            "long" => "123456789i64".to_string(),
            "double" => "3.25f64".to_string(),
            "bytes" => "vec![1u8, 2, 3]".to_string(),
            "string" => {
                if let Some(pn) = param_name {
                    if config
                        .bytes_fields
                        .contains(&(def.full_name(), pn.to_string()))
                    {
                        "vec![0xDEu8, 0xAD, 0xBE, 0xEF]".to_string()
                    } else {
                        "\"sample\".to_string()".to_string()
                    }
                } else {
                    "\"sample\".to_string()".to_string()
                }
            }
            "int128" => "[0xAAu8; 16]".to_string(),
            "int256" => "[0x55u8; 32]".to_string(),
            "Bool" => "true".to_string(),
            "Object" => "crate::RawObject(vec![0x01, 0x02, 0x03, 0x04])".to_string(),
            _ => {
                if ty.bare {
                    if let Some(ctor) = index.resolve_bare_ctor(ty) {
                        build_sample_struct(
                            ctor,
                            Some(&ctor.result_type.full_name()),
                            false,
                            index,
                            config,
                            sample_fn_name,
                            generic_dummy_call,
                        )
                    } else {
                        format!("{}()", sample_fn_name(&ty.full_name()))
                    }
                } else {
                    format!("{}()", sample_fn_name(&ty.full_name()))
                }
            }
        }
    } else if ty.bare {
        if let Some(ctor) = index.resolve_bare_ctor(ty) {
            build_sample_struct(
                ctor,
                Some(&ctor.result_type.full_name()),
                false,
                index,
                config,
                sample_fn_name,
                generic_dummy_call,
            )
        } else {
            format!("{}()", sample_fn_name(&ty.full_name()))
        }
    } else {
        format!("{}()", sample_fn_name(&ty.full_name()))
    };

    if boxed_field {
        format!("Box::new({raw})")
    } else {
        raw
    }
}
