//! Runtime tests: execute arithmetic-free, instance-free functions.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use super::host;
use crate::canon::{AggKind, CompositeBody, IrVal, ModuleType};
use crate::engine::Engine;
use crate::instance::Instance;
use crate::module::compile::{translate_function, CompileCtx};
use crate::store::Store;
use crate::trap::Trap;
use crate::value::{Finality, Val, ValType};
use crate::Result;
use wasmparser::{FuncValidatorAllocations, Parser, ValidPayload, Validator};

/// Maps a (numeric) public `ValType` to module IR — these tests only use numeric signatures.
fn ir(tys: &[ValType]) -> Vec<IrVal> {
    tys.iter()
        .map(|t| match t {
            ValType::I32 => IrVal::I32,
            ValType::I64 => IrVal::I64,
            ValType::F32 => IrVal::F32,
            ValType::F64 => IrVal::F64,
            ValType::V128 => IrVal::V128,
            ValType::Ref(_) => unreachable!("runtime tests use only numeric signatures"),
        })
        .collect()
}

/// Compiles `wat`'s single function and runs it with `args`, returning the results.
fn run_wat(wat: &str, params: &[ValType], results: &[ValType], args: Vec<Val>) -> Result<Vec<Val>> {
    let engine = Engine::default();
    let bytes = wat::parse_str(wat).unwrap();
    let types = [ModuleType {
        group: 0,
        finality: Finality::Final,
        supertype: None,
        body: CompositeBody::Func {
            params: ir(params),
            results: ir(results),
        },
    }];
    let kinds = [AggKind::Func];
    let ctx = CompileCtx {
        types: &types,
        kinds: &kinds,
        func_types: &[0],
        tag_types: &[],
    };
    let mut code = None;
    let mut validator = Validator::new_with_features(crate::module::enabled_features());
    for payload in Parser::new(0).parse_all(&bytes) {
        let payload = payload.unwrap();
        if let ValidPayload::Func(to_validate, body) = validator.payload(&payload).unwrap() {
            let mut fv = to_validate.into_validator(FuncValidatorAllocations::default());
            code = Some(translate_function(&ctx, 0, &body, &mut fv, false)?);
        }
    }
    let mut store = Store::new(&engine, ());
    host::execute(
        &mut store,
        Instance { index: 0 },
        0,
        Arc::new(code.unwrap()),
        args,
        results,
    )
}

fn run_i32(wat: &str, params: &[ValType], args: Vec<Val>) -> i32 {
    run_wat(wat, params, &[ValType::I32], args).unwrap()[0].unwrap_i32()
}

#[test]
fn const_return() {
    assert_eq!(
        run_i32("(module (func (result i32) i32.const 42))", &[], vec![]),
        42
    );
}

#[test]
fn local_get_param() {
    let got = run_i32(
        "(module (func (param i32 i32) (result i32) local.get 1))",
        &[ValType::I32, ValType::I32],
        vec![Val::I32(7), Val::I32(9)],
    );
    assert_eq!(got, 9);
}

#[test]
fn local_tee_and_drop() {
    let got = run_i32(
        "(module (func (result i32) (local i32) i32.const 5 local.tee 0 drop local.get 0))",
        &[],
        vec![],
    );
    assert_eq!(got, 5);
}

#[test]
fn select_picks_second_when_zero() {
    let got = run_i32(
        "(module (func (result i32) i32.const 10 i32.const 20 i32.const 0 select))",
        &[],
        vec![],
    );
    assert_eq!(got, 20);
}

#[test]
fn multi_value_results() {
    let res = run_wat(
        "(module (func (result i32 i32) i32.const 1 i32.const 2))",
        &[],
        &[ValType::I32, ValType::I32],
        vec![],
    )
    .unwrap();
    assert_eq!(res[0].unwrap_i32(), 1);
    assert_eq!(res[1].unwrap_i32(), 2);
}

#[test]
fn block_branch() {
    let got = run_i32(
        "(module (func (result i32) (block (result i32) i32.const 5 br 0)))",
        &[],
        vec![],
    );
    assert_eq!(got, 5);
}

#[test]
fn if_else_both_arms() {
    let wat = "(module (func (param i32) (result i32) local.get 0 \
        (if (result i32) (then i32.const 1) (else i32.const 2))))";
    assert_eq!(run_i32(wat, &[ValType::I32], vec![Val::I32(1)]), 1);
    assert_eq!(run_i32(wat, &[ValType::I32], vec![Val::I32(0)]), 2);
}

#[test]
fn loop_fallthrough() {
    let got = run_i32(
        "(module (func (result i32) (loop (result i32) i32.const 7)))",
        &[],
        vec![],
    );
    assert_eq!(got, 7);
}

