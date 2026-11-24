//! Abstract syntax tree for parsed `.tl` schemas.
//!
//! Port of schema representation concepts from `lib_tl/tl/generate_tl.py`.

use std::fmt;

/// A parsed `.tl` schema file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// All non-builtin type constructors and RPC functions in declaration order.
    pub definitions: Vec<Definition>,
    /// API layer extracted from a `// LAYER <n>` comment, if present.
    pub layer: Option<i32>,
}

/// Whether a TL definition belongs to `---types---` or `---functions---`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Type constructor (`---types---`).
    Type,
    /// RPC function (`---functions---`).
    Function,
}

/// A single TL constructor or function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Whether this is a type constructor or an RPC function.
    pub category: Category,
    /// Optional dot-separated namespace (e.g. `Some("messages")` for `messages.getDialogs`).
    pub namespace: Option<String>,
    /// Unqualified name (e.g. `"getDialogs"` or `"inputPeerUser"`).
    pub name: String,
    /// Effective 32-bit constructor ID (explicit `#hex` if present, otherwise CRC32).
    pub id: u32,
    /// Explicit `#hex` ID written in the `.tl` file, if any.
    pub explicit_id: Option<u32>,
    /// Generic type parameter names declared via `{X:Type}`.
    pub generic_params: Vec<String>,
    /// Ordered list of parameters.
    pub params: Vec<Param>,
    /// Result type on the right-hand side of `=`.
    pub result_type: TypeRef,
    /// 1-based source line number.
    pub line: usize,
}

impl Definition {
    /// Returns the full dotted TL name (e.g. `"messages.getDialogs"` or `"user"`).
    pub fn full_name(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Computes the CRC32 constructor ID from the normalized definition.
    pub fn computed_id(&self) -> u32 {
        crate::crc32::compute_constructor_id(self)
    }
}

/// A single parameter in a TL definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Parameter name as written in `.tl`.
    pub name: String,
    /// Parameter type specification.
    pub ty: ParamType,
}

/// Parameter type kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamType {
    /// Bitmask flag field (`#`, e.g. `flags:#`, `flags2:#`).
    Flags,
    /// Unconditional typed parameter (including generic `!X`).
    Normal(TypeRef),
    /// Conditional parameter guarded by a flag bit (`flags.N?true` or `flags.N?Type`).
    Flag {
        /// Name of the flag field (e.g. `"flags"` or `"flags2"`).
        flag_field: String,
        /// Bit index (`0..32`).
        bit: u32,
        /// Conditional payload type or presence-only `true`.
        inner: FlagInner,
    },
}

/// Payload of a conditional `flags.N?...` parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagInner {
    /// Presence-only boolean flag (`flags.N?true`).
    True,
    /// Conditionally serialized typed value (`flags.N?T`).
    Type(TypeRef),
}

/// A reference to a TL type (possibly bare, namespaced, generic, or parameterized).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeRef {
    /// Optional namespace (e.g. `Some("storage")` in `storage.FileType`).
    pub namespace: Option<String>,
    /// Short type name (e.g. `"int"`, `"Vector"`, `"vector"`, `"InputPeer"`, `"future_salt"`, `"X"`).
    pub name: String,
    /// Whether the reference is bare (`%Type`, lowercase `vector`, or lowercase constructor name).
    pub bare: bool,
    /// Whether the reference is a generic function-argument reference (`!X`).
    pub generic_ref: bool,
    /// Optional inner type argument (for `Vector<T>` / `vector<T>`).
    pub generic_arg: Option<Box<TypeRef>>,
}

impl TypeRef {
    /// Returns the full dotted type name (without `<...>`).
    pub fn full_name(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Returns `true` if this is a TL built-in scalar primitive (`int`, `long`, `double`,
    /// `string`, `bytes`, `int128`, `int256`, `Bool`, `Object`).
    pub fn is_builtin_scalar(&self) -> bool {
        self.namespace.is_none()
            && matches!(
                self.name.as_str(),
                "int"
                    | "long"
                    | "double"
                    | "string"
                    | "bytes"
                    | "int128"
                    | "int256"
                    | "Bool"
                    | "Object"
            )
    }

    /// Returns `true` if this is `Vector<T>` or bare `vector<T>`.
    pub fn is_vector(&self) -> bool {
        self.namespace.is_none() && (self.name == "Vector" || self.name == "vector")
    }
}

/// Error returned when parsing a `.tl` schema fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number where the error occurred.
    pub line: usize,
    /// Human-readable description of the syntax error.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TL parse error on line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}
