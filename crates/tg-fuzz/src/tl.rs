//! Schema-driven generator of random, well-formed TL values (TGB-3086).
//!
//! [`TlGen`] reads a `.tl` schema through `tg-tl-gen`'s parser and writes random wire bytes for
//! any boxed or bare type: a random constructor per type, random flag bits, edge-biased integers
//! and doubles, short strings with multibyte, combining and (sometimes) invalid UTF-8 bytes, and
//! vectors of a few items. The caller decodes the bytes with `tg-tl`'s normal `Deserializable`,
//! so every value is a real typed object, the same shape a converter receives from the server,
//! and the generator cannot drift from the code generator's naming or boxing rules.
//!
//! Termination: past [`TlGenConfig::max_depth`] or the node budget, every type takes its
//! cheapest constructor (a fixpoint over required, non-vector fields), vectors are empty and
//! typed optional fields are off.

use std::collections::{BTreeMap, HashMap};

use tg_tl_gen::{Category, Definition, FlagInner, ParamType, Schema, TypeRef};

use crate::{COMMON_TOKENS, Rng};

const VECTOR_ID: u32 = 0x1cb5_c415;
const BOOL_TRUE_ID: u32 = 0x9972_75b5;
const BOOL_FALSE_ID: u32 = 0xbc79_9737;

/// Limits for one generated value.
#[derive(Debug, Clone, Copy)]
pub struct TlGenConfig {
    /// Nesting depth after which only cheapest constructors, empty vectors and unset typed
    /// optional fields are generated.
    pub max_depth: usize,
    /// Upper bound (inclusive) of a vector's item count before the depth limit.
    pub max_vector: usize,
    /// One vector in this many gets up to `long_vector` items instead.
    pub long_vector_one_in: usize,
    /// Item count bound of a long vector.
    pub long_vector: usize,
    /// Constructor count after which generation behaves as past `max_depth`.
    pub max_nodes: usize,
    /// Upper bound of string tokens per string.
    pub max_string_tokens: usize,
}

impl Default for TlGenConfig {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_vector: 4,
            long_vector_one_in: 40,
            long_vector: 64,
            max_nodes: 600,
            max_string_tokens: 12,
        }
    }
}

/// Error from [`TlGen::new`].
#[derive(Debug)]
pub struct TlGenError(pub String);

impl std::fmt::Display for TlGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TlGenError {}

/// Random TL value writer for one schema.
pub struct TlGen {
    /// Boxed result type full name -> constructors.
    by_result: BTreeMap<String, Vec<Definition>>,
    /// Constructor full name -> (result type, index in `by_result`).
    ctor_index: HashMap<String, (String, usize)>,
    /// Boxed result type -> index of its cheapest constructor.
    cheapest: HashMap<String, usize>,
    config: TlGenConfig,
}

/// Per-value generation state.
struct Run<'a> {
    rng: &'a mut Rng,
    nodes: usize,
}

impl TlGen {
    /// Parses `schema_src` (e.g. `schema/api.tl`) with the default limits.
    pub fn new(schema_src: &str) -> Result<Self, TlGenError> {
        Self::with_config(schema_src, TlGenConfig::default())
    }

    /// Parses `schema_src` with explicit limits.
    pub fn with_config(schema_src: &str, config: TlGenConfig) -> Result<Self, TlGenError> {
        let schema = tg_tl_gen::parse_schema(schema_src).map_err(|e| TlGenError(e.to_string()))?;
        Ok(Self::from_schema(&schema, config))
    }

    /// Builds the generator from a parsed schema.
    pub fn from_schema(schema: &Schema, config: TlGenConfig) -> Self {
        let mut by_result: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
        for def in &schema.definitions {
            if def.category != Category::Type {
                continue;
            }
            let result = def.result_type.full_name();
            if result == "Bool" || result == "True" {
                continue;
            }
            by_result.entry(result).or_default().push(def.clone());
        }
        let mut ctor_index = HashMap::new();
        for (result, ctors) in &by_result {
            for (i, ctor) in ctors.iter().enumerate() {
                ctor_index.insert(ctor.full_name(), (result.clone(), i));
            }
        }
        let mut generator = Self {
            by_result,
            ctor_index,
            cheapest: HashMap::new(),
            config,
        };
        generator.compute_cheapest();
        generator
    }

