//! Inline `visit_*` lowering for the core straight-line ops the dispatcher used to handle inline:
//! constants, parametric (`drop`/`select`), and variable access (`local.*`/`global.*`). All are
//! skipped while unreachable (dead-code elision). Infallible arms still return `Result<()>` for the
//! uniform visitor delegation, hence the module-wide allow.
#![allow(clippy::unnecessary_wraps)]

use wasmparser::{Ieee32, Ieee64, ValType};

use super::Translator;
use crate::module::op::{Op, Slot};
use crate::{Error, Result};

impl Translator<'_> {
    // --- constants ---
    pub(in crate::module::compile) fn visit_i32_const(&mut self, value: i32) -> Result<()> {
        if self.reachable {
            self.fresh(|s| Op::I32Const(s, value));
            self.pending_const = self.stack.last().map(|&dst| (dst, value));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_i64_const(&mut self, value: i64) -> Result<()> {
        if self.reachable {
            self.fresh(|s| Op::I64Const(s, value));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_f32_const(&mut self, value: Ieee32) -> Result<()> {
        if self.reachable {
            self.fresh(|s| Op::F32Const(s, value.bits()));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_f64_const(&mut self, value: Ieee64) -> Result<()> {
        if self.reachable {
            self.fresh(|s| Op::F64Const(s, value.bits()));
        }
        Ok(())
    }

    // --- parametric ---
    /// `drop` emits nothing: with static operand heights, the dropped slot is simply dead.
    pub(in crate::module::compile) fn visit_drop(&mut self) -> Result<()> {
        if self.reachable {
            self.pop(1);
            // A dropped relop's result must not feed a following `br_if` fusion.
            self.clear_fusion();
        }
        Ok(())
    }

    fn lower_select(&mut self) {
        let s = self.slot_below(3);
        self.pop(3);
        self.push(1);
        self.emit(Op::Select(s));
    }

    pub(in crate::module::compile) fn visit_select(&mut self) -> Result<()> {
        if self.reachable {
            self.lower_select();
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_typed_select(&mut self, _ty: ValType) -> Result<()> {
        if self.reachable {
            self.lower_select();
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_typed_select_multi(
        &mut self,
        tys: Vec<ValType>,
    ) -> Result<()> {
        // Multi-value `select` is outside our feature target; the validator rejects it first.
        let _ = tys;
        Err(Error::msg("unsupported operator: typed_select_multi"))
    }

    // --- variable ---
    /// `local.get` emits nothing: consumers read the local's slot through its provider.
    pub(in crate::module::compile) fn visit_local_get(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            self.push_local(local_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_local_set(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            let src = self.operand(0);
            self.pop(1);
            if !self.retarget(local_index, src) {
                self.write_local(local_index, src);
            }
        }
        Ok(())
    }

    /// `local.tee`: like `local.set`, but the value stays on the stack (wherever it lives).
    pub(in crate::module::compile) fn visit_local_tee(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            let src = self.operand(0);
            if self.retarget(local_index, src) {
                self.pop(1); // the value now lives in the local
                self.push_local(local_index);
            } else {
                self.write_local(local_index, src);
            }
        }
        Ok(())
    }

    /// `local.set` fusion: a value the last op just computed into its own slot (`src`) is
    /// computed straight into the local instead. Not while another entry still reads the local's
    /// old value — its copy would have to precede the op.
    fn retarget(&mut self, local: u32, src: Slot) -> bool {
        let Some((ip, _)) = self.fusable_dst.filter(|&(_, dst)| dst == src) else {
            return false;
        };
        if self.reads_local(local) {
            return false;
        }
        let Some(dst) = self.op_get_mut(ip).and_then(Op::dst_mut) else {
            return false;
        };
        *dst = local;
        self.clear_fusion();
        true
    }

    /// Copies `src` into local `local`, first giving any entry still reading the local's old value
    /// its own copy. A local written with its own value is a no-op.
    fn write_local(&mut self, local: u32, src: Slot) {
        if src != local {
            self.preserve(local);
            self.emit(Op::Copy(local, src));
        }
    }

    pub(in crate::module::compile) fn visit_global_get(&mut self, global_index: u32) -> Result<()> {
        if self.reachable {
            self.fresh(|s| Op::GlobalGet(s, global_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_global_set(&mut self, global_index: u32) -> Result<()> {
        if self.reachable {
            self.sink(|s| Op::GlobalSet(s, global_index));
        }
        Ok(())
    }
}
