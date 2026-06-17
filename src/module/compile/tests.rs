//! Unit tests for the translator (#7 straight-line + #8 control flow).
#![allow(clippy::unwrap_used)]

use super::{translate_function, wp_err, CompileCtx};
use crate::engine::Engine;
use crate::module::op::{BranchTarget, Op};
use crate::module::Module;
use crate::value::{FuncType, ValType};
use crate::Result;
use wasmparser::{Parser, Payload};

type Sig<'a> = (&'a [ValType], &'a [ValType]);

/// Validates `wat`, then translates its first function body using the given
/// type section (`sigs`), function-index→type map (`func_types`), and the
/// compiled function's own type index.
fn compile_one(
    wat: &str,
    sigs: &[Sig<'_>],
    func_types: &[u32],
    type_idx: u32,
) -> Result<Box<[Op]>> {
    let engine = Engine::default();
    let bytes = wat::parse_str(wat).expect("valid wat");
    Module::validate(&engine, &bytes)?;
    let types: Vec<FuncType> = sigs
        .iter()
        .map(|(p, r)| FuncType::new(&engine, p.iter().cloned(), r.iter().cloned()))
        .collect();
    let ctx = CompileCtx {
        types: &types,
        func_types,
    };
    for payload in Parser::new(0).parse_all(&bytes) {
        if let Payload::CodeSectionEntry(body) = payload.map_err(wp_err)? {
            return Ok(translate_function(&ctx, type_idx, &body)?.ops);
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
