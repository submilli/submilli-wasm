//! `wasmparser` type → module-IR conversions, shared by the decoder ([`super::super::parse`]),
//! the type-section reader, and the per-category compile submodules.

use crate::canon::{AggKind, IrGlobalType, IrHeap, IrRef, IrTableType, IrVal};
use crate::module::op::MemArg;
use crate::value::MemoryType;
use crate::{Error, Result};

/// Maps a `wasmparser` value type to the module IR. `kinds` is the per-type-index kind table,
/// used to tag concrete references with their hierarchy.
pub(crate) fn conv_valtype(kinds: &[AggKind], ty: wasmparser::ValType) -> Result<IrVal> {
    Ok(match ty {
        wasmparser::ValType::I32 => IrVal::I32,
        wasmparser::ValType::I64 => IrVal::I64,
        wasmparser::ValType::F32 => IrVal::F32,
        wasmparser::ValType::F64 => IrVal::F64,
        wasmparser::ValType::V128 => IrVal::V128,
        wasmparser::ValType::Ref(rt) => IrVal::Ref {
            nullable: rt.is_nullable(),
            heap: conv_heaptype(kinds, rt.heap_type())?,
        },
    })
}

/// A `br_on_cast` target reference type as `(heap, nullable)` for the IR.
pub(super) fn ref_target(kinds: &[AggKind], rt: wasmparser::RefType) -> Result<(IrHeap, bool)> {
    match conv_valtype(kinds, wasmparser::ValType::Ref(rt))? {
        IrVal::Ref { nullable, heap } => Ok((heap, nullable)),
        _ => unreachable!("ref type maps to a reference"),
    }
}

/// Converts a wasmparser heap type to the module IR: the abstract hierarchies (func/extern/any
/// and the bottoms, preserved distinctly for canonical identity) plus concrete (defined) types,
/// carrying a module-relative type index and kind (rewritten to a canonical id by `intern`).
pub(crate) fn conv_heaptype(kinds: &[AggKind], hty: wasmparser::HeapType) -> Result<IrHeap> {
    use wasmparser::{AbstractHeapType as A, HeapType as H};
    Ok(match hty {
        H::Abstract { shared: false, ty } => match ty {
            A::Func => IrHeap::Func,
            A::NoFunc => IrHeap::NoFunc,
            A::Extern => IrHeap::Extern,
            A::NoExtern => IrHeap::NoExtern,
            A::Any => IrHeap::Any,
            A::Eq => IrHeap::Eq,
            A::I31 => IrHeap::I31,
            A::Struct => IrHeap::Struct,
            A::Array => IrHeap::Array,
            A::None => IrHeap::None,
            A::Exn => IrHeap::Exn,
            A::NoExn => IrHeap::NoExn,
            A::Cont | A::NoCont => return Err(Error::msg("continuation heap types unsupported")),
        },
        H::Concrete(idx) | H::Exact(idx) => {
            let i = idx
                .as_module_index()
                .ok_or_else(|| Error::msg("non-module-relative type index"))?;
            let kind = *kinds
                .get(i as usize)
                .ok_or_else(|| Error::msg("type index out of range"))?;
            IrHeap::Concrete(i, kind)
        }
        H::Abstract { shared: true, .. } => {
            return Err(Error::msg("shared heap types unsupported"))
        }
    })
}

pub(super) fn memarg(m: wasmparser::MemArg) -> MemArg {
    MemArg {
        memory: m.memory,
        offset: m.offset as u32,
    }
}

pub(crate) fn conv_memtype(mt: wasmparser::MemoryType) -> MemoryType {
    if mt.memory64 {
        MemoryType::new64(mt.initial, mt.maximum)
    } else {
        MemoryType::new(mt.initial as u32, mt.maximum.map(|m| m as u32))
    }
}

pub(crate) fn conv_globaltype(
    kinds: &[AggKind],
    gt: wasmparser::GlobalType,
) -> Result<IrGlobalType> {
    Ok(IrGlobalType {
        content: conv_valtype(kinds, gt.content_type)?,
        mutable: gt.mutable,
    })
}

pub(crate) fn conv_tabletype(kinds: &[AggKind], tt: wasmparser::TableType) -> Result<IrTableType> {
    Ok(IrTableType {
        element: conv_reftype(kinds, tt.element_type)?,
        min: tt.initial as u32,
        max: tt.maximum.map(|m| m as u32),
    })
}

fn conv_reftype(kinds: &[AggKind], rt: wasmparser::RefType) -> Result<IrRef> {
    match conv_valtype(kinds, wasmparser::ValType::Ref(rt))? {
        IrVal::Ref { nullable, heap } => Ok(IrRef { nullable, heap }),
        _ => unreachable!("Ref maps to Ref"),
    }
}

pub(crate) fn conv_reftype_heap(kinds: &[AggKind], hty: wasmparser::HeapType) -> Result<IrHeap> {
    let rt = wasmparser::RefType::new(true, hty).ok_or_else(|| Error::msg("bad ref type"))?;
    Ok(conv_reftype(kinds, rt)?.heap)
}