#[test]
fn explicit_return() {
    let got = run_i32(
        "(module (func (param i32) (result i32) local.get 0 return))",
        &[ValType::I32],
        vec![Val::I32(3)],
    );
    assert_eq!(got, 3);
}

fn const_i32(wat_body: &str) -> i32 {
    run_i32(
        &format!("(module (func (result i32) {wat_body}))"),
        &[],
        vec![],
    )
}

fn trap_of(wat_body: &str) -> Trap {
    let err = run_wat(
        &format!("(module (func (result i32) {wat_body}))"),
        &[],
        &[ValType::I32],
        vec![],
    )
    .unwrap_err();
    *err.downcast_ref::<Trap>().expect("a Trap")
}

#[test]
fn integer_arithmetic() {
    assert_eq!(const_i32("i32.const 5 i32.const 3 i32.add"), 8);
    assert_eq!(const_i32("i32.const 5 i32.const 3 i32.sub"), 2);
    assert_eq!(const_i32("i32.const 6 i32.const 7 i32.mul"), 42);
    assert_eq!(const_i32("i32.const 0xff i32.const 0x0f i32.and"), 0x0f);
    assert_eq!(const_i32("i32.const 1 i32.const 4 i32.shl"), 16);
    assert_eq!(const_i32("i32.const -1 i32.const 1 i32.shr_u"), i32::MAX);
    assert_eq!(const_i32("i32.const 1 i32.const 2 i32.lt_s"), 1);
    assert_eq!(const_i32("i32.const -1 i32.const 1 i32.lt_u"), 0);
}

#[test]
fn integer_div_traps() {
    assert!(matches!(
        trap_of("i32.const 1 i32.const 0 i32.div_s"),
        Trap::IntegerDivisionByZero
    ));
    assert!(matches!(
        trap_of("i32.const -2147483648 i32.const -1 i32.div_s"),
        Trap::IntegerOverflow
    ));
    // INT_MIN % -1 is 0, not a trap.
    assert_eq!(const_i32("i32.const -2147483648 i32.const -1 i32.rem_s"), 0);
}

#[test]
fn i64_division() {
    let got = run_wat(
        "(module (func (result i64) i64.const 10 i64.const 3 i64.div_u))",
        &[],
        &[ValType::I64],
        vec![],
    )
    .unwrap();
    assert_eq!(got[0].unwrap_i64(), 3);
}

#[test]
fn float_arithmetic() {
    let add = run_wat(
        "(module (func (result f64) f64.const 1.5 f64.const 2.25 f64.add))",
        &[],
        &[ValType::F64],
        vec![],
    )
    .unwrap();
    assert_eq!(add[0].unwrap_f64().to_bits(), 3.75_f64.to_bits());

    let sqrt = run_wat(
        "(module (func (result f32) f32.const 4 f32.sqrt))",
        &[],
        &[ValType::F32],
        vec![],
    )
    .unwrap();
    assert_eq!(sqrt[0].unwrap_f32().to_bits(), 2.0_f32.to_bits());

    let copysign = run_wat(
        "(module (func (result f32) f32.const 1 f32.const -2 f32.copysign))",
        &[],
        &[ValType::F32],
        vec![],
    )
    .unwrap();
    assert_eq!(copysign[0].unwrap_f32().to_bits(), (-1.0_f32).to_bits());
}

#[test]
fn float_min_canonical_nan() {
    let res = run_wat(
        "(module (func (result f32) f32.const nan f32.const 1 f32.min))",
        &[],
        &[ValType::F32],
        vec![],
    )
    .unwrap();
    assert_eq!(res[0].unwrap_f32().to_bits(), 0x7fc0_0000);
}

#[test]
fn conversions() {
    assert_eq!(const_i32("f32.const 3.9 i32.trunc_f32_s"), 3);
    assert!(matches!(
        trap_of("f32.const nan i32.trunc_f32_s"),
        Trap::BadConversionToInteger
    ));
    assert!(matches!(
        trap_of("f32.const 1e30 i32.trunc_f32_s"),
        Trap::IntegerOverflow
    ));
    assert_eq!(const_i32("f32.const 1e30 i32.trunc_sat_f32_s"), i32::MAX);
    assert_eq!(const_i32("f32.const nan i32.trunc_sat_f32_s"), 0);
    assert_eq!(const_i32("f32.const 1.0 i32.reinterpret_f32"), 0x3f80_0000);
    assert_eq!(const_i32("i32.const 255 i32.extend8_s"), -1);

    let ext = run_wat(
        "(module (func (result i64) i32.const -1 i64.extend_i32_s))",
        &[],
        &[ValType::I64],
        vec![],
    )
    .unwrap();
    assert_eq!(ext[0].unwrap_i64(), -1);
}
