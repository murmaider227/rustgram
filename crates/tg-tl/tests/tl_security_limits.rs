//! Regression tests for SEC-C1: TL deserialization nesting-depth and cumulative
//! element/allocation budgets carried in `Cursor` and enforced by generated deserializers.

use tg_tl::{
    BareVec, Cursor, DEFAULT_MAX_DEPTH, DEFAULT_MAX_ELEMENTS, DecodeLimits, Deserializable,
    DeserializeError, Identifiable, Serializable, VECTOR_ID, enums, types,
};

/// Builds a wire-encoded `RichText` chain of `depth` nested `textBold` constructors wrapping
/// `textPlain("")` without constructing a deep Rust value on the stack.
fn build_nested_rich_text_bytes(depth: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(depth * 4 + 8);
    let bold_id = types::TextBold::CONSTRUCTOR_ID.to_le_bytes();
    for _ in 0..depth {
        buf.extend_from_slice(&bold_id);
    }
    // Leaf: textPlain#744694e0 with empty string [0, 0, 0, 0].
    buf.extend_from_slice(&types::TextPlain::CONSTRUCTOR_ID.to_le_bytes());
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf
}

/// Builds a wire-encoded `JSONValue` chain of `depth` nested single-element `jsonArray`
/// constructors wrapping `jsonNull`.
fn build_nested_json_array_bytes(depth: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(depth * 12 + 4);
    let arr_id = types::JsonArray::CONSTRUCTOR_ID.to_le_bytes();
    let vec_id = VECTOR_ID.to_le_bytes();
    let one = 1i32.to_le_bytes();
    for _ in 0..depth {
        buf.extend_from_slice(&arr_id);
        buf.extend_from_slice(&vec_id);
        buf.extend_from_slice(&one);
    }
    buf.extend_from_slice(&types::JsonNull::CONSTRUCTOR_ID.to_le_bytes());
    buf
}

#[test]
fn deeply_nested_rich_text_returns_max_depth_error_instead_of_stack_overflow() {
    // Valid nesting well within DEFAULT_MAX_DEPTH succeeds.
    let shallow = build_nested_rich_text_bytes(10);
    assert!(enums::RichText::from_bytes(&shallow).is_ok());

    // Deeply nested RichText (10,000 levels: would blow a 1-2 MiB thread stack without depth guard)
    // must cleanly return MaxDepthExceeded.
    let deep = build_nested_rich_text_bytes(10_000);
    assert_eq!(
        enums::RichText::from_bytes(&deep),
        Err(DeserializeError::MaxDepthExceeded {
            limit: DEFAULT_MAX_DEPTH,
        })
    );
}

#[test]
fn deeply_nested_json_value_returns_max_depth_error_instead_of_stack_overflow() {
    let shallow = build_nested_json_array_bytes(8);
    assert!(enums::JSONValue::from_bytes(&shallow).is_ok());

    // 5,000 nested jsonArray layers must return MaxDepthExceeded instead of overflowing the stack.
    let deep = build_nested_json_array_bytes(5_000);
    assert_eq!(
        enums::JSONValue::from_bytes(&deep),
        Err(DeserializeError::MaxDepthExceeded {
            limit: DEFAULT_MAX_DEPTH,
        })
    );
}

#[test]
fn sibling_subtrees_unwind_depth_and_succeed() {
    // 200 sibling subtrees each nested 5 deep must NOT accumulate depth across siblings.
    let mut buf = Vec::new();
    buf.extend_from_slice(&types::JsonArray::CONSTRUCTOR_ID.to_le_bytes());
    buf.extend_from_slice(&VECTOR_ID.to_le_bytes());
    buf.extend_from_slice(&200i32.to_le_bytes());
    let child = build_nested_json_array_bytes(5);
    for _ in 0..200 {
        buf.extend_from_slice(&child);
    }
    let decoded = enums::JSONValue::from_bytes(&buf).expect("wide shallow jsonArray must decode");
    match decoded {
        enums::JSONValue::JsonArray(arr) => assert_eq!(arr.value.len(), 200),
        other => panic!("expected JsonArray, got {other:?}"),
    }
}

#[test]
fn cumulative_element_budget_rejects_zero_byte_bare_vector_bomb() {
    // A bare vector of a 0-byte bare unit constructor (`types::JsonNull`) claiming 1,000,000 items
    // fits in just 4 bytes on the wire! Without a cumulative element budget it would loop
    // 1,000,000 times and succeed on 4 bytes of input.
    let wire = 1_000_000i32.to_le_bytes();
    let res = BareVec::<types::JsonNull>::from_bytes(&wire);
    assert_eq!(
        res,
        Err(DeserializeError::BudgetExceeded {
            kind: "elements",
            limit: DEFAULT_MAX_ELEMENTS,
        })
    );

    // Custom tight element budget via Cursor::with_limits is enforced across nested fields too.
    let mut arr_wire = Vec::new();
    arr_wire.extend_from_slice(&types::JsonArray::CONSTRUCTOR_ID.to_le_bytes());
    arr_wire.extend_from_slice(&VECTOR_ID.to_le_bytes());
    arr_wire.extend_from_slice(&20i32.to_le_bytes());
    for _ in 0..20 {
        arr_wire.extend_from_slice(&types::JsonNull::CONSTRUCTOR_ID.to_le_bytes());
    }
    let limits = DecodeLimits {
        max_depth: DEFAULT_MAX_DEPTH,
        max_elements: 10,
        max_alloc_bytes: 1024 * 1024,
    };
    let mut cur = Cursor::with_limits(&arr_wire, limits);
    assert_eq!(
        enums::JSONValue::deserialize(&mut cur),
        Err(DeserializeError::BudgetExceeded {
            kind: "elements",
            limit: 10,
        })
    );
}

#[test]
fn cumulative_allocation_budget_rejects_oversized_allocations() {
    // Encode a Vector<bytes> of twenty 4 KiB blobs (80 KiB total payload).
    let blobs: Vec<Vec<u8>> = (0..20).map(|i| vec![i as u8; 4096]).collect();
    let wire = blobs.to_bytes();

    // Decoding with a 32 KiB allocation budget must fail with BudgetExceeded("bytes").
    let tight = DecodeLimits {
        max_depth: DEFAULT_MAX_DEPTH,
        max_elements: DEFAULT_MAX_ELEMENTS,
        max_alloc_bytes: 32 * 1024,
    };
    let mut cur = Cursor::with_limits(&wire, tight);
    assert_eq!(
        Vec::<Vec<u8>>::deserialize(&mut cur),
        Err(DeserializeError::BudgetExceeded {
            kind: "bytes",
            limit: 32 * 1024,
        })
    );

    // Decoding with default limits succeeds.
    assert_eq!(Vec::<Vec<u8>>::from_bytes(&wire).unwrap(), blobs);
}
