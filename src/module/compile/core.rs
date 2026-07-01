//! Inline `visit_*` lowering for the core straight-line ops the dispatcher used to handle inline:
//! constants, parametric (`drop`/`select`), and variable access (`local.*`/`global.*`). All are
//! skipped while unreachable (dead-code elision). Infallible arms still return `Result<()>` for the
//! uniform visitor delegation, hence the module-wide allow.
#![allow(clippy::unnecessary_wraps)]

use wasmparser::{Ieee32, Ieee64, ValType};

use super::Translator;
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    // --- constants ---
    pub(in crate::module::compile) fn visit_i32_const(&mut self, value: i32) -> Result<()> {
        if self.reachable {
            self.constop(Op::I32Const(value));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_i64_const(&mut self, value: i64) -> Result<()> {
        if self.reachable {
            self.constop(Op::I64Const(value));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_f32_const(&mut self, value: Ieee32) -> Result<()> {
        if self.reachable {
            self.constop(Op::F32Const(value.bits()));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_f64_const(&mut self, value: Ieee64) -> Result<()> {
        if self.reachable {
            self.constop(Op::F64Const(value.bits()));
        }
        Ok(())
    }

    // --- parametric ---
    pub(in crate::module::compile) fn visit_drop(&mut self) -> Result<()> {
        if self.reachable {
            self.pop(1);
            self.emit(Op::Drop);
        }
        Ok(())
    }

    fn lower_select(&mut self) {
        self.pop(3);
        self.push(1);
        self.emit(Op::Select);
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
    pub(in crate::module::compile) fn visit_local_get(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            self.constop(Op::LocalGet(local_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_local_set(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            self.pop(1);
            self.emit(Op::LocalSet(local_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_local_tee(&mut self, local_index: u32) -> Result<()> {
        if self.reachable {
            self.emit(Op::LocalTee(local_index)); // height-neutral
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_global_get(&mut self, global_index: u32) -> Result<()> {
        if self.reachable {
            self.constop(Op::GlobalGet(global_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_global_set(&mut self, global_index: u32) -> Result<()> {
        if self.reachable {
            self.pop(1);
            self.emit(Op::GlobalSet(global_index));
        }
        Ok(())
    }
}
