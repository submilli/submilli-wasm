//! Fixed-width SIMD (`v128`, #37) operator translation. `super::straight_line` routes here when an
//! operator isn't matched by an earlier category; returns `true` if it was a SIMD op (else the
//! caller falls through to `translate_numeric`). Each arm maps 1:1 to a [`SimdOp`] via the shared
//! `unop`/`binop`/`store`/`constop`/`ternary` stack-effect helpers.

use wasmparser::Operator;

use super::{memarg, Translator};
use crate::module::op::Op;
use crate::module::op_simd::SimdOp;

impl Translator<'_> {
    /// Translates a `v128` op, returning `true` if `op` was one (else the caller falls through to
    /// `translate_numeric`). Never fails — SIMD validity was already checked by the validator.
    #[allow(clippy::too_many_lines, clippy::semicolon_if_nothing_returned)]
    pub(super) fn translate_simd(&mut self, op: &Operator<'_>) -> bool {
        use Operator as W;
        match *op {
            W::V128Load { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load(memarg(m)))),
            W::V128Load8x8S { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load8x8S(memarg(m)))),
            W::V128Load8x8U { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load8x8U(memarg(m)))),
            W::V128Load16x4S { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load16x4S(memarg(m)))),
            W::V128Load16x4U { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load16x4U(memarg(m)))),
            W::V128Load32x2S { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load32x2S(memarg(m)))),
            W::V128Load32x2U { memarg: m } => self.unop(Op::Simd(SimdOp::V128Load32x2U(memarg(m)))),
            W::V128Load8Splat { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load8Splat(memarg(m))))
            }
            W::V128Load16Splat { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load16Splat(memarg(m))));
            }
            W::V128Load32Splat { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load32Splat(memarg(m))));
            }
            W::V128Load64Splat { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load64Splat(memarg(m))));
            }
            W::V128Load32Zero { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load32Zero(memarg(m))))
            }
            W::V128Load64Zero { memarg: m } => {
                self.unop(Op::Simd(SimdOp::V128Load64Zero(memarg(m))))
            }
            W::V128Store { memarg: m } => self.store(Op::Simd(SimdOp::V128Store(memarg(m)))),
            W::V128Load8Lane { memarg: m, lane } => {
                self.binop(Op::Simd(SimdOp::V128Load8Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Load16Lane { memarg: m, lane } => {
                self.binop(Op::Simd(SimdOp::V128Load16Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Load32Lane { memarg: m, lane } => {
                self.binop(Op::Simd(SimdOp::V128Load32Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Load64Lane { memarg: m, lane } => {
                self.binop(Op::Simd(SimdOp::V128Load64Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Store8Lane { memarg: m, lane } => {
                self.store(Op::Simd(SimdOp::V128Store8Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Store16Lane { memarg: m, lane } => {
                self.store(Op::Simd(SimdOp::V128Store16Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Store32Lane { memarg: m, lane } => {
                self.store(Op::Simd(SimdOp::V128Store32Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Store64Lane { memarg: m, lane } => {
                self.store(Op::Simd(SimdOp::V128Store64Lane {
                    mem: memarg(m),
                    lane,
                }));
            }
            W::V128Const { value } => {
                self.constop(Op::Simd(SimdOp::V128Const(u128::from_le_bytes(
                    *value.bytes(),
                ))));
            }
            W::I8x16Shuffle { lanes } => self.binop(Op::Simd(SimdOp::I8x16Shuffle(lanes))),
            W::I8x16ExtractLaneS { lane } => self.unop(Op::Simd(SimdOp::I8x16ExtractLaneS(lane))),
            W::I8x16ExtractLaneU { lane } => self.unop(Op::Simd(SimdOp::I8x16ExtractLaneU(lane))),
            W::I8x16ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::I8x16ReplaceLane(lane))),
            W::I16x8ExtractLaneS { lane } => self.unop(Op::Simd(SimdOp::I16x8ExtractLaneS(lane))),
            W::I16x8ExtractLaneU { lane } => self.unop(Op::Simd(SimdOp::I16x8ExtractLaneU(lane))),
            W::I16x8ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::I16x8ReplaceLane(lane))),
            W::I32x4ExtractLane { lane } => self.unop(Op::Simd(SimdOp::I32x4ExtractLane(lane))),
            W::I32x4ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::I32x4ReplaceLane(lane))),
            W::I64x2ExtractLane { lane } => self.unop(Op::Simd(SimdOp::I64x2ExtractLane(lane))),
            W::I64x2ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::I64x2ReplaceLane(lane))),
            W::F32x4ExtractLane { lane } => self.unop(Op::Simd(SimdOp::F32x4ExtractLane(lane))),
            W::F32x4ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::F32x4ReplaceLane(lane))),
            W::F64x2ExtractLane { lane } => self.unop(Op::Simd(SimdOp::F64x2ExtractLane(lane))),
            W::F64x2ReplaceLane { lane } => self.binop(Op::Simd(SimdOp::F64x2ReplaceLane(lane))),
            W::I8x16Swizzle => self.binop(Op::Simd(SimdOp::I8x16Swizzle)),
            W::I8x16Splat => self.unop(Op::Simd(SimdOp::I8x16Splat)),
            W::I16x8Splat => self.unop(Op::Simd(SimdOp::I16x8Splat)),
            W::I32x4Splat => self.unop(Op::Simd(SimdOp::I32x4Splat)),
            W::I64x2Splat => self.unop(Op::Simd(SimdOp::I64x2Splat)),
            W::F32x4Splat => self.unop(Op::Simd(SimdOp::F32x4Splat)),
            W::F64x2Splat => self.unop(Op::Simd(SimdOp::F64x2Splat)),
            W::I8x16Eq => self.binop(Op::Simd(SimdOp::I8x16Eq)),
            W::I8x16Ne => self.binop(Op::Simd(SimdOp::I8x16Ne)),
            W::I8x16LtS => self.binop(Op::Simd(SimdOp::I8x16LtS)),
            W::I8x16LtU => self.binop(Op::Simd(SimdOp::I8x16LtU)),
            W::I8x16GtS => self.binop(Op::Simd(SimdOp::I8x16GtS)),
            W::I8x16GtU => self.binop(Op::Simd(SimdOp::I8x16GtU)),
            W::I8x16LeS => self.binop(Op::Simd(SimdOp::I8x16LeS)),
            W::I8x16LeU => self.binop(Op::Simd(SimdOp::I8x16LeU)),
            W::I8x16GeS => self.binop(Op::Simd(SimdOp::I8x16GeS)),
            W::I8x16GeU => self.binop(Op::Simd(SimdOp::I8x16GeU)),
            W::I16x8Eq => self.binop(Op::Simd(SimdOp::I16x8Eq)),
            W::I16x8Ne => self.binop(Op::Simd(SimdOp::I16x8Ne)),
            W::I16x8LtS => self.binop(Op::Simd(SimdOp::I16x8LtS)),
            W::I16x8LtU => self.binop(Op::Simd(SimdOp::I16x8LtU)),
            W::I16x8GtS => self.binop(Op::Simd(SimdOp::I16x8GtS)),
            W::I16x8GtU => self.binop(Op::Simd(SimdOp::I16x8GtU)),
            W::I16x8LeS => self.binop(Op::Simd(SimdOp::I16x8LeS)),
            W::I16x8LeU => self.binop(Op::Simd(SimdOp::I16x8LeU)),
            W::I16x8GeS => self.binop(Op::Simd(SimdOp::I16x8GeS)),
            W::I16x8GeU => self.binop(Op::Simd(SimdOp::I16x8GeU)),
            W::I32x4Eq => self.binop(Op::Simd(SimdOp::I32x4Eq)),
            W::I32x4Ne => self.binop(Op::Simd(SimdOp::I32x4Ne)),
            W::I32x4LtS => self.binop(Op::Simd(SimdOp::I32x4LtS)),
            W::I32x4LtU => self.binop(Op::Simd(SimdOp::I32x4LtU)),
            W::I32x4GtS => self.binop(Op::Simd(SimdOp::I32x4GtS)),
            W::I32x4GtU => self.binop(Op::Simd(SimdOp::I32x4GtU)),
            W::I32x4LeS => self.binop(Op::Simd(SimdOp::I32x4LeS)),
            W::I32x4LeU => self.binop(Op::Simd(SimdOp::I32x4LeU)),
            W::I32x4GeS => self.binop(Op::Simd(SimdOp::I32x4GeS)),
            W::I32x4GeU => self.binop(Op::Simd(SimdOp::I32x4GeU)),
            W::I64x2Eq => self.binop(Op::Simd(SimdOp::I64x2Eq)),
            W::I64x2Ne => self.binop(Op::Simd(SimdOp::I64x2Ne)),
            W::I64x2LtS => self.binop(Op::Simd(SimdOp::I64x2LtS)),
            W::I64x2GtS => self.binop(Op::Simd(SimdOp::I64x2GtS)),
            W::I64x2LeS => self.binop(Op::Simd(SimdOp::I64x2LeS)),
            W::I64x2GeS => self.binop(Op::Simd(SimdOp::I64x2GeS)),
            W::F32x4Eq => self.binop(Op::Simd(SimdOp::F32x4Eq)),
            W::F32x4Ne => self.binop(Op::Simd(SimdOp::F32x4Ne)),
            W::F32x4Lt => self.binop(Op::Simd(SimdOp::F32x4Lt)),
            W::F32x4Gt => self.binop(Op::Simd(SimdOp::F32x4Gt)),
            W::F32x4Le => self.binop(Op::Simd(SimdOp::F32x4Le)),
            W::F32x4Ge => self.binop(Op::Simd(SimdOp::F32x4Ge)),
            W::F64x2Eq => self.binop(Op::Simd(SimdOp::F64x2Eq)),
            W::F64x2Ne => self.binop(Op::Simd(SimdOp::F64x2Ne)),
            W::F64x2Lt => self.binop(Op::Simd(SimdOp::F64x2Lt)),
            W::F64x2Gt => self.binop(Op::Simd(SimdOp::F64x2Gt)),
            W::F64x2Le => self.binop(Op::Simd(SimdOp::F64x2Le)),
            W::F64x2Ge => self.binop(Op::Simd(SimdOp::F64x2Ge)),
            W::V128Not => self.unop(Op::Simd(SimdOp::V128Not)),
            W::V128And => self.binop(Op::Simd(SimdOp::V128And)),
            W::V128AndNot => self.binop(Op::Simd(SimdOp::V128AndNot)),
            W::V128Or => self.binop(Op::Simd(SimdOp::V128Or)),
            W::V128Xor => self.binop(Op::Simd(SimdOp::V128Xor)),
            W::V128Bitselect => self.ternary(Op::Simd(SimdOp::V128Bitselect)),
            W::V128AnyTrue => self.unop(Op::Simd(SimdOp::V128AnyTrue)),
            W::I8x16Abs => self.unop(Op::Simd(SimdOp::I8x16Abs)),
            W::I8x16Neg => self.unop(Op::Simd(SimdOp::I8x16Neg)),
            W::I8x16Popcnt => self.unop(Op::Simd(SimdOp::I8x16Popcnt)),
            W::I8x16AllTrue => self.unop(Op::Simd(SimdOp::I8x16AllTrue)),
            W::I8x16Bitmask => self.unop(Op::Simd(SimdOp::I8x16Bitmask)),
            W::I8x16NarrowI16x8S => self.binop(Op::Simd(SimdOp::I8x16NarrowI16x8S)),
            W::I8x16NarrowI16x8U => self.binop(Op::Simd(SimdOp::I8x16NarrowI16x8U)),
            W::I8x16Shl => self.binop(Op::Simd(SimdOp::I8x16Shl)),
            W::I8x16ShrS => self.binop(Op::Simd(SimdOp::I8x16ShrS)),
            W::I8x16ShrU => self.binop(Op::Simd(SimdOp::I8x16ShrU)),
            W::I8x16Add => self.binop(Op::Simd(SimdOp::I8x16Add)),
            W::I8x16AddSatS => self.binop(Op::Simd(SimdOp::I8x16AddSatS)),
            W::I8x16AddSatU => self.binop(Op::Simd(SimdOp::I8x16AddSatU)),
            W::I8x16Sub => self.binop(Op::Simd(SimdOp::I8x16Sub)),
            W::I8x16SubSatS => self.binop(Op::Simd(SimdOp::I8x16SubSatS)),
            W::I8x16SubSatU => self.binop(Op::Simd(SimdOp::I8x16SubSatU)),
            W::I8x16MinS => self.binop(Op::Simd(SimdOp::I8x16MinS)),
            W::I8x16MinU => self.binop(Op::Simd(SimdOp::I8x16MinU)),
            W::I8x16MaxS => self.binop(Op::Simd(SimdOp::I8x16MaxS)),
            W::I8x16MaxU => self.binop(Op::Simd(SimdOp::I8x16MaxU)),
            W::I8x16AvgrU => self.binop(Op::Simd(SimdOp::I8x16AvgrU)),
            W::I16x8ExtAddPairwiseI8x16S => self.unop(Op::Simd(SimdOp::I16x8ExtAddPairwiseI8x16S)),
            W::I16x8ExtAddPairwiseI8x16U => self.unop(Op::Simd(SimdOp::I16x8ExtAddPairwiseI8x16U)),
            W::I16x8Abs => self.unop(Op::Simd(SimdOp::I16x8Abs)),
            W::I16x8Neg => self.unop(Op::Simd(SimdOp::I16x8Neg)),
            W::I16x8Q15MulrSatS => self.binop(Op::Simd(SimdOp::I16x8Q15MulrSatS)),
            W::I16x8AllTrue => self.unop(Op::Simd(SimdOp::I16x8AllTrue)),
            W::I16x8Bitmask => self.unop(Op::Simd(SimdOp::I16x8Bitmask)),
            W::I16x8NarrowI32x4S => self.binop(Op::Simd(SimdOp::I16x8NarrowI32x4S)),
            W::I16x8NarrowI32x4U => self.binop(Op::Simd(SimdOp::I16x8NarrowI32x4U)),
            W::I16x8ExtendLowI8x16S => self.unop(Op::Simd(SimdOp::I16x8ExtendLowI8x16S)),
            W::I16x8ExtendHighI8x16S => self.unop(Op::Simd(SimdOp::I16x8ExtendHighI8x16S)),
            W::I16x8ExtendLowI8x16U => self.unop(Op::Simd(SimdOp::I16x8ExtendLowI8x16U)),
            W::I16x8ExtendHighI8x16U => self.unop(Op::Simd(SimdOp::I16x8ExtendHighI8x16U)),
            W::I16x8Shl => self.binop(Op::Simd(SimdOp::I16x8Shl)),
            W::I16x8ShrS => self.binop(Op::Simd(SimdOp::I16x8ShrS)),
            W::I16x8ShrU => self.binop(Op::Simd(SimdOp::I16x8ShrU)),
            W::I16x8Add => self.binop(Op::Simd(SimdOp::I16x8Add)),
            W::I16x8AddSatS => self.binop(Op::Simd(SimdOp::I16x8AddSatS)),
            W::I16x8AddSatU => self.binop(Op::Simd(SimdOp::I16x8AddSatU)),
            W::I16x8Sub => self.binop(Op::Simd(SimdOp::I16x8Sub)),
            W::I16x8SubSatS => self.binop(Op::Simd(SimdOp::I16x8SubSatS)),
            W::I16x8SubSatU => self.binop(Op::Simd(SimdOp::I16x8SubSatU)),
            W::I16x8Mul => self.binop(Op::Simd(SimdOp::I16x8Mul)),
            W::I16x8MinS => self.binop(Op::Simd(SimdOp::I16x8MinS)),
            W::I16x8MinU => self.binop(Op::Simd(SimdOp::I16x8MinU)),
            W::I16x8MaxS => self.binop(Op::Simd(SimdOp::I16x8MaxS)),
            W::I16x8MaxU => self.binop(Op::Simd(SimdOp::I16x8MaxU)),
            W::I16x8AvgrU => self.binop(Op::Simd(SimdOp::I16x8AvgrU)),
            W::I16x8ExtMulLowI8x16S => self.binop(Op::Simd(SimdOp::I16x8ExtMulLowI8x16S)),
            W::I16x8ExtMulHighI8x16S => self.binop(Op::Simd(SimdOp::I16x8ExtMulHighI8x16S)),
            W::I16x8ExtMulLowI8x16U => self.binop(Op::Simd(SimdOp::I16x8ExtMulLowI8x16U)),
            W::I16x8ExtMulHighI8x16U => self.binop(Op::Simd(SimdOp::I16x8ExtMulHighI8x16U)),
            W::I32x4ExtAddPairwiseI16x8S => self.unop(Op::Simd(SimdOp::I32x4ExtAddPairwiseI16x8S)),
            W::I32x4ExtAddPairwiseI16x8U => self.unop(Op::Simd(SimdOp::I32x4ExtAddPairwiseI16x8U)),
            W::I32x4Abs => self.unop(Op::Simd(SimdOp::I32x4Abs)),
            W::I32x4Neg => self.unop(Op::Simd(SimdOp::I32x4Neg)),
            W::I32x4AllTrue => self.unop(Op::Simd(SimdOp::I32x4AllTrue)),
            W::I32x4Bitmask => self.unop(Op::Simd(SimdOp::I32x4Bitmask)),
            W::I32x4ExtendLowI16x8S => self.unop(Op::Simd(SimdOp::I32x4ExtendLowI16x8S)),
            W::I32x4ExtendHighI16x8S => self.unop(Op::Simd(SimdOp::I32x4ExtendHighI16x8S)),
            W::I32x4ExtendLowI16x8U => self.unop(Op::Simd(SimdOp::I32x4ExtendLowI16x8U)),
            W::I32x4ExtendHighI16x8U => self.unop(Op::Simd(SimdOp::I32x4ExtendHighI16x8U)),
            W::I32x4Shl => self.binop(Op::Simd(SimdOp::I32x4Shl)),
            W::I32x4ShrS => self.binop(Op::Simd(SimdOp::I32x4ShrS)),
            W::I32x4ShrU => self.binop(Op::Simd(SimdOp::I32x4ShrU)),
            W::I32x4Add => self.binop(Op::Simd(SimdOp::I32x4Add)),
            W::I32x4Sub => self.binop(Op::Simd(SimdOp::I32x4Sub)),
            W::I32x4Mul => self.binop(Op::Simd(SimdOp::I32x4Mul)),
            W::I32x4MinS => self.binop(Op::Simd(SimdOp::I32x4MinS)),
            W::I32x4MinU => self.binop(Op::Simd(SimdOp::I32x4MinU)),
            W::I32x4MaxS => self.binop(Op::Simd(SimdOp::I32x4MaxS)),
            W::I32x4MaxU => self.binop(Op::Simd(SimdOp::I32x4MaxU)),
            W::I32x4DotI16x8S => self.binop(Op::Simd(SimdOp::I32x4DotI16x8S)),
            W::I32x4ExtMulLowI16x8S => self.binop(Op::Simd(SimdOp::I32x4ExtMulLowI16x8S)),
            W::I32x4ExtMulHighI16x8S => self.binop(Op::Simd(SimdOp::I32x4ExtMulHighI16x8S)),
            W::I32x4ExtMulLowI16x8U => self.binop(Op::Simd(SimdOp::I32x4ExtMulLowI16x8U)),
            W::I32x4ExtMulHighI16x8U => self.binop(Op::Simd(SimdOp::I32x4ExtMulHighI16x8U)),
            W::I64x2Abs => self.unop(Op::Simd(SimdOp::I64x2Abs)),
            W::I64x2Neg => self.unop(Op::Simd(SimdOp::I64x2Neg)),
            W::I64x2AllTrue => self.unop(Op::Simd(SimdOp::I64x2AllTrue)),
            W::I64x2Bitmask => self.unop(Op::Simd(SimdOp::I64x2Bitmask)),
            W::I64x2ExtendLowI32x4S => self.unop(Op::Simd(SimdOp::I64x2ExtendLowI32x4S)),
            W::I64x2ExtendHighI32x4S => self.unop(Op::Simd(SimdOp::I64x2ExtendHighI32x4S)),
            W::I64x2ExtendLowI32x4U => self.unop(Op::Simd(SimdOp::I64x2ExtendLowI32x4U)),
            W::I64x2ExtendHighI32x4U => self.unop(Op::Simd(SimdOp::I64x2ExtendHighI32x4U)),
            W::I64x2Shl => self.binop(Op::Simd(SimdOp::I64x2Shl)),
            W::I64x2ShrS => self.binop(Op::Simd(SimdOp::I64x2ShrS)),
            W::I64x2ShrU => self.binop(Op::Simd(SimdOp::I64x2ShrU)),
            W::I64x2Add => self.binop(Op::Simd(SimdOp::I64x2Add)),
            W::I64x2Sub => self.binop(Op::Simd(SimdOp::I64x2Sub)),
            W::I64x2Mul => self.binop(Op::Simd(SimdOp::I64x2Mul)),
            W::I64x2ExtMulLowI32x4S => self.binop(Op::Simd(SimdOp::I64x2ExtMulLowI32x4S)),
            W::I64x2ExtMulHighI32x4S => self.binop(Op::Simd(SimdOp::I64x2ExtMulHighI32x4S)),
            W::I64x2ExtMulLowI32x4U => self.binop(Op::Simd(SimdOp::I64x2ExtMulLowI32x4U)),
            W::I64x2ExtMulHighI32x4U => self.binop(Op::Simd(SimdOp::I64x2ExtMulHighI32x4U)),
            W::F32x4Ceil => self.unop(Op::Simd(SimdOp::F32x4Ceil)),
            W::F32x4Floor => self.unop(Op::Simd(SimdOp::F32x4Floor)),
            W::F32x4Trunc => self.unop(Op::Simd(SimdOp::F32x4Trunc)),
            W::F32x4Nearest => self.unop(Op::Simd(SimdOp::F32x4Nearest)),
            W::F32x4Abs => self.unop(Op::Simd(SimdOp::F32x4Abs)),
            W::F32x4Neg => self.unop(Op::Simd(SimdOp::F32x4Neg)),
            W::F32x4Sqrt => self.unop(Op::Simd(SimdOp::F32x4Sqrt)),
            W::F32x4Add => self.binop(Op::Simd(SimdOp::F32x4Add)),
            W::F32x4Sub => self.binop(Op::Simd(SimdOp::F32x4Sub)),
            W::F32x4Mul => self.binop(Op::Simd(SimdOp::F32x4Mul)),
            W::F32x4Div => self.binop(Op::Simd(SimdOp::F32x4Div)),
            W::F32x4Min => self.binop(Op::Simd(SimdOp::F32x4Min)),
            W::F32x4Max => self.binop(Op::Simd(SimdOp::F32x4Max)),
            W::F32x4PMin => self.binop(Op::Simd(SimdOp::F32x4PMin)),
            W::F32x4PMax => self.binop(Op::Simd(SimdOp::F32x4PMax)),
            W::F64x2Ceil => self.unop(Op::Simd(SimdOp::F64x2Ceil)),
            W::F64x2Floor => self.unop(Op::Simd(SimdOp::F64x2Floor)),
            W::F64x2Trunc => self.unop(Op::Simd(SimdOp::F64x2Trunc)),
            W::F64x2Nearest => self.unop(Op::Simd(SimdOp::F64x2Nearest)),
            W::F64x2Abs => self.unop(Op::Simd(SimdOp::F64x2Abs)),
            W::F64x2Neg => self.unop(Op::Simd(SimdOp::F64x2Neg)),
            W::F64x2Sqrt => self.unop(Op::Simd(SimdOp::F64x2Sqrt)),
            W::F64x2Add => self.binop(Op::Simd(SimdOp::F64x2Add)),
            W::F64x2Sub => self.binop(Op::Simd(SimdOp::F64x2Sub)),
            W::F64x2Mul => self.binop(Op::Simd(SimdOp::F64x2Mul)),
            W::F64x2Div => self.binop(Op::Simd(SimdOp::F64x2Div)),
            W::F64x2Min => self.binop(Op::Simd(SimdOp::F64x2Min)),
            W::F64x2Max => self.binop(Op::Simd(SimdOp::F64x2Max)),
            W::F64x2PMin => self.binop(Op::Simd(SimdOp::F64x2PMin)),
            W::F64x2PMax => self.binop(Op::Simd(SimdOp::F64x2PMax)),
            W::I32x4TruncSatF32x4S => self.unop(Op::Simd(SimdOp::I32x4TruncSatF32x4S)),
            W::I32x4TruncSatF32x4U => self.unop(Op::Simd(SimdOp::I32x4TruncSatF32x4U)),
            W::F32x4ConvertI32x4S => self.unop(Op::Simd(SimdOp::F32x4ConvertI32x4S)),
            W::F32x4ConvertI32x4U => self.unop(Op::Simd(SimdOp::F32x4ConvertI32x4U)),
            W::I32x4TruncSatF64x2SZero => self.unop(Op::Simd(SimdOp::I32x4TruncSatF64x2SZero)),
            W::I32x4TruncSatF64x2UZero => self.unop(Op::Simd(SimdOp::I32x4TruncSatF64x2UZero)),
            W::F64x2ConvertLowI32x4S => self.unop(Op::Simd(SimdOp::F64x2ConvertLowI32x4S)),
            W::F64x2ConvertLowI32x4U => self.unop(Op::Simd(SimdOp::F64x2ConvertLowI32x4U)),
            W::F32x4DemoteF64x2Zero => self.unop(Op::Simd(SimdOp::F32x4DemoteF64x2Zero)),
            W::F64x2PromoteLowF32x4 => self.unop(Op::Simd(SimdOp::F64x2PromoteLowF32x4)),
            _ => return false,
        }
        true
    }
}
