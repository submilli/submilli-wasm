//! Call lowering: direct/indirect/ref calls and their tail-call (`return_call*`, #39) variants.

use super::{Op, Translator};

impl Translator<'_> {
    pub(in crate::module::compile) fn call(&mut self, func: u32) {
        let (params, results) = self.signature(self.ctx.func_types[func as usize]);
        self.pop(params);
        self.push(results);
        self.emit(Op::Call(func));
    }

    pub(in crate::module::compile) fn call_indirect(&mut self, type_index: u32, table: u32) {
        let (params, results) = self.signature(type_index);
        self.pop(1 + params); // table index + params
        self.push(results);
        self.emit(Op::CallIndirect {
            type_idx: type_index,
            table,
        });
    }

    pub(in crate::module::compile) fn call_ref(&mut self, type_index: u32) {
        let (params, results) = self.signature(type_index);
        self.pop(1 + params); // callee funcref + params
        self.push(results);
        self.emit(Op::CallRef(type_index));
    }

    /// Tail calls (#39): consume the args (and table/funcref operand) and terminate the block —
    /// the callee returns to the caller's caller, so no results land here (mirrors `ret`).
    pub(in crate::module::compile) fn return_call(&mut self, func: u32) {
        let (params, _) = self.signature(self.ctx.func_types[func as usize]);
        self.pop(params);
        self.emit(Op::ReturnCall(func));
        self.reachable = false;
    }

    pub(in crate::module::compile) fn return_call_indirect(&mut self, type_index: u32, table: u32) {
        let (params, _) = self.signature(type_index);
        self.pop(1 + params); // table index + params
        self.emit(Op::ReturnCallIndirect {
            type_idx: type_index,
            table,
        });
        self.reachable = false;
    }

    pub(in crate::module::compile) fn return_call_ref(&mut self, type_index: u32) {
        let (params, _) = self.signature(type_index);
        self.pop(1 + params); // callee funcref + params
        self.emit(Op::ReturnCallRef(type_index));
        self.reachable = false;
    }
}
