//! Build script for `tg-tl`: parses `schema/mtproto.tl` and `schema/api.tl`
//! and writes generated Rust modules into `OUT_DIR`.

use std::env;
use std::fs;
use std::path::PathBuf;

use tg_tl_gen::{
    GeneratorConfig, generate_constructor_id_table, generate_roundtrip_test, generate_schema,
    mtproto_bytes_fields, parse_schema,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    let schema_dir = manifest_dir.join("../../schema");
    let mtproto_path = schema_dir.join("mtproto.tl");
    let api_path = schema_dir.join("api.tl");

    println!("cargo:rerun-if-changed={}", mtproto_path.display());
    println!("cargo:rerun-if-changed={}", api_path.display());

    let mtproto_src = fs::read_to_string(&mtproto_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", mtproto_path.display()));
    let api_src = fs::read_to_string(&api_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", api_path.display()));

    let mtproto_schema =
        parse_schema(&mtproto_src).unwrap_or_else(|e| panic!("failed to parse mtproto.tl: {e}"));
    let api_schema =
        parse_schema(&api_src).unwrap_or_else(|e| panic!("failed to parse api.tl: {e}"));

    let api_cfg = GeneratorConfig {
        module_prefix: "crate".to_string(),
        bytes_fields: Default::default(),
    };
    let mtproto_cfg = GeneratorConfig {
        module_prefix: "crate::mtproto".to_string(),
        bytes_fields: mtproto_bytes_fields(),
    };

    let api_code = generate_schema(&api_schema, &api_cfg);
    let mtproto_code = generate_schema(&mtproto_schema, &mtproto_cfg);

    let extra_ids: &[(u32, &str)] = &[
        (0x1cb5_c415, "vector"),
        (0xbc79_9737, "boolFalse"),
        (0x9972_75b5, "boolTrue"),
        (0xf35c_6d01, "rpc_result"),
        (0x73f1_f8dc, "msg_container"),
        (0xe060_46b2, "msg_copy"),
        (0x3072_cfa1, "gzip_packed"),
    ];
    let id_table_code = generate_constructor_id_table(&[&mtproto_schema, &api_schema], extra_ids);

    let rt_api_code = generate_roundtrip_test(
        &api_schema,
        &api_cfg,
        "generated_roundtrip_api",
        "crate::mtproto::functions::ReqPqMulti",
    );
    let rt_mtproto_code = generate_roundtrip_test(
        &mtproto_schema,
        &mtproto_cfg,
        "generated_roundtrip_mtproto",
        "crate::mtproto::functions::ReqPqMulti",
    );

    fs::write(out_dir.join("api.rs"), api_code).expect("write api.rs");
    fs::write(out_dir.join("mtproto.rs"), mtproto_code).expect("write mtproto.rs");
    fs::write(out_dir.join("constructor_ids.rs"), id_table_code).expect("write constructor_ids.rs");
    fs::write(out_dir.join("roundtrip_api.rs"), rt_api_code).expect("write roundtrip_api.rs");
    fs::write(out_dir.join("roundtrip_mtproto.rs"), rt_mtproto_code)
        .expect("write roundtrip_mtproto.rs");
}
