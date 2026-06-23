//! Unit tests for the translator (straight-line + control flow).
#![allow(clippy::unwrap_used)]

use super::{translate_function, wp_err, CompileCtx};
use crate::canon::{AggKind, CompositeBody, IrVal, ModuleType};
use crate::engine::Engine;
use crate::module::op::{BranchTarget, Op};
use crate::value::{Finality, ValType};
use crate::Result;
use wasmparser::{FuncValidatorAllocations, Parser, ValidPayload, Validator};

type Sig<'a> = (&'a [ValType], &'a [ValType]);

/// Maps a (numeric) public `ValType` to module IR — these tests only use numeric signatures.
fn ir(tys: &[ValType]) -> Vec<IrVal> {
    tys.iter()
        .map(|t| match t {
            ValType::I32 => IrVal::I32,
            ValType::I64 => IrVal::I64,
            ValType::F32 => IrVal::F32,
            ValType::F64 => IrVal::F64,
            ValType::V128 => IrVal::V128,
            ValType::Ref(_) => unreachable!("translator tests use only numeric signatures"),
        })
        .collect()
}

/// Validates `wat`, then translates its first function body using the given
/// type section (`sigs`), function-index→type map (`func_types`), and the
/// compiled function's own type index.
fn compile_one(
    wat: &str,
    sigs: &[Sig<'_>],
    func_types: &[u32],
    type_idx: u32,
) -> Result<Box<[Op]>> {
    let _engine = Engine::default();
    let bytes = wat::parse_str(wat).expect("valid wat");
    let types: Vec<ModuleType> = sigs
        .iter()
        .enumerate()
        .map(|(i, (p, r))| ModuleType {
            group: i as u32,
            finality: Finality::Final,
            supertype: None,
            body: CompositeBody::Func {
                params: ir(p),
                results: ir(r),
            },
        })
        .collect();
    let kinds = vec![AggKind::Func; types.len()];
    let ctx = CompileCtx {
        types: &types,
        kinds: &kinds,
        func_types,
        tag_types: &[],
    };
    let mut validator = Validator::new_with_features(crate::module::enabled_features());
    for payload in Parser::new(0).parse_all(&bytes) {
        let payload = payload.map_err(wp_err)?;
        if let ValidPayload::Func(to_validate, body) =
            validator.payload(&payload).map_err(wp_err)?
        {
            let mut fv = to_validate.into_validator(FuncValidatorAllocations::default());
            return Ok(translate_function(&ctx, type_idx, &body, &mut fv, false)?.ops);
        }
    }
    Err(crate::Error::msg("no function body"))
}

fn one(sig: Sig<'_>) -> ([Sig<'_>; 1], [u32; 1]) {
    ([sig], [0])
}

#[test]
fn straight_line_add() {
    let (sigs, ft) = one((&[ValType::I32, ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    assert!(matches!(
        ops.as_ref(),
        [Op::LocalGet(0), Op::LocalGet(1), Op::I32Add]
    ));
}

#[test]
fn memory_load_store() {
    let (sigs, ft) = one((&[ValType::I32], &[]));
    let ops = compile_one(
        "(module (memory 1) (func (param i32) i32.const 0 local.get 0 i32.store))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    assert!(matches!(
        ops.as_ref(),
        [Op::I32Const(0), Op::LocalGet(0), Op::I32Store(_)]
    ));
}

#[test]
fn block_forward_branch() {
    let (sigs, ft) = one((&[], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (result i32) (block (result i32) i32.const 1 br 0)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // I32Const(1), Br -> end (ip == ops.len()), transferring the 1 result.
    assert_eq!(ops.len(), 2);
    assert!(matches!(ops[0], Op::I32Const(1)));
    let Op::Br(BranchTarget { ip, keep, pop }) = ops[1] else {
        panic!("expected Br")
    };
    assert_eq!((ip, keep, pop), (2, 1, 0));
}

#[test]
fn loop_label_uses_param_arity() {
    let (sigs, ft) = one((&[ValType::I32], &[]));
    let ops = compile_one(
        "(module (func (param i32) local.get 0 (loop (param i32) br 0)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // LocalGet, Br backward to loop start (ip == 1) with keep == param_count == 1.
    let Op::Br(BranchTarget { ip, keep, .. }) = ops[1] else {
        panic!("expected Br")
    };
    assert_eq!((ip, keep), (1, 1));
}

#[test]
fn if_else_targets() {
    let (sigs, ft) = one((&[ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32) (result i32) local.get 0 \
            (if (result i32) (then i32.const 1) (else i32.const 2))))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // LocalGet, BrIfNot->else-start, I32Const(1), Br->end, I32Const(2)
    let Op::BrIfNot(else_t) = ops[1] else {
        panic!("expected BrIfNot")
    };
    assert_eq!(else_t.ip, 4); // else body starts at index 4
    let Op::Br(end_t) = ops[3] else {
        panic!("expected Br")
    };
    assert_eq!(end_t.ip, 5); // end is past the last op
}

#[test]
fn br_table_uniform_keep() {
    let (sigs, ft) = one((&[ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32) (result i32) \
            (block (result i32) i32.const 7 local.get 0 br_table 0 0)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    let last = ops.last().unwrap();
    let Op::BrTable { targets, default } = last else {
        panic!("expected BrTable")
    };
    assert_eq!(default.keep, 1);
    assert!(targets.iter().all(|t| t.keep == 1));
}

#[test]
fn call_stack_effect() {
    let ops = compile_one(
        "(module \
            (func (param i32 i32) (result i32) local.get 0 local.get 1 call 1) \
            (func (param i32 i32) (result i32) local.get 0))",
        &[(&[ValType::I32, ValType::I32], &[ValType::I32])],
        &[0, 0],
        0,
    )
    .unwrap();
    assert!(matches!(
        ops.as_ref(),
        [Op::LocalGet(0), Op::LocalGet(1), Op::Call(1)]
    ));
}

#[test]
fn return_branches_to_function_end() {
    let (sigs, ft) = one((&[ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32) (result i32) local.get 0 return))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    let Op::Br(BranchTarget { ip, keep, .. }) = ops[1] else {
        panic!("expected Br for return")
    };
    assert_eq!(ip as usize, ops.len()); // function end
    assert_eq!(keep, 1);
}

#[test]
fn dead_code_after_branch_is_elided() {
    let (sigs, ft) = one((&[], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (result i32) (block (result i32) i32.const 1 br 0 i32.const 99 drop)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // The `i32.const 99` / `drop` after `br 0` are not emitted.
    assert_eq!(ops.len(), 2);
    assert!(matches!(ops[0], Op::I32Const(1)));
    assert!(matches!(ops[1], Op::Br(_)));
}

#[test]
fn block_now_compiles() {
    let (sigs, ft) = one((&[], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (result i32) (block (result i32) i32.const 1)))",
        &sigs,
        &ft,
        0,
    );
    assert!(ops.is_ok());
}
