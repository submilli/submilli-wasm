//! Lowering of GC aggregate instructions (`struct.*`/`array.*`/`ref.i31`/`i31.get_*`) to internal
//! `Op`s. Variable-arity constructors (`struct.new`, `array.new_fixed`) read their operand count
//! from the module type table; everything else has a fixed stack effect. Infallible arms still
//! return `Result<()>` for the uniform visitor delegation.
#![allow(clippy::unnecessary_wraps)]

use super::{conv_heaptype, Translator};
use crate::canon::CompositeBody;
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    fn struct_field_count(&self, ty: u32) -> Result<u32> {
        match &self.ctx.types[ty as usize].body {
            CompositeBody::Struct(fields) => Ok(fields.len() as u32),
            _ => Err(Error::msg("struct.new on non-struct type")),
        }
    }
}

/// Inline lowering of GC aggregate ops. `struct.new`/`array.new_fixed` pop a type-derived operand
/// count; casts resolve their heap type (fallible); the rest are fixed-effect. (`br_on_cast{,_fail}`
/// are branches — see `control`.)
impl Translator<'_> {
    super::visit::lower_nullary! {
        visit_ref_eq => window_binop RefEq;
        visit_array_len => window_unop ArrayLen;
        visit_ref_i31 => window_unop RefI31;
        visit_i31_get_s => window_unop I31GetS;
        visit_i31_get_u => window_unop I31GetU;
        visit_any_convert_extern => window_unop AnyConvertExtern;
        visit_extern_convert_any => window_unop ExternConvertAny;
    }

    pub(in crate::module::compile) fn visit_struct_new(
        &mut self,
        struct_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            let n = self.struct_field_count(struct_type_index)?;
            self.window(n, 1, |top| Op::StructNew(struct_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_struct_new_default(
        &mut self,
        struct_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(0, 1, |top| Op::StructNewDefault(struct_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_struct_get(
        &mut self,
        struct_type_index: u32,
        field_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window_unop(|top| Op::StructGet {
                ty: struct_type_index,
                field: field_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_struct_get_s(
        &mut self,
        struct_type_index: u32,
        field_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window_unop(|top| Op::StructGetS {
                ty: struct_type_index,
                field: field_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_struct_get_u(
        &mut self,
        struct_type_index: u32,
        field_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window_unop(|top| Op::StructGetU {
                ty: struct_type_index,
                field: field_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_struct_set(
        &mut self,
        struct_type_index: u32,
        field_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 0, |top| Op::StructSet {
                ty: struct_type_index,
                field: field_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_new(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayNew(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_new_default(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(1, 1, |top| Op::ArrayNewDefault(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_new_fixed(
        &mut self,
        array_type_index: u32,
        array_size: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(array_size, 1, |top| Op::ArrayNewFixed {
                ty: array_type_index,
                n: array_size,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_new_data(
        &mut self,
        array_type_index: u32,
        array_data_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayNewData {
                ty: array_type_index,
                data: array_data_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_new_elem(
        &mut self,
        array_type_index: u32,
        array_elem_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayNewElem {
                ty: array_type_index,
                elem: array_elem_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_get(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayGet(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_get_s(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayGetS(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_get_u(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(2, 1, |top| Op::ArrayGetU(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_set(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(3, 0, |top| Op::ArraySet(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_fill(
        &mut self,
        array_type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(4, 0, |top| Op::ArrayFill(array_type_index, top));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_copy(
        &mut self,
        array_type_index_dst: u32,
        array_type_index_src: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(5, 0, |top| Op::ArrayCopy {
                dst: array_type_index_dst,
                src: array_type_index_src,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_init_data(
        &mut self,
        array_type_index: u32,
        array_data_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(4, 0, |top| Op::ArrayInitData {
                ty: array_type_index,
                data: array_data_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_array_init_elem(
        &mut self,
        array_type_index: u32,
        array_elem_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.window(4, 0, |top| Op::ArrayInitElem {
                ty: array_type_index,
                elem: array_elem_index,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_test_non_null(
        &mut self,
        hty: wasmparser::HeapType,
    ) -> Result<()> {
        if self.reachable {
            let ty = conv_heaptype(self.ctx.kinds, hty)?;
            self.window_unop(|top| Op::RefTest {
                ty,
                nullable: false,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_test_nullable(
        &mut self,
        hty: wasmparser::HeapType,
    ) -> Result<()> {
        if self.reachable {
            let ty = conv_heaptype(self.ctx.kinds, hty)?;
            self.window_unop(|top| Op::RefTest {
                ty,
                nullable: true,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_cast_non_null(
        &mut self,
        hty: wasmparser::HeapType,
    ) -> Result<()> {
        if self.reachable {
            let ty = conv_heaptype(self.ctx.kinds, hty)?;
            self.window_unop(|top| Op::RefCast {
                ty,
                nullable: false,
                top,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_cast_nullable(
        &mut self,
        hty: wasmparser::HeapType,
    ) -> Result<()> {
        if self.reachable {
            let ty = conv_heaptype(self.ctx.kinds, hty)?;
            self.window_unop(|top| Op::RefCast {
                ty,
                nullable: true,
                top,
            });
        }
        Ok(())
    }
}
