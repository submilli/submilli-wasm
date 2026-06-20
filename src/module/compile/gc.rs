//! Lowering of GC aggregate instructions (`struct.*`/`array.*`/`ref.i31`/`i31.get_*`) to internal
//! `Op`s. Variable-arity constructors (`struct.new`, `array.new_fixed`) read their operand count
//! from the module type table; everything else has a fixed stack effect.

use wasmparser::Operator;

use super::{conv_heaptype, Translator};
use crate::canon::CompositeBody;
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    #[allow(clippy::too_many_lines)] // flat per-operator routing
    pub(super) fn translate_gc(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            // --- structs ---
            W::StructNew { struct_type_index } => {
                self.pop(self.struct_field_count(struct_type_index)?);
                self.constop(Op::StructNew(struct_type_index));
            }
            W::StructNewDefault { struct_type_index } => {
                self.constop(Op::StructNewDefault(struct_type_index));
            }
            W::StructGet {
                struct_type_index,
                field_index,
            } => self.unop(Op::StructGet {
                ty: struct_type_index,
                field: field_index,
            }),
            W::StructGetS {
                struct_type_index,
                field_index,
            } => self.unop(Op::StructGetS {
                ty: struct_type_index,
                field: field_index,
            }),
            W::StructGetU {
                struct_type_index,
                field_index,
            } => self.unop(Op::StructGetU {
                ty: struct_type_index,
                field: field_index,
            }),
            W::StructSet {
                struct_type_index,
                field_index,
            } => self.store(Op::StructSet {
                ty: struct_type_index,
                field: field_index,
            }),

            // --- arrays ---
            W::ArrayNew { array_type_index } => {
                self.pop(2);
                self.constop(Op::ArrayNew(array_type_index));
            }
            W::ArrayNewDefault { array_type_index } => {
                self.unop(Op::ArrayNewDefault(array_type_index));
            }
            W::ArrayNewFixed {
                array_type_index,
                array_size,
            } => {
                self.pop(array_size);
                self.constop(Op::ArrayNewFixed {
                    ty: array_type_index,
                    n: array_size,
                });
            }
            W::ArrayNewData {
                array_type_index,
                array_data_index,
            } => {
                self.pop(2);
                self.constop(Op::ArrayNewData {
                    ty: array_type_index,
                    data: array_data_index,
                });
            }
            W::ArrayNewElem {
                array_type_index,
                array_elem_index,
            } => {
                self.pop(2);
                self.constop(Op::ArrayNewElem {
                    ty: array_type_index,
                    elem: array_elem_index,
                });
            }
            W::ArrayGet { array_type_index } => {
                self.pop(2);
                self.constop(Op::ArrayGet(array_type_index));
            }
            W::ArrayGetS { array_type_index } => {
                self.pop(2);
                self.constop(Op::ArrayGetS(array_type_index));
            }
            W::ArrayGetU { array_type_index } => {
                self.pop(2);
                self.constop(Op::ArrayGetU(array_type_index));
            }
            W::ArraySet { array_type_index } => {
                self.pop(3);
                self.emit(Op::ArraySet(array_type_index));
            }
            W::ArrayLen => self.unop(Op::ArrayLen),
            W::ArrayFill { array_type_index } => {
                self.pop(4);
                self.emit(Op::ArrayFill(array_type_index));
            }
            W::ArrayCopy {
                array_type_index_dst,
                array_type_index_src,
            } => {
                self.pop(5);
                self.emit(Op::ArrayCopy {
                    dst: array_type_index_dst,
                    src: array_type_index_src,
                });
            }
            W::ArrayInitData {
                array_type_index,
                array_data_index,
            } => {
                self.pop(4);
                self.emit(Op::ArrayInitData {
                    ty: array_type_index,
                    data: array_data_index,
                });
            }
            W::ArrayInitElem {
                array_type_index,
                array_elem_index,
            } => {
                self.pop(4);
                self.emit(Op::ArrayInitElem {
                    ty: array_type_index,
                    elem: array_elem_index,
                });
            }

            // --- i31 ---
            W::RefI31 => self.unop(Op::RefI31),
            W::I31GetS => self.unop(Op::I31GetS),
            W::I31GetU => self.unop(Op::I31GetU),

            // --- casts / equality ---
            W::RefTestNonNull { hty } => self.unop(Op::RefTest {
                ty: conv_heaptype(self.ctx.kinds, hty)?,
                nullable: false,
            }),
            W::RefTestNullable { hty } => self.unop(Op::RefTest {
                ty: conv_heaptype(self.ctx.kinds, hty)?,
                nullable: true,
            }),
            W::RefCastNonNull { hty } => self.unop(Op::RefCast {
                ty: conv_heaptype(self.ctx.kinds, hty)?,
                nullable: false,
            }),
            W::RefCastNullable { hty } => self.unop(Op::RefCast {
                ty: conv_heaptype(self.ctx.kinds, hty)?,
                nullable: true,
            }),
            W::RefEq => self.binop(Op::RefEq),
            W::AnyConvertExtern => self.unop(Op::AnyConvertExtern),
            W::ExternConvertAny => self.unop(Op::ExternConvertAny),

            ref other => return Err(Error::msg(format!("not a gc op: {other:?}"))),
        }
        Ok(())
    }

    /// The number of fields of a struct type (its `struct.new` operand count).
    fn struct_field_count(&self, ty: u32) -> Result<u32> {
        match &self.ctx.types[ty as usize].body {
            CompositeBody::Struct(fields) => Ok(fields.len() as u32),
            _ => Err(Error::msg("struct.new on non-struct type")),
        }
    }
}
