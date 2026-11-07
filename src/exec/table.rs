//! Table ops: `table.init/copy/get/set/size/fill` and `elem.drop`. Bulk-memory table
//! parts mirror the `memory.init`/`data.drop` handling in [`super::memory`]; the reference
//! *value* ops (`ref.null` etc.) live in [`super::ref_`].

// Indexing is into the wasmparser-validated table/elem index space or a just-checked range (#33
// carve-out): never unchecked guest input.
#![allow(clippy::indexing_slicing)]

use super::{cell, Execution};
use crate::instance::Instance;
use crate::module::op::{Op, Slot};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Val;
use crate::{Error, Result};

fn oob() -> Error {
    Trap::TableOutOfBounds.into()
}

impl Execution {
    pub(super) fn exec_table(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
        base: u32,
    ) -> Result<()> {
        let at = |s: Slot| super::step::slot(base, s);
        match op {
            Op::TableInit { elem, table, s } => {
                self.table_init(inner, instance, (*elem, *table), at(*s))
            }
            Op::TableCopy {
                dst_table,
                src_table,
                s,
            } => self.table_copy(inner, instance, (*dst_table, *src_table), at(*s)),
            Op::ElemDrop(elem) => {
                inner.instance_mut(instance).elems[*elem as usize] = Vec::new();
                Ok(())
            }
            Op::TableGet(t, s) => self.table_get(inner, instance, *t, at(*s)),
            Op::TableSet(t, s) => self.table_set(inner, instance, *t, at(*s)),
            Op::TableSize(t, s) => {
                let handle = inner.instance(instance).tables[*t as usize];
                let size = inner.table(handle).size();
                self.put_index(at(*s), inner.table(handle).ty.is_64(), size)
            }
            Op::TableFill(t, s) => self.table_fill(inner, instance, *t, at(*s)),
            _ => Err(Error::msg(format!("not a table op: {op:?}"))),
        }
    }

    /// `table.get`: the index at slot `at` is replaced by the element.
    fn table_get(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        table: u32,
        at: usize,
    ) -> Result<()> {
        let handle = inner.instance(instance).tables[table as usize];
        let idx = self.index_at(at, inner.table(handle).ty.is_64())?;
        let r = inner.table(handle).get(idx).ok_or_else(oob)?;
        self.set_val(at, Val::from_ref(r))
    }

    /// `table.set`: operands `index, value` from slot `at`.
    fn table_set(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        table: u32,
        at: usize,
    ) -> Result<()> {
        let handle = inner.instance(instance).tables[table as usize];
        let tt = &inner.table(handle).ty;
        let (is_64, kind) = (tt.is_64(), cell::refkind_of_heap(tt.element().heap_type()));
        let idx = self.index_at(at, is_64)?;
        let val = self.take_ref(at + 1, kind)?.to_ref();
        if inner.table_mut(handle).set(idx, val) {
            Ok(())
        } else {
            Err(oob())
        }
    }

    /// `table.fill`: operands `dst, value, len` from slot `at`.
    fn table_fill(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        table: u32,
        at: usize,
    ) -> Result<()> {
        let handle = inner.instance(instance).tables[table as usize];
        let tt = &inner.table(handle).ty;
        let (is_64, kind) = (tt.is_64(), cell::refkind_of_heap(tt.element().heap_type()));
        let dst = self.index_at(at, is_64)?;
        let val = self.take_ref(at + 1, kind)?.to_ref();
        let len = self.index_at(at + 2, is_64)?;
        if inner.table_mut(handle).fill(dst, val, len) {
            Ok(())
        } else {
            Err(oob())
        }
    }

    /// `table.init`: operands `dst, src, len` from slot `at`. The element segment is 32-bit
    /// (src/len are i32); only the table dst is index-typed (#42).
    fn table_init(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        (elem, table): (u32, u32),
        at: usize,
    ) -> Result<()> {
        let handle = inner.instance(instance).tables[table as usize];
        let dst = self.index_at(at, inner.table(handle).ty.is_64())?;
        let src = u64::from(self.scalar_at(at + 1)?.unwrap_i32() as u32);
        let len = u64::from(self.scalar_at(at + 2)?.unwrap_i32() as u32);

        let entity = inner.instance(instance);
        // The element instance was evaluated once at instantiation (`elem.drop` empties it).
        let refs = entity.elems[elem as usize].clone();

        let src_end = checked_range(src, len, refs.len() as u64)?;
        checked_range(dst, len, inner.table(handle).size())?;
        let (dst, src, src_end) = (dst as usize, src as usize, src_end as usize);
        for (i, r) in refs[src..src_end].iter().enumerate() {
            inner.table_mut(handle).set((dst + i) as u64, r.clone());
        }
        Ok(())
    }

    /// `table.copy`: operands `dst, src, len` from slot `at`.
    fn table_copy(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        (dst_table, src_table): (u32, u32),
        at: usize,
    ) -> Result<()> {
        let entity = inner.instance(instance);
        let dst_handle = entity.tables[dst_table as usize];
        let src_handle = entity.tables[src_table as usize];
        let (dst_64, src_64) = (
            inner.table(dst_handle).ty.is_64(),
            inner.table(src_handle).ty.is_64(),
        );
        let dst = self.index_at(at, dst_64)?;
        let src = self.index_at(at + 1, src_64)?;
        // The length is typed as the narrower of the two tables (#42).
        let len = self.index_at(at + 2, dst_64 && src_64)?;

        let src_end = checked_range(src, len, inner.table(src_handle).size())?;
        checked_range(dst, len, inner.table(dst_handle).size())?;
        let (dst, src, src_end) = (dst as usize, src as usize, src_end as usize);
        // Snapshot the source range so overlap (and two-table aliasing) is safe.
        let snapshot = inner.table(src_handle).elems[src..src_end].to_vec();
        for (i, r) in snapshot.into_iter().enumerate() {
            inner.table_mut(dst_handle).set((dst + i) as u64, r);
        }
        Ok(())
    }
}

/// Returns `start + len` if the whole range fits in `total`, else an OOB error. u64 so table64
/// (#42) indices can't truncate before the bounds check; callers cast to `usize` after.
fn checked_range(start: u64, len: u64, total: u64) -> Result<u64> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(end),
        _ => Err(oob()),
    }
}
