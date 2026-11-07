//! Constant-expression evaluation for instantiation: a small stack machine over [`ConstExpr`]
//! that yields the value of a global/table/segment initializer. The GC aggregate constructors
//! (`struct.new`/`array.new*`/`ref.i31`) allocate into the store's heap, so this takes `&mut`.

// Index/width juggling on validated inputs is intentional narrowing.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

// Validation guarantees a well-typed, balanced expression with in-range indices; a corrupt
// deserialized artifact doesn't, so every pop, index and field write below fails closed.

use crate::canon::{CanonicalTypeId, Slot};
use crate::error::{ir_get, InternalError};
use crate::extern_::Global;
use crate::func::Func;
use crate::module::inner::{ConstExpr, ConstOp, ElemItems};
use crate::module::Module;
use crate::store::{
    anyref_handle_i31, anyref_handle_slot, anyref_value, default_for_slot, slot_accepts,
    write_slot, GcObject, StoreInner,
};
use crate::trap::Trap;
use crate::value::{Ref, Val};
use crate::{Error, Result};

/// The instance context a constant expression resolves against: the defining module plus the
/// (partially built) function and global index spaces.
pub(crate) struct ConstCtx<'a> {
    pub module: &'a Module,
    pub funcs: &'a [Func],
    pub globals: &'a [Global],
}

/// Evaluates a constant expression with a small stack machine. The GC aggregate constructors
/// allocate into the store's heap (hence `&mut`), so the result of, e.g., a `(global (ref $s)
/// (struct.new ...))` initializer is a real reference.
pub(crate) fn eval_const(inner: &mut StoreInner, ctx: &ConstCtx<'_>, e: &ConstExpr) -> Result<Val> {
    let mut stack: Vec<Val> = Vec::new();
    for op in &e.0 {
        let v = match op {
            ConstOp::I32(v) => Val::I32(*v),
            ConstOp::I64(v) => Val::I64(*v),
            ConstOp::F32(v) => Val::F32(*v),
            ConstOp::F64(v) => Val::F64(*v),
            #[cfg(feature = "simd")]
            ConstOp::V128(v) => Val::V128(crate::value::V128::from(*v)),
            ConstOp::RefNull(heap) => Val::null_for_heap(heap),
            ConstOp::RefFunc(i) => Val::FuncRef(Some(*ir_get(ctx.funcs, *i)?)),
            ConstOp::GlobalGet(g) => inner.global(*ir_get(ctx.globals, *g)?).value,
            ConstOp::I32Add => i32_bin(&mut stack, i32::wrapping_add)?,
            ConstOp::I32Sub => i32_bin(&mut stack, i32::wrapping_sub)?,
            ConstOp::I32Mul => i32_bin(&mut stack, i32::wrapping_mul)?,
            ConstOp::I64Add => i64_bin(&mut stack, i64::wrapping_add)?,
            ConstOp::I64Sub => i64_bin(&mut stack, i64::wrapping_sub)?,
            ConstOp::I64Mul => i64_bin(&mut stack, i64::wrapping_mul)?,
            ConstOp::RefI31 => anyref_value(anyref_handle_i31(pop_i32(&mut stack)?)),
            ConstOp::StructNew(ty) => const_struct(inner, ctx, &mut stack, *ty, false)?,
            ConstOp::StructNewDefault(ty) => const_struct(inner, ctx, &mut stack, *ty, true)?,
            ConstOp::ArrayNew(ty) => const_array(inner, ctx, &mut stack, *ty, false)?,
            ConstOp::ArrayNewDefault(ty) => const_array(inner, ctx, &mut stack, *ty, true)?,
            ConstOp::ArrayNewFixed { ty, n } => const_array_fixed(inner, ctx, &mut stack, *ty, *n)?,
            ConstOp::ArrayNewData { ty, data } => {
                const_array_data(inner, ctx, &mut stack, *ty, *data)?
            }
            ConstOp::ArrayNewElem { ty, elem } => {
                const_array_elem(inner, ctx, &mut stack, *ty, *elem)?
            }
            ConstOp::AnyConvertExtern => inner.any_convert_extern(pop(&mut stack)?)?,
            ConstOp::ExternConvertAny => inner.extern_convert_any(pop(&mut stack)?)?,
        };
        stack.push(v);
    }
    match (stack.pop(), stack.is_empty()) {
        (Some(v), true) => Ok(v),
        _ => Err(malformed()),
    }
}

pub(crate) fn eval_const_ref(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    e: &ConstExpr,
) -> Result<Ref> {
    val_to_ref(eval_const(inner, ctx, e)?)
}

/// Resolves an element segment to its references (function indices or initializer expressions).
pub(crate) fn elem_refs(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    items: &ElemItems,
) -> Result<Vec<Ref>> {
    match items {
        ElemItems::Funcs(idxs) => Ok(idxs
            .iter()
            .map(|&i| Ok(Ref::Func(Some(*ir_get(ctx.funcs, i)?))))
            .collect::<Result<_>>()?),
        ElemItems::Exprs(exprs) => {
            let mut refs = Vec::with_capacity(exprs.len());
            for e in exprs {
                refs.push(eval_const_ref(inner, ctx, e)?);
            }
            Ok(refs)
        }
    }
}

#[cold]
fn malformed() -> Error {
    InternalError::Compiler("malformed constant expression").into()
}

fn pop(stack: &mut Vec<Val>) -> Result<Val> {
    stack.pop().ok_or_else(malformed)
}

fn pop_i32(stack: &mut Vec<Val>) -> Result<i32> {
    pop(stack)?.i32().ok_or_else(malformed)
}

