//! Const-expression decoding: maps a wasm initializer expression (globals, element/data
//! offsets, table inits) to our owned [`ConstExpr`] of [`ConstOp`]s.

use wasmparser::Operator;

use crate::canon::AggKind;
use crate::module::compile::conv_reftype_heap;
use crate::module::inner::{ConstExpr, ConstOp};
use crate::{Error, Result};

use super::parse::wp_err;

pub(crate) fn parse_const_expr(
    kinds: &[AggKind],
    expr: &wasmparser::ConstExpr<'_>,
) -> Result<ConstExpr> {
    let mut reader = expr.get_operators_reader();
    let mut ops = Vec::new();
    while let Some(op) = conv_const_op(kinds, &reader.read().map_err(wp_err)?)? {
        ops.push(op);
    }
    Ok(ConstExpr(ops.into_boxed_slice()))
}

/// Maps one const-expression operator to a [`ConstOp`]; `None` marks the terminating `end`.
fn conv_const_op(kinds: &[AggKind], op: &Operator<'_>) -> Result<Option<ConstOp>> {
    Ok(Some(match *op {
        Operator::I32Const { value } => ConstOp::I32(value),
        Operator::I64Const { value } => ConstOp::I64(value),
        Operator::F32Const { value } => ConstOp::F32(value.bits()),
        Operator::F64Const { value } => ConstOp::F64(value.bits()),
        #[cfg(feature = "simd")]
        Operator::V128Const { value } => ConstOp::V128(u128::from_le_bytes(*value.bytes())),
        Operator::RefNull { hty } => ConstOp::RefNull(conv_reftype_heap(kinds, hty)?),
        Operator::RefFunc { function_index } => ConstOp::RefFunc(function_index),
        Operator::GlobalGet { global_index } => ConstOp::GlobalGet(global_index),
        Operator::I32Add => ConstOp::I32Add,
        Operator::I32Sub => ConstOp::I32Sub,
        Operator::I32Mul => ConstOp::I32Mul,
        Operator::I64Add => ConstOp::I64Add,
        Operator::I64Sub => ConstOp::I64Sub,
        Operator::I64Mul => ConstOp::I64Mul,
        Operator::End => return Ok(None),
        _ => return conv_const_gc_op(op),
    }))
}

/// GC-aggregate const operators (`struct.new*`/`array.new*`/`ref.i31`/`*.convert_*`).
fn conv_const_gc_op(op: &Operator<'_>) -> Result<Option<ConstOp>> {
    Ok(Some(match *op {
        Operator::RefI31 => ConstOp::RefI31,
        Operator::StructNew { struct_type_index } => ConstOp::StructNew(struct_type_index),
        Operator::StructNewDefault { struct_type_index } => {
            ConstOp::StructNewDefault(struct_type_index)
        }
        Operator::ArrayNew { array_type_index } => ConstOp::ArrayNew(array_type_index),
        Operator::ArrayNewDefault { array_type_index } => {
            ConstOp::ArrayNewDefault(array_type_index)
        }
        Operator::ArrayNewFixed {
            array_type_index,
            array_size,
        } => ConstOp::ArrayNewFixed {
            ty: array_type_index,
            n: array_size,
        },
        Operator::ArrayNewData {
            array_type_index,
            array_data_index,
        } => ConstOp::ArrayNewData {
            ty: array_type_index,
            data: array_data_index,
        },
        Operator::ArrayNewElem {
            array_type_index,
            array_elem_index,
        } => ConstOp::ArrayNewElem {
            ty: array_type_index,
            elem: array_elem_index,
        },
        Operator::AnyConvertExtern => ConstOp::AnyConvertExtern,
        Operator::ExternConvertAny => ConstOp::ExternConvertAny,
        ref other => {
            return Err(Error::msg(format!(
                "unsupported constant expression: {other:?}"
            )))
        }
    }))
}