    /// Names of every boxed result type in the schema (e.g. `"PageBlock"`, `"messages.Dialogs"`).
    pub fn type_names(&self) -> impl Iterator<Item = &str> {
        self.by_result.keys().map(String::as_str)
    }

    /// Number of constructors of the boxed type `name` (0 if unknown).
    pub fn constructor_count(&self, name: &str) -> usize {
        self.by_result.get(name).map_or(0, Vec::len)
    }

    /// Writes a random boxed value of type `name` (e.g. `"MessageMedia"`, `"Vector<PageBlock>"`).
    ///
    /// Returns `None` when `name` is not a type of the schema.
    pub fn boxed(&self, rng: &mut Rng, name: &str) -> Option<Vec<u8>> {
        let ty = parse_type_ref(name)?;
        if !self.is_known(&ty) {
            return None;
        }
        let mut out = Vec::new();
        let mut run = Run { rng, nodes: 0 };
        self.write_type(&mut run, &ty, 0, &mut out);
        Some(out)
    }

    /// Writes a random boxed value of type `name` using constructor `ctor` (full TL name, e.g.
    /// `"pageBlockTable"`), so a fuzz target can concentrate on one constructor.
    pub fn boxed_with(&self, rng: &mut Rng, name: &str, ctor: &str) -> Option<Vec<u8>> {
        let (result, index) = self.ctor_index.get(ctor)?;
        if result != name {
            return None;
        }
        let def = self.by_result.get(result)?.get(*index)?;
        let mut out = Vec::new();
        let mut run = Run { rng, nodes: 0 };
        out.extend_from_slice(&def.id.to_le_bytes());
        self.write_params(&mut run, def, 0, &mut out);
        Some(out)
    }

    fn is_known(&self, ty: &TypeRef) -> bool {
        if ty.is_vector() {
            return ty.generic_arg.as_deref().is_some_and(|a| self.is_known(a));
        }
        if ty.is_builtin_scalar() {
            return ty.name != "Object";
        }
        let full = ty.full_name();
        self.by_result.contains_key(&full) || self.ctor_index.contains_key(&full)
    }

