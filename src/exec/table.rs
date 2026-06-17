//! Bulk-memory table ops: `table.init`, `table.copy`, `elem.drop`. Mirrors the
//! `memory.init`/`data.drop` handling in [`super::memory`].

use super::Execution;
use crate::instance::Instance;
use crate::module::inner::ElemItems;
use crate::module::op::Op;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Ref;
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
    ) -> Result<()> {
        match op {
            Op::TableInit { elem, table } => self.table_init(inner, instance, *elem, *table),
            Op::TableCopy {
                dst_table,
                src_table,
            } => self.table_copy(inner, instance, *dst_table, *src_table),
            Op::ElemDrop(elem) => {
                inner.instance_mut(instance).dropped_elems[*elem as usize] = true;
                Ok(())
            }
            _ => Err(Error::msg(format!("not a table op: {op:?}"))),
        }
    }

    fn table_init(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        elem: u32,
        table: u32,
    ) -> Result<()> {
        let len = self.pop().unwrap_i32() as u32 as usize;
        let src = self.pop().unwrap_i32() as u32 as usize;
        let dst = self.pop().unwrap_i32() as u32 as usize;

        let entity = inner.instance(instance);
        let module = entity.module.clone();
        let dropped = entity.dropped_elems[elem as usize];
        let handle = entity.tables[table as usize];
        let refs = if dropped {
            Vec::new()
        } else {
            elem_refs(&entity.funcs, &module.inner().elems[elem as usize].items)?
        };

        let src_end = checked_range(src, len, refs.len())?;
        let table_len = inner.table(handle).size() as usize;
        checked_range(dst, len, table_len)?;
        for (i, r) in refs[src..src_end].iter().enumerate() {
            inner.table_mut(handle).set((dst + i) as u64, r.clone());
        }
        Ok(())
    }

    fn table_copy(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        dst_table: u32,
        src_table: u32,
    ) -> Result<()> {
        let len = self.pop().unwrap_i32() as u32 as usize;
        let src = self.pop().unwrap_i32() as u32 as usize;
        let dst = self.pop().unwrap_i32() as u32 as usize;

        let entity = inner.instance(instance);
        let dst_handle = entity.tables[dst_table as usize];
        let src_handle = entity.tables[src_table as usize];

        let src_end = checked_range(src, len, inner.table(src_handle).size() as usize)?;
        checked_range(dst, len, inner.table(dst_handle).size() as usize)?;
        // Snapshot the source range so overlap (and two-table aliasing) is safe.
        let snapshot = inner.table(src_handle).elems[src..src_end].to_vec();
        for (i, r) in snapshot.into_iter().enumerate() {
            inner.table_mut(dst_handle).set((dst + i) as u64, r);
        }
        Ok(())
    }
}

/// Builds the reference list of a (live) element segment for `table.init`.
fn elem_refs(funcs: &[crate::func::Func], items: &ElemItems) -> Result<Vec<Ref>> {
    match items {
        ElemItems::Funcs(idxs) => Ok(idxs
            .iter()
            .map(|&i| Ref::Func(Some(funcs[i as usize])))
            .collect()),
        ElemItems::Exprs(_) => Err(Error::msg(
            "element expressions require reference-types (Phase 4)",
        )),
    }
}

/// Returns `start + len` if the whole range fits in `total`, else an OOB error.
fn checked_range(start: usize, len: usize, total: usize) -> Result<usize> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(end),
        _ => Err(oob()),
    }
}
