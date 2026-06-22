//! #37: fixed-width SIMD smoke test — exercises the public `Val::V128` boundary (host passes/
//! receives `v128`) end-to-end. The exhaustive conformance lives in the `simd_*.wast` spec suite.

#![cfg(feature = "simd")]
#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Val, V128};

/// Packs four little-endian `i32` lanes into a `v128`.
fn i32x4(a: i32, b: i32, c: i32, d: i32) -> V128 {
    let mut bytes = [0u8; 16];
    for (i, v) in [a, b, c, d].into_iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    V128::from(u128::from_le_bytes(bytes))
}

fn lanes(v: V128) -> [i32; 4] {
    let b = v.as_u128().to_le_bytes();
    std::array::from_fn(|i| i32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]))
}

#[test]
fn v128_param_and_result_roundtrip() {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (func (export "add") (param v128 v128) (result v128)
                    (i32x4.add (local.get 0) (local.get 1)))
                (func (export "mul") (param v128 v128) (result v128)
                    (i32x4.mul (local.get 0) (local.get 1))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();

    let call = |store: &mut Store<()>, name: &str, a: V128, b: V128| -> V128 {
        let f = inst.get_func(&mut *store, name).unwrap();
        let mut out = [Val::I32(0)];
        f.call(&mut *store, &[Val::V128(a), Val::V128(b)], &mut out)
            .unwrap();
        out[0].unwrap_v128()
    };

    let a = i32x4(1, 2, 3, 4);
    let b = i32x4(10, 20, 30, 40);
    assert_eq!(lanes(call(&mut store, "add", a, b)), [11, 22, 33, 44]);
    assert_eq!(lanes(call(&mut store, "mul", a, b)), [10, 40, 90, 160]);
}
