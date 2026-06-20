//! Constant-expression evaluation for instantiation: a small stack machine over [`ConstExpr`]
//! that yields the value of a global/table/segment initializer. The GC aggregate constructors
//! (`struct.new`/`array.new*`/`ref.i31`) allocate into the store's heap, so this takes `&mut`.

// Index/width juggling on validated inputs is intentional narrowing.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use crate::canon::{CanonicalTypeId, Layout};
use crate::extern_::Global;
use crate::func::Func;
use crate::module::inner::{ConstExpr, ConstOp, ElemItems};
use crate::module::Module;
use crate::store::{
    anyref_handle_i31, anyref_handle_slot, anyref_value, default_for_slot, write_slot, GcObject,
    StoreInner,
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
            ConstOp::RefNull(heap) => Val::null_for_heap(heap),
            ConstOp::RefFunc(i) => Val::FuncRef(Some(ctx.funcs[*i as usize])),
            ConstOp::GlobalGet(g) => inner.global(ctx.globals[*g as usize]).value,
            ConstOp::RefI31 => anyref_value(anyref_handle_i31(pop(&mut stack).unwrap_i32())),
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
            ConstOp::AnyConvertExtern => inner.any_convert_extern(pop(&mut stack))?,
            ConstOp::ExternConvertAny => inner.extern_convert_any(pop(&mut stack)),
        };
        stack.push(v);
    }
    Ok(stack.pop().expect("const expr yields one value"))
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
            .map(|&i| Ref::Func(Some(ctx.funcs[i as usize])))
            .collect()),
        ElemItems::Exprs(exprs) => {
            let mut refs = Vec::with_capacity(exprs.len());
            for e in exprs {
                refs.push(eval_const_ref(inner, ctx, e)?);
            }
            Ok(refs)
        }
    }
}

fn pop(stack: &mut Vec<Val>) -> Val {
    stack.pop().expect("const-expr operand stack underflow")
}

fn const_struct(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    default: bool,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty);
    let layout = ctx.module.inner().layout(ty);
    let Layout::Struct { fields, size } = layout else {
        unreachable!("struct.new on non-struct type");
    };
    let mut data = vec![0u8; *size];
    if default {
        for &slot in fields.as_ref() {
            write_slot(slot, &mut data, default_for_slot(slot));
        }
    } else {
        for &slot in fields.iter().rev() {
            write_slot(slot, &mut data, pop(stack));
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
    let type_id = ctx.module.inner().canonical_type_id(ty);
    let layout = ctx.module.inner().layout(ty);
    let count = pop(stack).unwrap_i32() as u32 as usize;
    let byte_len = elem_bytes(count, layout.stride())?;
    inner.gc_check_capacity(byte_len)?;
    let mut data = vec![0u8; byte_len];
    let fill = if default {
        default_for_slot(layout.elem_at(0))
    } else {
        pop(stack)
    };
    for i in 0..count {
        write_slot(layout.elem_at(i), &mut data, fill);
    }
    alloc_array(inner, type_id, count, data)
}

fn const_array_fixed(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    n: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty);
    let layout = ctx.module.inner().layout(ty);
    let count = n as usize;
    let mut data = vec![0u8; layout.body_size(count)];
    for i in (0..count).rev() {
        write_slot(layout.elem_at(i), &mut data, pop(stack));
    }
    alloc_array(inner, type_id, count, data)
}

fn const_array_data(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    data: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty);
    let stride = ctx.module.inner().layout(ty).stride();
    let count = pop(stack).unwrap_i32() as u32 as usize;
    let offset = pop(stack).unwrap_i32() as u32 as usize;
    let byte_len = elem_bytes(count, stride)?;
    let seg = &ctx.module.inner().datas[data as usize].bytes;
    range(offset, byte_len, seg.len(), Trap::MemoryOutOfBounds)?;
    inner.gc_check_capacity(byte_len)?;
    let body = seg[offset..offset + byte_len].to_vec();
    alloc_array(inner, type_id, count, body)
}

fn const_array_elem(
    inner: &mut StoreInner,
    ctx: &ConstCtx<'_>,
    stack: &mut Vec<Val>,
    ty: u32,
    elem: u32,
) -> Result<Val> {
    let type_id = ctx.module.inner().canonical_type_id(ty);
    let layout = ctx.module.inner().layout(ty);
    let count = pop(stack).unwrap_i32() as u32 as usize;
    let offset = pop(stack).unwrap_i32() as u32 as usize;
    let refs = elem_refs(inner, ctx, &ctx.module.inner().elems[elem as usize].items)?;
    range(offset, count, refs.len(), Trap::TableOutOfBounds)?;
    inner.gc_check_capacity(elem_bytes(count, layout.stride())?)?;
    let mut data = vec![0u8; layout.body_size(count)];
    for (i, r) in refs[offset..offset + count].iter().enumerate() {
        write_slot(layout.elem_at(i), &mut data, Val::from_ref(r.clone()));
    }
    alloc_array(inner, type_id, count, data)
}

fn alloc(inner: &mut StoreInner, object: GcObject) -> Result<Val> {
    let slot = inner.alloc_gc(object)?;
    Ok(anyref_value(anyref_handle_slot(slot)))
}

fn alloc_array(
    inner: &mut StoreInner,
    type_id: CanonicalTypeId,
    count: usize,
    data: Vec<u8>,
) -> Result<Val> {
    alloc(
        inner,
        GcObject::new_array(type_id, count as u32, data.into_boxed_slice()),
    )
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
