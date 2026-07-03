//! Memory instructions: bounds-checked loads/stores plus size/grow/copy/fill, each on an explicit
//! memory index (#41). `memory.init`/`data.drop` operate on passive data segments.

// `&MemArg` arrives naturally from matching `&Op`; passing it by ref is fine. Indexing is into the
// wasmparser-validated memory/data index space or guarded by a just-checked `checked_range` bound —
// never unchecked guest input (#33 carve-out).
#![allow(clippy::trivially_copy_pass_by_ref, clippy::indexing_slicing)]

use super::Execution;
use crate::extern_::Memory;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::module::op::{MemArg, Op, BIG_MEMARG};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::{Error, Result};

fn oob() -> Error {
    Trap::MemoryOutOfBounds.into()
}

/// Resolves a compact memarg to `(memory index, byte offset)` — a demoted wide immediate (the
/// [`BIG_MEMARG`] sentinel; memory64 offsets past `u32`, or the literal `u32::MAX`) reads the
/// function's out-of-line pool instead.
#[inline(always)]
#[allow(clippy::inline_always)] // one hot caller per load/store inside the fused dispatch
fn resolve(code: &Code, m: &MemArg) -> (u32, u64) {
    if m.offset == BIG_MEMARG {
        let big = &code.big_memargs()[m.memory as usize];
        (big.memory, big.offset)
    } else {
        (m.memory, u64::from(m.offset))
    }
}

/// The instance's memory at index `idx` (imported memories first, then defined â the wasm index
/// space). `memory.grow` is serviced separately in the run loop (for the limiter).
fn mem(inner: &StoreInner, instance: Instance, idx: u32) -> Memory {
    inner.instance(instance).memories[idx as usize]
}

