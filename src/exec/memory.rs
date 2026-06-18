//! Memory instructions: bounds-checked loads/stores plus size/grow/copy/fill.
//! `memory.init`/`data.drop` (passive data segments) arrive in Task #13.

// `&MemArg` arrives naturally from matching `&Op`; passing it by ref is fine.
#![allow(clippy::trivially_copy_pass_by_ref)]

use super::Execution;
use crate::instance::Instance;
use crate::module::op::{MemArg, Op};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Val;
use crate::{Error, Result};

fn oob() -> Error {
    Trap::MemoryOutOfBounds.into()
}

impl Execution {
    /// Executes a memory op (loads/stores/size/copy/fill/init/data.drop). `step` routes
    /// only these ops here.
    #[allow(clippy::too_many_lines)] // flat per-width load/store dispatch
    pub(super) fn exec_memory(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        let mem = inner.instance(instance).memories[0];
        match op {
            Op::I32Load(m) => {
                let b = self.load_n::<4>(inner, mem, m)?;
                self.push(Val::I32(i32::from_le_bytes(b)));
            }
            Op::I64Load(m) => {
                let b = self.load_n::<8>(inner, mem, m)?;
                self.push(Val::I64(i64::from_le_bytes(b)));
            }
            Op::F32Load(m) => {
                let b = self.load_n::<4>(inner, mem, m)?;
                self.push(Val::F32(u32::from_le_bytes(b)));
            }
            Op::F64Load(m) => {
                let b = self.load_n::<8>(inner, mem, m)?;
                self.push(Val::F64(u64::from_le_bytes(b)));
            }
            Op::I32Load8S(m) => {
                let b = self.load_n::<1>(inner, mem, m)?;
                self.push(Val::I32(i32::from(b[0] as i8)));
            }
            Op::I32Load8U(m) => {
                let b = self.load_n::<1>(inner, mem, m)?;
                self.push(Val::I32(i32::from(b[0])));
            }
            Op::I32Load16S(m) => {
                let b = self.load_n::<2>(inner, mem, m)?;
                self.push(Val::I32(i32::from(i16::from_le_bytes(b))));
            }
            Op::I32Load16U(m) => {
                let b = self.load_n::<2>(inner, mem, m)?;
                self.push(Val::I32(i32::from(u16::from_le_bytes(b))));
            }
            Op::I64Load8S(m) => {
                let b = self.load_n::<1>(inner, mem, m)?;
                self.push(Val::I64(i64::from(b[0] as i8)));
            }
            Op::I64Load8U(m) => {
                let b = self.load_n::<1>(inner, mem, m)?;
                self.push(Val::I64(i64::from(b[0])));
            }
            Op::I64Load16S(m) => {
                let b = self.load_n::<2>(inner, mem, m)?;
                self.push(Val::I64(i64::from(i16::from_le_bytes(b))));
            }
            Op::I64Load16U(m) => {
                let b = self.load_n::<2>(inner, mem, m)?;
                self.push(Val::I64(i64::from(u16::from_le_bytes(b))));
            }
            Op::I64Load32S(m) => {
                let b = self.load_n::<4>(inner, mem, m)?;
                self.push(Val::I64(i64::from(i32::from_le_bytes(b))));
            }
            Op::I64Load32U(m) => {
                let b = self.load_n::<4>(inner, mem, m)?;
                self.push(Val::I64(i64::from(u32::from_le_bytes(b))));
            }
            Op::I32Store(m) => {
                let v = self.pop().unwrap_i32();
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::I64Store(m) => {
                let v = self.pop().unwrap_i64();
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::F32Store(m) => {
                let v = self.pop().unwrap_f32().to_bits();
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::F64Store(m) => {
                let v = self.pop().unwrap_f64().to_bits();
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::I32Store8(m) => {
                let v = self.pop().unwrap_i32() as u8;
                self.store_n(inner, mem, m, [v])?;
            }
            Op::I32Store16(m) => {
                let v = self.pop().unwrap_i32() as u16;
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::I64Store8(m) => {
                let v = self.pop().unwrap_i64() as u8;
                self.store_n(inner, mem, m, [v])?;
            }
            Op::I64Store16(m) => {
                let v = self.pop().unwrap_i64() as u16;
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::I64Store32(m) => {
                let v = self.pop().unwrap_i64() as u32;
                self.store_n(inner, mem, m, v.to_le_bytes())?;
            }
            Op::MemorySize => {
                let pages = inner.memory(mem).size_pages();
                self.push(Val::I32(pages as i32));
            }
            Op::MemoryGrow => {
                let delta = u64::from(self.pop().unwrap_i32() as u32);
                let old = inner.memory_mut(mem).grow(delta);
                self.push(Val::I32(old.map_or(-1, |o| o as i32)));
            }
            Op::MemoryFill => {
                let len = self.pop().unwrap_i32() as u32 as usize;
                let val = self.pop().unwrap_i32() as u8;
                let dst = self.pop().unwrap_i32() as u32 as usize;
                let bytes = &mut inner.memory_mut(mem).bytes;
                let end = checked_range(dst, len, bytes.len())?;
                bytes[dst..end].fill(val);
            }
            Op::MemoryCopy => {
                let len = self.pop().unwrap_i32() as u32 as usize;
                let src = self.pop().unwrap_i32() as u32 as usize;
                let dst = self.pop().unwrap_i32() as u32 as usize;
                let bytes = &mut inner.memory_mut(mem).bytes;
                let src_end = checked_range(src, len, bytes.len())?;
                let dst_end = checked_range(dst, len, bytes.len())?;
                let _ = (src_end, dst_end);
                bytes.copy_within(src..src + len, dst);
            }
            Op::MemoryInit(seg) => {
                let len = self.pop().unwrap_i32() as u32 as usize;
                let src = self.pop().unwrap_i32() as u32 as usize;
                let dst = self.pop().unwrap_i32() as u32 as usize;
                let entity = inner.instance(instance);
                let module = entity.module.clone();
                let dropped = entity.dropped_data[*seg as usize];
                let data = &module.inner().datas[*seg as usize].bytes;
                let data_len = if dropped { 0 } else { data.len() };
                let src_end = checked_range(src, len, data_len)?;
                let mem_len = inner.memory(mem).bytes.len();
                checked_range(dst, len, mem_len)?;
                inner.memory_mut(mem).bytes[dst..dst + len].copy_from_slice(&data[src..src_end]);
            }
            Op::DataDrop(seg) => {
                inner.instance_mut(instance).dropped_data[*seg as usize] = true;
            }
            _ => return Err(Error::msg(format!("not a memory op: {op:?}"))),
        }
        Ok(())
    }

    /// Pops the address operand and bounds-checks `[ea, ea+N)` against `mem`.
    fn mem_ea<const N: usize>(
        &mut self,
        inner: &StoreInner,
        mem: crate::extern_::Memory,
        m: &MemArg,
    ) -> Result<usize> {
        let addr = u64::from(self.pop().unwrap_i32() as u32);
        let ea = addr.checked_add(u64::from(m.offset)).ok_or_else(oob)?;
        let end = ea.checked_add(N as u64).ok_or_else(oob)?;
        if end > inner.memory(mem).bytes.len() as u64 {
            return Err(oob());
        }
        Ok(ea as usize)
    }

    fn load_n<const N: usize>(
        &mut self,
        inner: &StoreInner,
        mem: crate::extern_::Memory,
        m: &MemArg,
    ) -> Result<[u8; N]> {
        let ea = self.mem_ea::<N>(inner, mem, m)?;
        let mut buf = [0u8; N];
        buf.copy_from_slice(&inner.memory(mem).bytes[ea..ea + N]);
        Ok(buf)
    }

    fn store_n<const N: usize>(
        &mut self,
        inner: &mut StoreInner,
        mem: crate::extern_::Memory,
        m: &MemArg,
        data: [u8; N],
    ) -> Result<()> {
        let ea = self.mem_ea::<N>(inner, mem, m)?;
        inner.memory_mut(mem).bytes[ea..ea + N].copy_from_slice(&data);
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
