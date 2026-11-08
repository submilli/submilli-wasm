//! Memory instructions: bounds-checked loads/stores plus size/copy/fill, each on an explicit memory
//! index (#41) and slot-addressed operands. `memory.init`/`data.drop` operate on passive data
//! segments.

// `&LoadArg`/`&StoreArg` arrive naturally from matching `&Op`; passing them by ref is fine. Indexing is into the
// wasmparser-validated memory/data index space or guarded by a just-checked `checked_range` bound —
// never unchecked guest input (#33 carve-out).
#![allow(clippy::trivially_copy_pass_by_ref, clippy::indexing_slicing)]

use super::Execution;
use crate::error::{ir_get, ir_get_mut};
use crate::extern_::Memory;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::module::op::{LoadArg, Op, Slot, StoreArg, BIG_MEMARG};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::{Error, Result};

fn oob() -> Error {
    Trap::MemoryOutOfBounds.into()
}

/// A load's address slot and `(memory index, byte offset)`: memory 0 with the inline offset, or
/// — the [`BIG_MEMARG`] sentinel — the pooled memarg, with the address in place at `dst`.
#[inline(always)]
#[allow(clippy::inline_always)] // one hot caller per load inside the fused dispatch
pub(super) fn load_operands(code: &Code, m: &LoadArg) -> Result<(Slot, (u32, u64))> {
    if m.offset == BIG_MEMARG {
        Ok((m.dst, pooled(code, m.addr)?))
    } else {
        Ok((m.addr, (0, u64::from(m.offset))))
    }
}

/// A store's address and value slots and `(memory index, byte offset)` (see [`load_operands`]);
/// the pooled form keeps the value at `addr + 1`.
#[inline(always)]
#[allow(clippy::inline_always)] // one hot caller per store inside the fused dispatch
pub(super) fn store_operands(code: &Code, m: &StoreArg) -> Result<(Slot, Slot, (u32, u64))> {
    if m.offset == BIG_MEMARG {
        Ok((m.addr, m.addr.saturating_add(1), pooled(code, m.val)?))
    } else {
        Ok((m.addr, m.val, (0, u64::from(m.offset))))
    }
}

/// A memarg pooled in the function's `big_memargs` (a demoted wide one, or any SIMD memory op's
/// `op_simd::MemIdx`) as `(memory index, byte offset)`.
#[cold]
fn pooled(code: &Code, idx: u32) -> Result<(u32, u64)> {
    let big = ir_get(code.big_memargs()?, idx)?;
    Ok((big.memory, big.offset))
}

#[cold]
fn missing_memory() -> Error {
    crate::error::InternalError::FrameStack("cached memory not in this store").into()
}

/// The instance's memory at index `idx` (imported memories first, then defined â the wasm index
/// space). `memory.grow` is serviced separately in the run loop (for the limiter).
#[inline]
fn mem(inner: &StoreInner, instance: Instance, idx: u32) -> Result<Memory> {
    ir_get(&inner.instance(instance).memories, idx).copied()
}

impl Execution {
    /// Executes the cold memory ops (size/copy/fill/init/data.drop). Loads/stores dispatch flat in
    /// `step`; `memory.grow` suspends through the run loop (for the limiter).
    pub(super) fn exec_memory(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
        base: u32,
    ) -> Result<()> {
        let at = |s: Slot| super::step::slot(base, s);
        match op {
            // `data.drop` needs no memory (a module may carry passive data segments without one —
            // common in GC modules using `array.new_data`).
            Op::DataDrop(seg) => {
                *ir_get_mut(&mut inner.instance_mut(instance).dropped_data, *seg)? = true;
            }
            Op::MemorySize(i, s) => {
                let entity = inner.memory(mem(inner, instance, *i)?);
                let (is_64, pages) = (entity.ty.is_64(), entity.size_pages());
                self.put_index(at(*s), is_64, pages)?;
            }
            Op::MemoryFill(i, s) => self.memory_fill(inner, instance, *i, at(*s))?,
            Op::MemoryCopy(dst_i, src_i, s) => {
                self.memory_copy(inner, instance, (*dst_i, *src_i), at(*s))?;
            }
            Op::MemoryInit(seg, i, s) => self.memory_init(inner, instance, (*seg, *i), at(*s))?,
            _ => return Err(Error::msg(format!("not a memory op: {op:?}"))),
        }
        Ok(())
    }

    /// `memory.fill`: operands `dst, val, len` from slot `at`.
    fn memory_fill(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        mem_i: u32,
        at: usize,
    ) -> Result<()> {
        let memory = mem(inner, instance, mem_i)?;
        let is_64 = inner.memory(memory).ty.is_64();
        let dst = self.index_at(at, is_64)?;
        let val = self.scalar_at(at + 1)?.unwrap_i32() as u8;
        let len = self.index_at(at + 2, is_64)?;
        let bytes = &mut inner.memory_mut(memory).bytes;
        let end = checked_range(dst, len, bytes.len() as u64)?;
        bytes[dst as usize..end as usize].fill(val);
        Ok(())
    }