    /// Fixpoint: a constructor's cost is 1 + the largest cost of its required, non-vector,
    /// non-scalar fields; a type's cost is its cheapest constructor's.
    fn compute_cheapest(&mut self) {
        let mut cost: HashMap<String, usize> = HashMap::new();
        loop {
            let mut changed = false;
            for (result, ctors) in &self.by_result {
                for (i, ctor) in ctors.iter().enumerate() {
                    let Some(c) = self.ctor_cost(ctor, &cost) else {
                        continue;
                    };
                    let better = cost.get(result).is_none_or(|&old| c < old);
                    if better {
                        cost.insert(result.clone(), c);
                        self.cheapest.insert(result.clone(), i);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn ctor_cost(&self, ctor: &Definition, cost: &HashMap<String, usize>) -> Option<usize> {
        let mut worst = 0usize;
        for param in &ctor.params {
            let ParamType::Normal(ty) = &param.ty else {
                continue;
            };
            if ty.is_vector() || ty.is_builtin_scalar() {
                continue;
            }
            let target = self.result_of(ty)?;
            worst = worst.max(*cost.get(&target)?);
        }
        Some(worst + 1)
    }

    /// Boxed result type a (possibly bare) type reference belongs to.
    fn result_of(&self, ty: &TypeRef) -> Option<String> {
        let full = ty.full_name();
        if self.by_result.contains_key(&full) {
            return Some(full);
        }
        self.ctor_index.get(&full).map(|(r, _)| r.clone())
    }

    fn exhausted(&self, run: &Run<'_>, depth: usize) -> bool {
        depth >= self.config.max_depth || run.nodes >= self.config.max_nodes
    }

    fn write_type(&self, run: &mut Run<'_>, ty: &TypeRef, depth: usize, out: &mut Vec<u8>) {
        if ty.is_vector() {
            self.write_vector(run, ty, depth, out);
            return;
        }
        if ty.namespace.is_none() && self.write_scalar(run, &ty.name, out) {
            return;
        }
        let full = ty.full_name();
        if ty.bare {
            // `%Type` of a single-constructor type, or a lowercase constructor name.
            let def = if let Some((result, index)) = self.ctor_index.get(&full) {
                self.by_result.get(result).and_then(|c| c.get(*index))
            } else {
                self.by_result
                    .get(&full)
                    .and_then(|c| (c.len() == 1).then(|| &c[0]))
            };
            if let Some(def) = def {
                self.write_params(run, def, depth, out);
            }
            return;
        }
        let Some(ctors) = self.by_result.get(&full) else {
            return;
        };
        let index = if self.exhausted(run, depth) {
            self.cheapest.get(&full).copied().unwrap_or(0)
        } else {
            run.rng.below(ctors.len())
        };
        let Some(def) = ctors.get(index) else {
            return;
        };
        out.extend_from_slice(&def.id.to_le_bytes());
        self.write_params(run, def, depth, out);
    }

    fn write_vector(&self, run: &mut Run<'_>, ty: &TypeRef, depth: usize, out: &mut Vec<u8>) {
        if !ty.bare {
            out.extend_from_slice(&VECTOR_ID.to_le_bytes());
        }
        let count = if self.exhausted(run, depth) {
            0
        } else if run.rng.one_in(self.config.long_vector_one_in) {
            run.rng.below(self.config.long_vector + 1)
        } else {
            run.rng.below(self.config.max_vector + 1)
        };
        out.extend_from_slice(&(count as u32).to_le_bytes());
        if let Some(arg) = ty.generic_arg.as_deref() {
            for _ in 0..count {
                self.write_type(run, arg, depth + 1, out);
            }
        }
    }

    /// Writes a built-in scalar; returns `false` when `name` is not one.
    fn write_scalar(&self, run: &mut Run<'_>, name: &str, out: &mut Vec<u8>) -> bool {
        match name {
            "int" => out.extend_from_slice(&gen_i32(run.rng).to_le_bytes()),
            "long" => out.extend_from_slice(&gen_i64(run.rng).to_le_bytes()),
            "double" => out.extend_from_slice(&gen_f64(run.rng).to_le_bytes()),
            "string" => write_tl_bytes(&gen_string(run.rng, self.config.max_string_tokens), out),
            "bytes" => {
                let len = if run.rng.one_in(20) {
                    run.rng.below(600)
                } else {
                    run.rng.below(40)
                };
                let bytes: Vec<u8> = (0..len).map(|_| run.rng.next_u64() as u8).collect();
                write_tl_bytes(&bytes, out);
            }
            "int128" => (0..16).for_each(|_| out.push(run.rng.next_u64() as u8)),
            "int256" => (0..32).for_each(|_| out.push(run.rng.next_u64() as u8)),
            "Bool" => {
                let id = if run.rng.one_in(2) {
                    BOOL_TRUE_ID
                } else {
                    BOOL_FALSE_ID
                };
                out.extend_from_slice(&id.to_le_bytes());
            }
            _ => return false,
        }
        true
    }

    fn write_params(&self, run: &mut Run<'_>, def: &Definition, depth: usize, out: &mut Vec<u8>) {
        run.nodes += 1;
        let exhausted = self.exhausted(run, depth);
        let mut flags: HashMap<&str, u32> = HashMap::new();
        for param in &def.params {
            match &param.ty {
                ParamType::Flags => {
                    let value = self.flag_word(run, def, &param.name, exhausted);
                    flags.insert(param.name.as_str(), value);
                    out.extend_from_slice(&value.to_le_bytes());
                }
                ParamType::Normal(ty) => self.write_type(run, ty, depth + 1, out),
                ParamType::Flag {
                    flag_field,
                    bit,
                    inner,
                } => {
                    let word = flags.get(flag_field.as_str()).copied().unwrap_or(0);
                    let set = *bit < 32 && word & (1 << bit) != 0;
                    if let (true, FlagInner::Type(ty)) = (set, inner) {
                        self.write_type(run, ty, depth + 1, out);
                    }
                }
            }
        }
    }

    /// A flag word: each used bit is set with probability 1/2 (typed bits are cleared once the
    /// budget is spent); one word in 16 also sets unused bits, which decoders must ignore.
    fn flag_word(&self, run: &mut Run<'_>, def: &Definition, field: &str, exhausted: bool) -> u32 {
        let mut used = 0u32;
        let mut typed = 0u32;
        for param in &def.params {
            if let ParamType::Flag {
                flag_field,
                bit,
                inner,
            } = &param.ty
                && flag_field == field
                && *bit < 32
            {
                used |= 1 << bit;
                if matches!(inner, FlagInner::Type(_)) {
                    typed |= 1 << bit;
                }
            }
        }
        let mut word = (run.rng.next_u64() as u32) & used;
        if run.rng.one_in(16) {
            word |= (run.rng.next_u64() as u32) & !used;
        }
        if exhausted {
            word &= !typed;
        }
        word
    }
}

/// Parses `"Vector<messages.Dialogs>"`, `"%Foo"` or `"PageBlock"` into a [`TypeRef`].
fn parse_type_ref(s: &str) -> Option<TypeRef> {
    let s = s.trim();
    let (bare, s) = match s.strip_prefix('%') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (head, arg) = match s.find('<') {
        Some(open) => {
            let inner = s.get(open + 1..s.len().checked_sub(1)?)?;
            if !s.ends_with('>') {
                return None;
            }
            (&s[..open], Some(Box::new(parse_type_ref(inner)?)))
        }
        None => (s, None),
    };
    let (namespace, name) = match head.rsplit_once('.') {
        Some((ns, n)) => (Some(ns.to_string()), n.to_string()),
        None => (None, head.to_string()),
    };
    if name.is_empty() {
        return None;
    }
    // Built-in scalars are spelled in lowercase but are not bare constructors.
    let scalar = matches!(
        name.as_str(),
        "int" | "long" | "double" | "string" | "bytes" | "int128" | "int256"
    );
    let lowercase = name.starts_with(|c: char| c.is_ascii_lowercase());
    Some(TypeRef {
        namespace,
        bare: bare || (lowercase && !scalar),
        name,
        generic_ref: false,
        generic_arg: arg,
    })
}

/// TL string/bytes encoding (`write_tl_bytes` in tg-tl), capped at the 24-bit length limit.
fn write_tl_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    let bytes = &bytes[..bytes.len().min(0x00FF_FFFF)];
    let len = bytes.len();
    if len < 254 {
        out.push(len as u8);
        out.extend_from_slice(bytes);
        let pad = (4 - (1 + len) % 4) % 4;
        out.extend(std::iter::repeat_n(0, pad));
    } else {
        out.push(254);
        out.extend_from_slice(&(len as u32).to_le_bytes()[..3]);
        out.extend_from_slice(bytes);
        let pad = (4 - len % 4) % 4;
        out.extend(std::iter::repeat_n(0, pad));
    }
}

const I32_EDGES: &[i32] = &[
    0,
    1,
    -1,
    2,
    7,
    8,
    10,
    100,
    255,
    256,
    1000,
    4096,
    65_535,
    65_536,
    1 << 24,
    i32::MAX - 1,
    i32::MAX,
    i32::MIN,
    i32::MIN + 1,
];

const I64_EDGES: &[i64] = &[
    0,
    1,
    -1,
    1_000_000_000,
    1 << 31,
    (1 << 32) - 1,
    1 << 32,
    1 << 40,
    1 << 52,
    1 << 53,
    i64::MAX,
    i64::MIN,
    i64::MIN + 1,
    -(1 << 40),
];

const F64_EDGES: &[f64] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    0.5,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::MAX,
    f64::MIN,
    f64::MIN_POSITIVE,
    1e-300,
    1e300,
    90.0,
    180.0,
    -91.0,
    360.5,
];

/// Small values most of the time (offsets, lengths, counts that fit short strings), an edge
/// value one time in four, a full random value one time in sixteen.
fn gen_i32(rng: &mut Rng) -> i32 {
    match rng.below(16) {
        0 => rng.next_u64() as i32,
        1..=4 => *rng.pick(I32_EDGES),
        5..=7 => -(rng.below(70) as i32),
        _ => rng.below(70) as i32,
    }
}

fn gen_i64(rng: &mut Rng) -> i64 {
    match rng.below(16) {
        0..=1 => rng.next_u64() as i64,
        2..=5 => *rng.pick(I64_EDGES),
        6 => -(rng.below(1000) as i64),
        _ => rng.below(1000) as i64,
    }
}

fn gen_f64(rng: &mut Rng) -> f64 {
    match rng.below(4) {
        0 => *rng.pick(F64_EDGES),
        1 => f64::from_bits(rng.next_u64()),
        _ => (rng.below(2_000_001) as f64 - 1_000_000.0) / 1000.0,
    }
}

/// Mostly valid UTF-8 from [`COMMON_TOKENS`]; one string in 24 gets raw invalid bytes, one in
/// 40 is long enough for the 4-byte TL length prefix.
fn gen_string(rng: &mut Rng, max_tokens: usize) -> Vec<u8> {
    let tokens = if rng.one_in(40) {
        200 + rng.below(200)
    } else {
        rng.below(max_tokens + 1)
    };
    let mut s = Vec::new();
    for _ in 0..tokens {
        s.extend_from_slice(rng.pick(COMMON_TOKENS).as_bytes());
    }
    if rng.one_in(24) {
        let at = rng.below(s.len() + 1);
        let raw = *rng.pick(&[&[0xffu8][..], &[0xc3], &[0xe2, 0x82], &[0xed, 0xa0, 0x80]]);
        s.splice(at..at, raw.iter().copied());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
---types---
boolFalse#bc799737 = Bool;
boolTrue#997275b5 = Bool;
nodeLeaf#11111111 value:int = Node;
nodeBranch#22222222 flags:# left:Node right:flags.0?Node tag:flags.1?true items:Vector<Node> = Node;
pair#33333333 a:%Node b:string = Pair;
"#;

    fn sample() -> TlGen {
        TlGen::new(SAMPLE).expect("sample schema parses")
    }

    #[test]
    fn cheapest_constructor_is_the_leaf() {
        let g = sample();
        assert_eq!(g.cheapest.get("Node"), Some(&0));
        assert_eq!(g.constructor_count("Node"), 2);
    }

    #[test]
    fn generation_terminates_and_is_deterministic() {
        let g = sample();
        for seed in 1..200 {
            let a = g
                .boxed(&mut Rng::new(seed), "Vector<Pair>")
                .expect("known type");
            let b = g
                .boxed(&mut Rng::new(seed), "Vector<Pair>")
                .expect("known type");
            assert_eq!(a, b);
            assert_eq!(a.len() % 4, 0, "TL values are 4-byte aligned");
        }
        assert!(g.boxed(&mut Rng::new(1), "Missing").is_none());
    }

    #[test]
    fn boxed_with_uses_the_requested_constructor() {
        let g = sample();
        let bytes = g
            .boxed_with(&mut Rng::new(3), "Node", "nodeBranch")
            .expect("known constructor");
        assert_eq!(bytes[..4], 0x2222_2222u32.to_le_bytes());
        assert!(
            g.boxed_with(&mut Rng::new(3), "Pair", "nodeBranch")
                .is_none()
        );
    }

    #[test]
    fn tl_bytes_use_the_long_prefix_past_253() {
        let mut out = Vec::new();
        write_tl_bytes(&[7; 300], &mut out);
        assert_eq!(out[0], 254);
        assert_eq!(out.len() % 4, 0);
    }

    #[test]
    fn parses_type_names() {
        let t = parse_type_ref("Vector<messages.Dialogs>").expect("parses");
        assert!(t.is_vector() && !t.bare);
        let inner = t.generic_arg.expect("arg");
        assert_eq!(inner.namespace.as_deref(), Some("messages"));
        assert!(!inner.bare);
        assert!(parse_type_ref("%Node").expect("parses").bare);
        assert!(!parse_type_ref("int").expect("parses").bare);
    }
}