fn pop_i64(stack: &mut Vec<Val>) -> Result<i64> {
    pop(stack)?.i64().ok_or_else(malformed)
}

/// Pops a value for GC field `slot`, checking its kind fits (as validation would have).
fn pop_for(stack: &mut Vec<Val>, slot: Slot) -> Result<Val> {
    let v = pop(stack)?;
    if slot_accepts(slot, &v) {
        Ok(v)
    } else {
        Err(malformed())
    }
}

/// Extended-const binop: `(op a b)` pushed `a` then `b`, so pop yields `b` then `a` (#40).
fn i32_bin(stack: &mut Vec<Val>, f: fn(i32, i32) -> i32) -> Result<Val> {
    let b = pop_i32(stack)?;
    let a = pop_i32(stack)?;
    Ok(Val::I32(f(a, b)))
}

fn i64_bin(stack: &mut Vec<Val>, f: fn(i64, i64) -> i64) -> Result<Val> {
    let b = pop_i64(stack)?;
    let a = pop_i64(stack)?;
    Ok(Val::I64(f(a, b)))
}

fn const_struct(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    default: bool,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty)?;
    let layout = ctx.module.inner().struct_layout(ty)?;
    let fields = layout.fields();
    let mut data = vec![0u8; layout.size()];
    if default {
        for &slot in fields {
            write_slot(slot, &mut data, default_for_slot(slot));
        }
    } else {
        for &slot in fields.iter().rev() {
            write_slot(slot, &mut data, pop_for(stack, slot)?);
        }
    }
    alloc(
        inner,
        GcObject::new_struct(type_id, data.into_boxed_slice()),
    )
}

fn const_array(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    default: bool,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty)?;
    let layout = ctx.module.inner().array_layout(ty)?;
    let count = pop_i32(stack)? as u32 as usize;
    let byte_len = elem_bytes(count, layout.stride())?;
    inner.gc_check_capacity(byte_len)?;
    let mut data = vec![0u8; byte_len];
    let fill = if default {
        default_for_slot(layout.elem_at(0))
    } else {
        pop_for(stack, layout.elem_at(0))?
    };
    for i in 0..count {
        write_slot(layout.elem_at(i), &mut data, fill);
    }
    alloc_array(inner, type_id, data)
}

fn const_array_fixed(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    n: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty)?;
    let layout = ctx.module.inner().array_layout(ty)?;
    let count = n as usize;
    if count > stack.len() {
        return Err(malformed());
    }
    let mut data = vec![0u8; layout.body_size(count)];
    for i in (0..count).rev() {
        write_slot(
            layout.elem_at(i),
            &mut data,
            pop_for(stack, layout.elem_at(i))?,
        );
    }
    alloc_array(inner, type_id, data)
}

fn const_array_data(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    data: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty)?;
    let stride = ctx.module.inner().array_layout(ty)?.stride();
    let count = pop_i32(stack)? as u32 as usize;
    let offset = pop_i32(stack)? as u32 as usize;
    let byte_len = elem_bytes(count, stride)?;
    let seg = &ir_get(&ctx.module.inner().datas, data)?.bytes;
    range(offset, byte_len, seg.len(), Trap::MemoryOutOfBounds)?;
    inner.gc_check_capacity(byte_len)?;
    let body = seg[offset..offset + byte_len].to_vec();
    alloc_array(inner, type_id, body)
}

fn const_array_elem(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    elem: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty)?;
    let layout = ctx.module.inner().array_layout(ty)?;
    let count = pop_i32(stack)? as u32 as usize;
    let offset = pop_i32(stack)? as u32 as usize;
    let refs = elem_refs(inner, ctx, &ir_get(&ctx.module.inner().elems, elem)?.items)?;
    range(offset, count, refs.len(), Trap::TableOutOfBounds)?;
    inner.gc_check_capacity(elem_bytes(count, layout.stride())?)?;
    let mut data = vec![0u8; layout.body_size(count)];
    for (i, r) in refs[offset..offset + count].iter().enumerate() {
        let (slot, v) = (layout.elem_at(i), Val::from_ref(r.clone()));
        if !slot_accepts(slot, &v) {
            return Err(malformed());
        }
        write_slot(slot, &mut data, v);
    }
    alloc_array(inner, type_id, data)
}

fn alloc(inner: &mut StoreInner, object: GcObject) -> Result<Val> {
    // Const-eval (global/element initializers) runs at instantiation, outside the run loop's
    // reservation flow, so it is bounded by the hard ceiling.
    let slot = inner.alloc_gc_unreserved(object)?;
    Ok(anyref_value(anyref_handle_slot(slot)))
}

fn alloc_array(inner: &mut StoreInner, type_id: CanonicalTypeId, data: Vec<u8>) -> Result<Val> {
    alloc(inner, GcObject::new_array(type_id, data.into_boxed_slice()))
}

fn elem_bytes(count: usize, width: usize) -> Result<usize> {
    count
        .checked_mul(width)
        .ok_or_else(|| Trap::AllocationTooLarge.into())
}

fn range(start: usize, len: usize, total: usize, trap: Trap) -> Result<()> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(()),
        _ => Err(trap.into()),
    }
}

fn val_to_ref(v: Val) -> Result<Ref> {
    match v {
        Val::FuncRef(f) => Ok(Ref::Func(f)),
        Val::ExternRef(e) => Ok(Ref::Extern(e)),
        Val::AnyRef(a) => Ok(Ref::Any(a)),
        Val::ExnRef(x) => Ok(Ref::Exn(x)),
        _ => Err(Error::msg("global is not a reference")),
    }
}