    /// `memory.copy dst_mem src_mem` (operands `dst, src, len` from slot `at`): the two memories may
    /// differ. Same index uses overlap-safe `copy_within`; different indices copy via a temporary.
    fn memory_copy(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        (dst_i, src_i): (u32, u32),
        at: usize,
    ) -> Result<()> {
        let (dst_mem, src_mem) = (mem(inner, instance, dst_i)?, mem(inner, instance, src_i)?);
        let (dst_64, src_64) = (
            inner.memory(dst_mem).ty.is_64(),
            inner.memory(src_mem).ty.is_64(),
        );
        let dst = self.index_at(at, dst_64)?;
        let src = self.index_at(at + 1, src_64)?;
        // The length is typed as the narrower of the two memories (#42).
        let len = self.index_at(at + 2, dst_64 && src_64)?;
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

    /// `memory.init seg mem` (operands `dst, src, len` from slot `at`): copy from passive data
    /// segment `seg` into memory `mem_i`.
    fn memory_init(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        (seg, mem_i): (u32, u32),
        at: usize,
    ) -> Result<()> {
        let is_64 = inner.memory(mem(inner, instance, mem_i)?).ty.is_64();
        let dst = self.index_at(at, is_64)?;
        let src = u64::from(self.scalar_at(at + 1)?.unwrap_i32() as u32);
        let len = u64::from(self.scalar_at(at + 2)?.unwrap_i32() as u32);
        let entity = inner.instance(instance);
        let module = entity.module.clone();
        let dropped = *ir_get(&entity.dropped_data, seg)?;
        let data = &ir_get(&module.inner().datas, seg)?.bytes;
        let data_len = if dropped { 0 } else { data.len() };
        let src_end = checked_range(src, len, data_len as u64)?;
        let mem = mem(inner, instance, mem_i)?;
        checked_range(dst, len, inner.memory(mem).bytes.len() as u64)?;
        let (dst, src, src_end, len) = (dst as usize, src as usize, src_end as usize, len as usize);
        inner.memory_mut(mem).bytes[dst..dst + len].copy_from_slice(&data[src..src_end]);
        Ok(())
    }

    /// Bounds-checks the `N`-byte access whose address operand is at slot `at`, on an
    /// already-resolved memory (one handle lookup per load/store).
    fn mem_ea<const N: usize>(
        &self,
        entity: &crate::store::MemoryEntity,
        offset: u64,
        at: usize,
    ) -> Result<usize> {
        let addr = self.index_at(at, entity.ty.is_64())?;
        let ea = addr.checked_add(offset).ok_or_else(oob)?;
        let end = ea.checked_add(N as u64).ok_or_else(oob)?;
        if end > entity.bytes.len() as u64 {
            return Err(oob());
        }
        Ok(ea as usize)
    }

    /// The arena index of `instance`'s memory `memory`. Memory 0 of the instance used last comes
    /// from the cache (`Execution::mem0`); anything else resolves through the instance (its handle
    /// checked), refreshing the cache when it is a memory 0.
    #[inline]
    fn memory_index(&mut self, inner: &StoreInner, instance: Instance, memory: u32) -> Result<u32> {
        if memory == 0 && self.mem0.0 == instance.index {
            return Ok(self.mem0.1);
        }
        let handle = mem(inner, instance, memory)?;
        inner.check_handle(handle.store);
        if memory == 0 {
            self.mem0 = (instance.index, handle.index);
        }
        Ok(handle.index)
    }

    /// An `N`-byte load from the address at slot `at` (the caller writes the result over it).
    /// Inlined into each load arm: out of line, every load paid a call/return plus a stack
    /// round-trip for the `Result<[u8; N]>` (~1.05–1.1× on memory-heavy loops, PERF-NOTES §25).
    #[allow(clippy::inline_always)] // one hot caller per load inside the fused dispatch
    #[inline(always)]
    pub(super) fn load_at<const N: usize>(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        (memory, offset): (u32, u64),
        at: usize,
    ) -> Result<[u8; N]> {
        let index = self.memory_index(inner, instance, memory)?;
        let entity = inner.resolved_memory(index).ok_or_else(missing_memory)?;
        let ea = self.mem_ea::<N>(entity, offset, at)?;
        let mut buf = [0u8; N];
        buf.copy_from_slice(&entity.bytes[ea..ea + N]);
        Ok(buf)
    }

    /// Stack-window load for the SIMD memory ops (pops the address at `sp`; the caller pushes the
    /// result).
    #[cfg(feature = "simd")]
    pub(super) fn load_n<const N: usize>(
        &mut self,
        inner: &StoreInner,
        code: &Code,
        instance: Instance,
        m: &u32,
    ) -> Result<[u8; N]> {
        let at = self.ensure_operands(1)?;
        let b = self.load_at::<N>(inner, instance, pooled(code, *m)?, at)?;
        self.sp = at; // the address is consumed; the caller pushes the result
        Ok(b)
    }

    /// Stack-window store for the SIMD memory ops (the value is already popped; pops the address).
    #[cfg(feature = "simd")]
    pub(super) fn store_n<const N: usize>(
        &mut self,
        inner: &mut StoreInner,
        code: &Code,
        instance: Instance,
        m: &u32,
        data: [u8; N],
    ) -> Result<()> {
        let at = self.ensure_operands(1)?;
        self.store_at::<N>(inner, instance, pooled(code, *m)?, at, data)?;
        self.sp = at;
        Ok(())
    }

    /// An `N`-byte store of `data` (the value operand at `at + 1`) to the address at slot `at`.
    /// Inlined like [`Execution::load_at`].
    #[allow(clippy::inline_always)] // one hot caller per store inside the fused dispatch
    #[inline(always)]
    pub(super) fn store_at<const N: usize>(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        (memory, offset): (u32, u64),
        at: usize,
        data: [u8; N],
    ) -> Result<()> {
        let index = self.memory_index(inner, instance, memory)?;
        let entity = inner
            .resolved_memory_mut(index)
            .ok_or_else(missing_memory)?;
        let ea = self.mem_ea::<N>(entity, offset, at)?;
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
