//! Call lowering: direct/indirect/ref calls and their tail-call (`return_call*`, #39) variants.

use super::{Op, Translator};

impl Translator<'_> {
    pub(in crate::module::compile) fn call(&mut self, func: u32) {
        let (params, results) = self.signature(self.ctx.func_types[func as usize]);
        let s = self.slot_below(params);
        self.pop(params);
        self.push(results);
        self.emit(Op::Call(func, s));
    }

    pub(in crate::module::compile) fn call_indirect(&mut self, type_index: u32, table: u32) {
        let (params, results) = self.signature(type_index);
        self.materialize(1 + params); // the args, then the table index
        let s = self.height_slot(1);
        self.pop(1 + params);
        self.push(results);
        self.emit(Op::CallIndirect {
            type_idx: type_index,
            table,
            s,
        });
    }

    pub(in crate::module::compile) fn call_ref(&mut self, type_index: u32) {
        let (params, results) = self.signature(type_index);
        self.materialize(1 + params); // the args, then the callee funcref
        let s = self.height_slot(1);
        self.pop(1 + params);
        self.push(results);
        self.emit(Op::CallRef(type_index, s));
    }

    /// Tail calls (#39): consume the args (and table/funcref operand) and terminate the block —
    /// the callee returns to the caller's caller, so no results land here (mirrors `ret`).
    pub(in crate::module::compile) fn return_call(&mut self, func: u32) {
        let (params, _) = self.signature(self.ctx.func_types[func as usize]);
        let s = self.slot_below(params);
        self.pop(params);
        self.emit(Op::ReturnCall(func, s));
        self.reachable = false;
    }

    pub(in crate::module::compile) fn return_call_indirect(&mut self, type_index: u32, table: u32) {
        let (params, _) = self.signature(type_index);
        self.materialize(1 + params); // the args, then the table index
        let s = self.height_slot(1);
        self.pop(1 + params);
        self.emit(Op::ReturnCallIndirect {
            type_idx: type_index,
            table,
            s,
        });
        self.reachable = false;
    }

    pub(in crate::module::compile) fn return_call_ref(&mut self, type_index: u32) {
        let (params, _) = self.signature(type_index);
        self.materialize(1 + params); // the args, then the callee funcref
        let s = self.height_slot(1);
        self.pop(1 + params);
        self.emit(Op::ReturnCallRef(type_index, s));
        self.reachable = false;
    }
}