impl Execution {
    /// Executes a memory op (loads/stores/size/copy/fill/init/data.drop). `step` routes
    /// only these ops here (`memory.grow` excepted â see the run loop).
    // Single call site inside the fused dispatch: inlining threads this secondary match into
    // the primary one (measured +19% CoreMark with numeric+memory inlined).
    #[allow(clippy::too_many_lines, clippy::inline_always)] // flat per-width load/store dispatch
    #[inline(always)]
    pub(super) fn exec_memory(
        &mut self,
        inner: &mut StoreInner,
        code: &Code,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            // `data.drop` needs no memory (a module may carry passive data segments without one â
            // common in GC modules using `array.new_data`).
            Op::DataDrop(seg) => inner.instance_mut(instance).dropped_data[*seg as usize] = true,
            Op::I32Load(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i32(i32::from_le_bytes(b));
            }
            Op::I64Load(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_i64(i64::from_le_bytes(b));
            }
            Op::F32Load(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_f32_bits(u32::from_le_bytes(b));
            }
            Op::F64Load(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_f64_bits(u64::from_le_bytes(b));
            }
            Op::I32Load8S(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i32(i32::from(b[0] as i8));
            }
            Op::I32Load8U(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i32(i32::from(b[0]));
            }
            Op::I32Load16S(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i32(i32::from(i16::from_le_bytes(b)));
            }
            Op::I32Load16U(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i32(i32::from(u16::from_le_bytes(b)));
            }
            Op::I64Load8S(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i64(i64::from(b[0] as i8));
            }
            Op::I64Load8U(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i64(i64::from(b[0]));
            }
            Op::I64Load16S(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i64(i64::from(i16::from_le_bytes(b)));
            }
            Op::I64Load16U(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i64(i64::from(u16::from_le_bytes(b)));
            }
            Op::I64Load32S(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i64(i64::from(i32::from_le_bytes(b)));
            }
            Op::I64Load32U(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i64(i64::from(u32::from_le_bytes(b)));
            }
            Op::I32Store(m) => {
                let v = self.pop().unwrap_i32();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store(m) => {
                let v = self.pop().unwrap_i64();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::F32Store(m) => {
                let v = self.pop().unwrap_f32().to_bits();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::F64Store(m) => {
                let v = self.pop().unwrap_f64().to_bits();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I32Store8(m) => {
                let v = self.pop().unwrap_i32() as u8;
                self.store_n(inner, code, instance, m, [v])?;
            }
            Op::I32Store16(m) => {
                let v = self.pop().unwrap_i32() as u16;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store8(m) => {
                let v = self.pop().unwrap_i64() as u8;
                self.store_n(inner, code, instance, m, [v])?;
            }
            Op::I64Store16(m) => {
                let v = self.pop().unwrap_i64() as u16;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store32(m) => {
                let v = self.pop().unwrap_i64() as u32;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::MemorySize(i) => {
                let memory = mem(inner, instance, *i);
                let pages = inner.memory(memory).size_pages();
                self.push_index(inner.memory(memory).ty.is_64(), pages);
            }
            Op::MemoryFill(i) => {
                let is_64 = inner.memory(mem(inner, instance, *i)).ty.is_64();
                let len = self.pop_index(is_64);
                let val = self.pop().unwrap_i32() as u8;
                let dst = self.pop_index(is_64);
                let bytes = &mut inner.memory_mut(mem(inner, instance, *i)).bytes;
                let end = checked_range(dst, len, bytes.len() as u64)?;
                bytes[dst as usize..end as usize].fill(val);
            }
            Op::MemoryCopy(dst_i, src_i) => self.memory_copy(inner, instance, *dst_i, *src_i)?,
            Op::MemoryInit(seg, i) => self.memory_init(inner, instance, *seg, *i)?,
            _ => return Err(Error::msg(format!("not a memory op: {op:?}"))),
        }
        Ok(())
    }

    /// `memory.copy dst_mem src_mem`: the two memories may differ. Same index uses overlap-safe
    /// `copy_within`; different indices copy via a temporary (no aliasing concern).
    fn memory_copy(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        dst_i: u32,
        src_i: u32,
    ) -> Result<()> {
        let (dst_mem, src_mem) = (mem(inner, instance, dst_i), mem(inner, instance, src_i));
        let (dst_64, src_64) = (
            inner.memory(dst_mem).ty.is_64(),
            inner.memory(src_mem).ty.is_64(),
        );
        // The length is typed as the narrower of the two memories (#42).
        let len = self.pop_index(dst_64 && src_64);
        let src = self.pop_index(src_64);
        let dst = self.pop_index(dst_64);
        checked_range(src, len, inner.memory(src_mem).bytes.len() as u64)?;
        checked_range(dst, len, inner.memory(dst_mem).bytes.len() as u64)?;
        let (src, dst, len) = (src as usize, dst as usize, len as usize);
        if dst_i == src_i {
            inner
                .memory_mut(dst_mem)
                .bytes
                .copy_within(src..src + len, dst);
        } else {
            let chunk = inner.memory(src_mem).bytes[src..src + len].to_vec();
            inner.memory_mut(dst_mem).bytes[dst..dst + len].copy_from_slice(&chunk);
        }
        Ok(())
    }

    /// `memory.init seg mem`: copy from passive data segment `seg` into memory `mem_i`.
    fn memory_init(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        seg: u32,
        mem_i: u32,
    ) -> Result<()> {
        let is_64 = inner.memory(mem(inner, instance, mem_i)).ty.is_64();
        let len = u64::from(self.pop_i32() as u32);
        let src = u64::from(self.pop_i32() as u32);
        let dst = self.pop_index(is_64);
        let entity = inner.instance(instance);
        let module = entity.module.clone();
        let dropped = entity.dropped_data[seg as usize];
        let data = &module.inner().datas[seg as usize].bytes;
        let data_len = if dropped { 0 } else { data.len() };
        let src_end = checked_range(src, len, data_len as u64)?;
        let mem = mem(inner, instance, mem_i);
        checked_range(dst, len, inner.memory(mem).bytes.len() as u64)?;
        let (dst, src, src_end, len) = (dst as usize, src as usize, src_end as usize, len as usize);
        inner.memory_mut(mem).bytes[dst..dst + len].copy_from_slice(&data[src..src_end]);
        Ok(())
    }

    /// Pops the address operand, resolves `m`'s memory, and bounds-checks `[ea, ea+N)`.
    /// Pops the address operand and bounds-checks the `N`-byte access on an already-resolved
    /// memory (one handle lookup per load/store, not one per field touched).
    fn mem_ea<const N: usize>(
        &mut self,
        entity: &crate::store::MemoryEntity,
        offset: u64,
    ) -> Result<usize> {
        let addr = self.pop_index(entity.ty.is_64());
        let ea = addr.checked_add(offset).ok_or_else(oob)?;
        let end = ea.checked_add(N as u64).ok_or_else(oob)?;
        if end > entity.bytes.len() as u64 {
            return Err(oob());
        }
        Ok(ea as usize)
    }

    pub(super) fn load_n<const N: usize>(
        &mut self,
        inner: &StoreInner,
        code: &Code,
        instance: Instance,
        m: &MemArg,
    ) -> Result<[u8; N]> {
        let (memory, offset) = resolve(code, m);
        let entity = inner.memory(mem(inner, instance, memory));
        let ea = self.mem_ea::<N>(entity, offset)?;
        let mut buf = [0u8; N];
        buf.copy_from_slice(&entity.bytes[ea..ea + N]);
        Ok(buf)
    }

    pub(super) fn store_n<const N: usize>(
        &mut self,
        inner: &mut StoreInner,
        code: &Code,
        instance: Instance,
        m: &MemArg,
        data: [u8; N],
    ) -> Result<()> {
        let (memory, offset) = resolve(code, m);
        let memory = mem(inner, instance, memory);
        let entity = inner.memory_mut(memory);
        let ea = self.mem_ea::<N>(entity, offset)?;
        entity.bytes[ea..ea + N].copy_from_slice(&data);
        Ok(())
    }
}

/// Returns `start + len` if the whole range fits in `total`, else an OOB error. Operates in u64 so
/// memory64 (#42) addresses can't truncate before the bounds check; callers cast to `usize` after.
fn checked_range(start: u64, len: u64, total: u64) -> Result<u64> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(end),
        _ => Err(oob()),
    }
}
