//! Table ops: `table.init/copy/get/set/size/fill` and `elem.drop`. Bulk-memory table
//! parts mirror the `memory.init`/`data.drop` handling in [`super::memory`]; the reference
//! *value* ops (`ref.null` etc.) live in [`super::ref_`].

use super::Execution;
use crate::instance::Instance;
use crate::module::op::Op;
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
    ) -> Result<()> {
        match op {
            Op::TableInit { elem, table } => self.table_init(inner, instance, *elem, *table),
            Op::TableCopy {
                dst_table,
                src_table,
            } => self.table_copy(inner, instance, *dst_table, *src_table),
            Op::ElemDrop(elem) => {
                inner.instance_mut(instance).elems[*elem as usize] = Vec::new();
                Ok(())
            }
            Op::TableGet(t) => self.table_get(inner, instance, *t),
            Op::TableSet(t) => self.table_set(inner, instance, *t),
            Op::TableSize(t) => {
                let handle = inner.instance(instance).tables[*t as usize];
                let size = inner.table(handle).size();
                self.push(Val::I32(size as i32));
                Ok(())
            }
            Op::TableFill(t) => self.table_fill(inner, instance, *t),
            _ => Err(Error::msg(format!("not a table op: {op:?}"))),
        }
    }

    fn table_get(&mut self, inner: &StoreInner, instance: Instance, table: u32) -> Result<()> {
        let idx = u64::from(self.pop_i32() as u32);
        let handle = inner.instance(instance).tables[table as usize];
        let r = inner.table(handle).get(idx).ok_or_else(oob)?;
        self.push(Val::from_ref(r));
        Ok(())
    }

    fn table_set(&mut self, inner: &mut StoreInner, instance: Instance, table: u32) -> Result<()> {
        let val = self.pop().to_ref();
        let idx = u64::from(self.pop_i32() as u32);
        let handle = inner.instance(instance).tables[table as usize];
        if inner.table_mut(handle).set(idx, val) {
            Ok(())
        } else {
            Err(oob())
        }
    }

    fn table_fill(&mut self, inner: &mut StoreInner, instance: Instance, table: u32) -> Result<()> {
        let len = u64::from(self.pop_i32() as u32);
        let val = self.pop().to_ref();
        let dst = u64::from(self.pop_i32() as u32);
        let handle = inner.instance(instance).tables[table as usize];
        if inner.table_mut(handle).fill(dst, val, len) {
            Ok(())
        } else {
            Err(oob())
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
        let handle = entity.tables[table as usize];
        // The element instance was evaluated once at instantiation (`elem.drop` empties it).
        let refs = entity.elems[elem as usize].clone();

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

/// Returns `start + len` if the whole range fits in `total`, else an OOB error.
fn checked_range(start: usize, len: usize, total: usize) -> Result<usize> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(end),
        _ => Err(oob()),
    }
}
