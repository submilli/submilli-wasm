//! Unit tests for the translator (straight-line + control flow).
#![allow(clippy::unwrap_used)]

use super::{translate_function, wp_err, CompileCtx};
use crate::canon::{AggKind, CompositeBody, IrVal, ModuleType};
use crate::engine::Engine;
use crate::module::op::{BranchTarget, CmpKind, Op, StoreArg};
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
fn compile_one(wat: &str, sigs: &[Sig<'_>], func_types: &[u32], type_idx: u32) -> Result<Vec<Op>> {
    let (func, code) = compile_func(wat, sigs, func_types, type_idx)?;
    Ok(code.ops[func.ops.range()].to_vec())
}

fn compile_func(
    wat: &str,
    sigs: &[Sig<'_>],
    func_types: &[u32],
    type_idx: u32,
) -> Result<(
    crate::module::op::CompiledFunc,
    crate::module::code::CodeArenas,
)> {
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
        num_imported_funcs: 0,
        tag_types: &[],
        max_expanded_locals: usize::MAX,
    };
    let mut validator = Validator::new_with_features(crate::module::enabled_features());
    for payload in Parser::new(0).parse_all(&bytes) {
        let payload = payload.map_err(wp_err)?;
        if let ValidPayload::Func(to_validate, body) =
            validator.payload(&payload).map_err(wp_err)?
        {
            let mut fv = to_validate.into_validator(FuncValidatorAllocations::default());
            let mut scratch = super::Scratch::default();
            let mut code = crate::module::code::CodeArenas::default();
            let func = translate_function(
                &ctx,
                &mut code,
                type_idx,
                &body,
                &mut fv,
                false,
                &mut scratch,
            )?;
            return Ok((func, code));
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
    // `local.get`s emit nothing: the add reads both locals and writes the result slot.
    assert!(matches!(ops.as_slice(), [Op::I32Add(2, 0, 1)]));
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
        ops.as_slice(),
        [
            Op::I32Const(1, 0),
            Op::I32Store(StoreArg {
                addr: 1,
                val: 0,
                offset: 0
            })
        ]
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
    assert!(matches!(ops[0], Op::I32Const(0, 1)));
    let Op::Br(BranchTarget { ip, keep, pop }, _) = ops[1] else {
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
    let Op::Br(BranchTarget { ip, keep, .. }, _) = ops[1] else {
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
    // BrIfNot(local 0)->else-start, I32Const(1), Br->end, I32Const(2)
    let Op::BrIfNot(else_t, 0) = ops[0] else {
        panic!("expected BrIfNot on local 0")
    };
    assert_eq!(else_t.ip, 3); // else body starts at index 3
    let Op::Br(end_t, _) = ops[2] else {
        panic!("expected Br")
    };
    assert_eq!(end_t.ip, 4); // end is past the last op
}

#[test]
fn br_table_uniform_keep() {
    let (sigs, ft) = one((&[ValType::I32], &[ValType::I32]));
    let (func, code) = compile_func(
        "(module (func (param i32) (result i32) \
            (block (result i32) i32.const 7 local.get 0 br_table 0 0)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    let ops = &code.ops[func.ops.range()];
    let Op::BrTable(range, _) = ops.last().unwrap() else {
        panic!("expected BrTable")
    };
    // Side-table layout: `len` case targets then the default. Every edge keeps the 1 block result.
    let pool = &code.br_tables[func.br_tables.range()];
    let end = (range.base + range.len) as usize;
    let default = &pool[end];
    let cases = &pool[range.base as usize..end];
    assert_eq!(default.keep, 1);
    assert!(cases.iter().all(|t| t.keep == 1));
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
        ops.as_slice(),
        [Op::Copy(3, 1), Op::Copy(2, 0), Op::CallInternal(1, 2)]
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
    let Op::Br(BranchTarget { ip, keep, .. }, _) = ops[1] else {
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
    assert!(matches!(ops[0], Op::I32Const(0, 1)));
    assert!(matches!(ops[1], Op::Br(..)));
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

#[test]
fn dropped_relop_does_not_fuse_into_br_if() {
    let (sigs, ft) = one((&[ValType::I32, ValType::I32, ValType::I32], &[]));
    let ops = compile_one(
        "(module (func (param i32 i32 i32)
            (block local.get 0 local.get 1 local.get 2 i32.lt_s drop br_if 0)))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // `br_if` tests local 0, not the dropped comparison.
    assert!(ops.iter().any(|op| matches!(op, Op::BrIf(..))));
    assert!(!ops.iter().any(|op| matches!(op, Op::BrIfCmp { .. })));
}

#[test]
fn constant_operands_fold_into_immediates() {
    let (sigs, ft) = one((&[ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32) (result i32) local.get 0 i32.const 1 i32.sub))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    assert!(matches!(ops.as_slice(), [Op::I32AddImm(1, 0, -1)]));
}

#[test]
fn compare_against_constant_fuses_into_one_branch() {
    let (sigs, ft) = one((&[ValType::I32], &[]));
    let ops = compile_one(
        "(module (func (param i32) (block (br_if 0 (i32.lt_s (local.get 0) (i32.const 2))))))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    assert!(matches!(
        ops.as_slice(),
        [Op::BrIfCmpImm {
            kind: CmpKind::LtS,
            negate: false,
            lhs: 0,
            imm: 2,
            ..
        }]
    ));
}

#[test]
fn loop_back_edge_runs_the_header_test_inverted() {
    let (sigs, ft) = one((&[ValType::I32], &[]));
    let ops = compile_one(
        "(module (func (param i32)
            (block (loop
                (br_if 1 (i32.eqz (local.get 0)))
                (local.set 0 (i32.sub (local.get 0) (i32.const 1)))
                (br 0)))))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // head: exit if zero; body; back-edge: stay (to the body) if nonzero, else fall to the head.
    assert!(matches!(
        ops.as_slice(),
        [
            Op::BrIfNot(BranchTarget { ip: 4, .. }, 0),
            Op::I32AddImm(0, 0, -1),
            Op::BrIf(BranchTarget { ip: 1, .. }, 0),
            Op::Br(BranchTarget { ip: 0, .. }, _),
        ]
    ));
}

#[test]
fn relop_fuses_into_if_past_the_block_entry_copies() {
    let (sigs, ft) = one((&[ValType::I32, ValType::I32], &[ValType::I32]));
    let ops = compile_one(
        "(module (func (param i32 i32) (result i32)
            local.get 1
            (if (result i32) (i32.eq (local.get 0) (local.get 1))
                (then (i32.const 7)) (else (i32.const 9)))
            i32.add))",
        &sigs,
        &ft,
        0,
    )
    .unwrap();
    // The pending `local.get 1` is copied to its own slot first; the compare still fuses.
    assert!(matches!(
        ops.as_slice(),
        [
            Op::Copy(2, 1),
            Op::BrIfCmp {
                kind: CmpKind::Eq,
                negate: true,
                lhs: 0,
                ..
            },
            ..
        ]
    ));
}
