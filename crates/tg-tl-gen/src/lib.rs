//! `tg-tl-gen`: Telegram TL (`.tl`) schema parser and Rust code generator.
//!
//! Port of `Telegram/lib_tl/tl/generate_tl.py` and `Telegram/SourceFiles/codegen/scheme/codegen_scheme.py`.
//! Used as a build-dependency of `tg-tl`.

pub mod ast;
pub mod codegen;
pub mod crc32;
pub mod parser;

pub use ast::{Category, Definition, FlagInner, Param, ParamType, ParseError, Schema, TypeRef};
pub use codegen::{
    GeneratorConfig, enum_variant_name, generate_constructor_id_table, generate_roundtrip_test,
    generate_schema, rust_field_ident, to_pascal_case,
};
pub use crc32::{compute_constructor_id, crc32, normalize_definition_for_crc};
pub use parser::parse_schema;

/// Returns the set of `(ctor_name, param_name)` pairs in `schema/mtproto.tl` whose `:string`
/// parameters carry raw binary data (RSA/DH blobs, bitmasks, TLS bytes) rather than UTF-8 text.
///
/// Leaving `rpc_error.error_message` and `accessPointRule.phone_prefix_rules` as `String`.
pub fn mtproto_bytes_fields() -> std::collections::HashSet<(String, String)> {
    const FIELDS: &[(&str, &str)] = &[
        ("resPQ", "pq"),
        ("p_q_inner_data", "pq"),
        ("p_q_inner_data", "p"),
        ("p_q_inner_data", "q"),
        ("p_q_inner_data_dc", "pq"),
        ("p_q_inner_data_dc", "p"),
        ("p_q_inner_data_dc", "q"),
        ("p_q_inner_data_temp", "pq"),
        ("p_q_inner_data_temp", "p"),
        ("p_q_inner_data_temp", "q"),
        ("p_q_inner_data_temp_dc", "pq"),
        ("p_q_inner_data_temp_dc", "p"),
        ("p_q_inner_data_temp_dc", "q"),
        ("server_DH_params_ok", "encrypted_answer"),
        ("server_DH_inner_data", "dh_prime"),
        ("server_DH_inner_data", "g_a"),
        ("client_DH_inner_data", "g_b"),
        ("req_DH_params", "p"),
        ("req_DH_params", "q"),
        ("req_DH_params", "encrypted_data"),
        ("set_client_DH_params", "encrypted_data"),
        ("msgs_state_info", "info"),
        ("msgs_all_info", "info"),
        ("tlsBlockString", "data"),
    ];
    FIELDS
        .iter()
        .map(|&(c, p)| (c.to_string(), p.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Historical MTProto schema constructor IDs that predate modern CRC32 normalization
    /// (identical to `typeIdExceptions` in `codegen/scheme/codegen_scheme.py`).
    const MTPROTO_CRC_EXCEPTIONS: &[(&str, u32)] = &[
        ("ipPortSecret", 0x3798_2646),
        ("accessPointRule", 0x4679_b65f),
        ("help.configSimple", 0x5a59_2a6c),
    ];

    #[test]
    fn parse_and_verify_crc32_api_tl() {
        let src = include_str!("../../../schema/api.tl");
        let schema = parse_schema(src).expect("api.tl must parse");
        assert_eq!(schema.layer, Some(233));
        assert!(schema.definitions.len() > 2500);

        for def in &schema.definitions {
            let explicit = def.explicit_id.unwrap_or_else(|| {
                panic!(
                    "expected explicit id on api.tl line {} ({})",
                    def.line,
                    def.full_name()
                )
            });
            let computed = def.computed_id();
            assert_eq!(
                computed,
                explicit,
                "CRC32 mismatch on api.tl line {} ({}): normalized = `{}`",
                def.line,
                def.full_name(),
                normalize_definition_for_crc(def)
            );
        }
    }

    #[test]
    fn parse_and_verify_crc32_mtproto_tl() {
        let src = include_str!("../../../schema/mtproto.tl");
        let schema = parse_schema(src).expect("mtproto.tl must parse");
        assert_eq!(schema.layer, None);
        assert!(schema.definitions.len() >= 60);

        for def in &schema.definitions {
            let computed = def.computed_id();
            if let Some(explicit) = def.explicit_id {
                let full = def.full_name();
                if MTPROTO_CRC_EXCEPTIONS
                    .iter()
                    .any(|&(n, id)| n == full && id == explicit)
                {
                    continue;
                }
                assert_eq!(
                    computed,
                    explicit,
                    "CRC32 mismatch on mtproto.tl line {} ({}): normalized = `{}`",
                    def.line,
                    full,
                    normalize_definition_for_crc(def)
                );
            } else {
                // Missing explicit IDs (e.g. `tlsClientHello`, `tlsBlock*`) must use computed CRC32.
                assert_eq!(def.id, computed);
                assert_ne!(def.id, 0);
            }
        }
    }

    #[test]
    fn parse_bare_percent_and_object_and_generics() {
        let sample = r#"
// Comment line
---types---
foo a:int b:%Bar c:Object = Foo;
bar#12345678 x:long = Bar;
multi flags:# a:flags.0?true flags2:# b:flags2.3?string = Multi;
---functions---
wrap {X:Type} flags:# opt:flags.0?int query:!X = X;
// LAYER 999
"#;
        let schema = parse_schema(sample).expect("sample schema parses");
        assert_eq!(schema.layer, Some(999));
        assert_eq!(schema.definitions.len(), 4);

        let foo = &schema.definitions[0];
        assert!(foo.explicit_id.is_none());
        assert_eq!(foo.id, foo.computed_id());
        match &foo.params[1].ty {
            ParamType::Normal(t) => {
                assert!(t.bare);
                assert_eq!(t.name, "Bar");
            }
            other => panic!("expected Normal bare type, got {other:?}"),
        }
        match &foo.params[2].ty {
            ParamType::Normal(t) => {
                assert_eq!(t.name, "Object");
                assert!(t.is_builtin_scalar());
            }
            other => panic!("expected Normal Object, got {other:?}"),
        }

        let wrap = &schema.definitions[3];
        assert_eq!(wrap.category, Category::Function);
        assert_eq!(wrap.generic_params, vec!["X"]);
        assert_eq!(wrap.result_type.name, "X");
    }
}
