//! Inline `visit_*` lowering for fixed-width + relaxed SIMD (`v128`, #37/#38), plus the
//! `VisitSimdOperator` half of [`ValidateThenLower`]. Every op is a 1:1 map to a
//! [`SimdOp`](crate::module::op_simd::SimdOp); infallible arms still return `Result<()>` for the
//! uniform visitor delegation.
#![allow(clippy::unnecessary_wraps)]

use wasmparser::{for_each_visit_simd_operator, VisitSimdOperator};

use super::visit::ValidateThenLower;
use super::{wp_err, Translator};
use crate::module::op_simd::SimdOp;
use crate::Result;

/// Generates a nullary SIMD `visit_*` method lowering to `Op::Simd(SimdOp::$variant)`.
macro_rules! simd_nullary {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {$(
        pub(in crate::module::compile) fn $visit(&mut self) -> Result<()> {
            if self.reachable { self.$helper(SimdOp::$variant); }
            Ok(())
        }
    )*};
}

/// Like [`simd_nullary`] for ops with a single `lane: u8` immediate.
macro_rules! simd_lane {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {$(
        pub(in crate::module::compile) fn $visit(&mut self, lane: u8) -> Result<()> {
            if self.reachable { self.$helper(SimdOp::$variant(lane)); }
            Ok(())
        }
    )*};
}

/// Like [`simd_nullary`] for load/store ops with a `memarg` immediate.
macro_rules! simd_memarg {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {$(
        pub(in crate::module::compile) fn $visit(&mut self, memarg: wasmparser::MemArg) -> Result<()> {
            if self.reachable {
                let m = self.pool_memarg(memarg);
                self.$helper(SimdOp::$variant(m));
            }
            Ok(())
        }
    )*};
}

/// Like [`simd_nullary`] for lane load/store ops carrying both a `memarg` and a `lane`.
macro_rules! simd_memarg_lane {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {$(
        pub(in crate::module::compile) fn $visit(&mut self, memarg: wasmparser::MemArg, lane: u8) -> Result<()> {
            if self.reachable {
                let mem = self.pool_memarg(memarg);
                self.$helper(SimdOp::$variant { mem, lane });
            }
            Ok(())
        }
    )*};
}

impl Translator<'_> {
    simd_nullary! {
        visit_i8x16_swizzle => simd_binop I8x16Swizzle;
        visit_i8x16_splat => simd_unop I8x16Splat;
        visit_i16x8_splat => simd_unop I16x8Splat;
        visit_i32x4_splat => simd_unop I32x4Splat;
        visit_i64x2_splat => simd_unop I64x2Splat;
        visit_f32x4_splat => simd_unop F32x4Splat;
        visit_f64x2_splat => simd_unop F64x2Splat;
        visit_i8x16_eq => simd_binop I8x16Eq;
        visit_i8x16_ne => simd_binop I8x16Ne;
        visit_i8x16_lt_s => simd_binop I8x16LtS;
        visit_i8x16_lt_u => simd_binop I8x16LtU;
        visit_i8x16_gt_s => simd_binop I8x16GtS;
        visit_i8x16_gt_u => simd_binop I8x16GtU;
        visit_i8x16_le_s => simd_binop I8x16LeS;
        visit_i8x16_le_u => simd_binop I8x16LeU;
        visit_i8x16_ge_s => simd_binop I8x16GeS;
        visit_i8x16_ge_u => simd_binop I8x16GeU;
        visit_i16x8_eq => simd_binop I16x8Eq;
        visit_i16x8_ne => simd_binop I16x8Ne;
        visit_i16x8_lt_s => simd_binop I16x8LtS;
        visit_i16x8_lt_u => simd_binop I16x8LtU;
        visit_i16x8_gt_s => simd_binop I16x8GtS;
        visit_i16x8_gt_u => simd_binop I16x8GtU;
        visit_i16x8_le_s => simd_binop I16x8LeS;
        visit_i16x8_le_u => simd_binop I16x8LeU;
        visit_i16x8_ge_s => simd_binop I16x8GeS;
        visit_i16x8_ge_u => simd_binop I16x8GeU;
        visit_i32x4_eq => simd_binop I32x4Eq;
        visit_i32x4_ne => simd_binop I32x4Ne;
        visit_i32x4_lt_s => simd_binop I32x4LtS;
        visit_i32x4_lt_u => simd_binop I32x4LtU;
        visit_i32x4_gt_s => simd_binop I32x4GtS;
        visit_i32x4_gt_u => simd_binop I32x4GtU;
        visit_i32x4_le_s => simd_binop I32x4LeS;
        visit_i32x4_le_u => simd_binop I32x4LeU;
        visit_i32x4_ge_s => simd_binop I32x4GeS;
        visit_i32x4_ge_u => simd_binop I32x4GeU;
        visit_i64x2_eq => simd_binop I64x2Eq;
        visit_i64x2_ne => simd_binop I64x2Ne;
        visit_i64x2_lt_s => simd_binop I64x2LtS;
        visit_i64x2_gt_s => simd_binop I64x2GtS;
        visit_i64x2_le_s => simd_binop I64x2LeS;
        visit_i64x2_ge_s => simd_binop I64x2GeS;
        visit_f32x4_eq => simd_binop F32x4Eq;
        visit_f32x4_ne => simd_binop F32x4Ne;
        visit_f32x4_lt => simd_binop F32x4Lt;
        visit_f32x4_gt => simd_binop F32x4Gt;
        visit_f32x4_le => simd_binop F32x4Le;
        visit_f32x4_ge => simd_binop F32x4Ge;
        visit_f64x2_eq => simd_binop F64x2Eq;
        visit_f64x2_ne => simd_binop F64x2Ne;
        visit_f64x2_lt => simd_binop F64x2Lt;
        visit_f64x2_gt => simd_binop F64x2Gt;
        visit_f64x2_le => simd_binop F64x2Le;
        visit_f64x2_ge => simd_binop F64x2Ge;
        visit_v128_not => simd_unop V128Not;
        visit_v128_and => simd_binop V128And;
        visit_v128_andnot => simd_binop V128AndNot;
        visit_v128_or => simd_binop V128Or;
        visit_v128_xor => simd_binop V128Xor;
        visit_v128_bitselect => simd_ternary V128Bitselect;
        visit_v128_any_true => simd_unop V128AnyTrue;
        visit_i8x16_abs => simd_unop I8x16Abs;
        visit_i8x16_neg => simd_unop I8x16Neg;
        visit_i8x16_popcnt => simd_unop I8x16Popcnt;
        visit_i8x16_all_true => simd_unop I8x16AllTrue;
        visit_i8x16_bitmask => simd_unop I8x16Bitmask;
        visit_i8x16_narrow_i16x8_s => simd_binop I8x16NarrowI16x8S;
        visit_i8x16_narrow_i16x8_u => simd_binop I8x16NarrowI16x8U;
        visit_i8x16_shl => simd_binop I8x16Shl;
        visit_i8x16_shr_s => simd_binop I8x16ShrS;
        visit_i8x16_shr_u => simd_binop I8x16ShrU;
        visit_i8x16_add => simd_binop I8x16Add;
        visit_i8x16_add_sat_s => simd_binop I8x16AddSatS;
        visit_i8x16_add_sat_u => simd_binop I8x16AddSatU;
        visit_i8x16_sub => simd_binop I8x16Sub;
        visit_i8x16_sub_sat_s => simd_binop I8x16SubSatS;
        visit_i8x16_sub_sat_u => simd_binop I8x16SubSatU;
        visit_i8x16_min_s => simd_binop I8x16MinS;
        visit_i8x16_min_u => simd_binop I8x16MinU;
        visit_i8x16_max_s => simd_binop I8x16MaxS;
        visit_i8x16_max_u => simd_binop I8x16MaxU;
        visit_i8x16_avgr_u => simd_binop I8x16AvgrU;
        visit_i16x8_extadd_pairwise_i8x16_s => simd_unop I16x8ExtAddPairwiseI8x16S;
        visit_i16x8_extadd_pairwise_i8x16_u => simd_unop I16x8ExtAddPairwiseI8x16U;
        visit_i16x8_abs => simd_unop I16x8Abs;
        visit_i16x8_neg => simd_unop I16x8Neg;
        visit_i16x8_q15mulr_sat_s => simd_binop I16x8Q15MulrSatS;
        visit_i16x8_all_true => simd_unop I16x8AllTrue;
        visit_i16x8_bitmask => simd_unop I16x8Bitmask;
        visit_i16x8_narrow_i32x4_s => simd_binop I16x8NarrowI32x4S;
        visit_i16x8_narrow_i32x4_u => simd_binop I16x8NarrowI32x4U;
        visit_i16x8_extend_low_i8x16_s => simd_unop I16x8ExtendLowI8x16S;
        visit_i16x8_extend_high_i8x16_s => simd_unop I16x8ExtendHighI8x16S;
        visit_i16x8_extend_low_i8x16_u => simd_unop I16x8ExtendLowI8x16U;
        visit_i16x8_extend_high_i8x16_u => simd_unop I16x8ExtendHighI8x16U;
        visit_i16x8_shl => simd_binop I16x8Shl;
        visit_i16x8_shr_s => simd_binop I16x8ShrS;
        visit_i16x8_shr_u => simd_binop I16x8ShrU;
        visit_i16x8_add => simd_binop I16x8Add;
        visit_i16x8_add_sat_s => simd_binop I16x8AddSatS;
        visit_i16x8_add_sat_u => simd_binop I16x8AddSatU;
        visit_i16x8_sub => simd_binop I16x8Sub;
        visit_i16x8_sub_sat_s => simd_binop I16x8SubSatS;
        visit_i16x8_sub_sat_u => simd_binop I16x8SubSatU;
        visit_i16x8_mul => simd_binop I16x8Mul;
        visit_i16x8_min_s => simd_binop I16x8MinS;
        visit_i16x8_min_u => simd_binop I16x8MinU;
        visit_i16x8_max_s => simd_binop I16x8MaxS;
        visit_i16x8_max_u => simd_binop I16x8MaxU;
        visit_i16x8_avgr_u => simd_binop I16x8AvgrU;
        visit_i16x8_extmul_low_i8x16_s => simd_binop I16x8ExtMulLowI8x16S;
        visit_i16x8_extmul_high_i8x16_s => simd_binop I16x8ExtMulHighI8x16S;
        visit_i16x8_extmul_low_i8x16_u => simd_binop I16x8ExtMulLowI8x16U;
        visit_i16x8_extmul_high_i8x16_u => simd_binop I16x8ExtMulHighI8x16U;
        visit_i32x4_extadd_pairwise_i16x8_s => simd_unop I32x4ExtAddPairwiseI16x8S;
        visit_i32x4_extadd_pairwise_i16x8_u => simd_unop I32x4ExtAddPairwiseI16x8U;
        visit_i32x4_abs => simd_unop I32x4Abs;
        visit_i32x4_neg => simd_unop I32x4Neg;
        visit_i32x4_all_true => simd_unop I32x4AllTrue;
        visit_i32x4_bitmask => simd_unop I32x4Bitmask;
        visit_i32x4_extend_low_i16x8_s => simd_unop I32x4ExtendLowI16x8S;
        visit_i32x4_extend_high_i16x8_s => simd_unop I32x4ExtendHighI16x8S;
        visit_i32x4_extend_low_i16x8_u => simd_unop I32x4ExtendLowI16x8U;
        visit_i32x4_extend_high_i16x8_u => simd_unop I32x4ExtendHighI16x8U;
        visit_i32x4_shl => simd_binop I32x4Shl;
        visit_i32x4_shr_s => simd_binop I32x4ShrS;
        visit_i32x4_shr_u => simd_binop I32x4ShrU;
        visit_i32x4_add => simd_binop I32x4Add;
        visit_i32x4_sub => simd_binop I32x4Sub;
        visit_i32x4_mul => simd_binop I32x4Mul;
        visit_i32x4_min_s => simd_binop I32x4MinS;
        visit_i32x4_min_u => simd_binop I32x4MinU;
        visit_i32x4_max_s => simd_binop I32x4MaxS;
        visit_i32x4_max_u => simd_binop I32x4MaxU;
        visit_i32x4_dot_i16x8_s => simd_binop I32x4DotI16x8S;
        visit_i32x4_extmul_low_i16x8_s => simd_binop I32x4ExtMulLowI16x8S;
        visit_i32x4_extmul_high_i16x8_s => simd_binop I32x4ExtMulHighI16x8S;
        visit_i32x4_extmul_low_i16x8_u => simd_binop I32x4ExtMulLowI16x8U;
        visit_i32x4_extmul_high_i16x8_u => simd_binop I32x4ExtMulHighI16x8U;
        visit_i64x2_abs => simd_unop I64x2Abs;
        visit_i64x2_neg => simd_unop I64x2Neg;
        visit_i64x2_all_true => simd_unop I64x2AllTrue;
        visit_i64x2_bitmask => simd_unop I64x2Bitmask;
        visit_i64x2_extend_low_i32x4_s => simd_unop I64x2ExtendLowI32x4S;
        visit_i64x2_extend_high_i32x4_s => simd_unop I64x2ExtendHighI32x4S;
        visit_i64x2_extend_low_i32x4_u => simd_unop I64x2ExtendLowI32x4U;
        visit_i64x2_extend_high_i32x4_u => simd_unop I64x2ExtendHighI32x4U;
        visit_i64x2_shl => simd_binop I64x2Shl;
        visit_i64x2_shr_s => simd_binop I64x2ShrS;
        visit_i64x2_shr_u => simd_binop I64x2ShrU;
        visit_i64x2_add => simd_binop I64x2Add;
        visit_i64x2_sub => simd_binop I64x2Sub;
        visit_i64x2_mul => simd_binop I64x2Mul;
        visit_i64x2_extmul_low_i32x4_s => simd_binop I64x2ExtMulLowI32x4S;
        visit_i64x2_extmul_high_i32x4_s => simd_binop I64x2ExtMulHighI32x4S;
        visit_i64x2_extmul_low_i32x4_u => simd_binop I64x2ExtMulLowI32x4U;
        visit_i64x2_extmul_high_i32x4_u => simd_binop I64x2ExtMulHighI32x4U;
        visit_f32x4_ceil => simd_unop F32x4Ceil;
        visit_f32x4_floor => simd_unop F32x4Floor;
        visit_f32x4_trunc => simd_unop F32x4Trunc;
        visit_f32x4_nearest => simd_unop F32x4Nearest;
        visit_f32x4_abs => simd_unop F32x4Abs;
        visit_f32x4_neg => simd_unop F32x4Neg;
        visit_f32x4_sqrt => simd_unop F32x4Sqrt;
        visit_f32x4_add => simd_binop F32x4Add;
        visit_f32x4_sub => simd_binop F32x4Sub;
        visit_f32x4_mul => simd_binop F32x4Mul;
        visit_f32x4_div => simd_binop F32x4Div;
        visit_f32x4_min => simd_binop F32x4Min;
        visit_f32x4_max => simd_binop F32x4Max;
        visit_f32x4_pmin => simd_binop F32x4PMin;
        visit_f32x4_pmax => simd_binop F32x4PMax;
        visit_f64x2_ceil => simd_unop F64x2Ceil;
        visit_f64x2_floor => simd_unop F64x2Floor;
        visit_f64x2_trunc => simd_unop F64x2Trunc;
        visit_f64x2_nearest => simd_unop F64x2Nearest;
        visit_f64x2_abs => simd_unop F64x2Abs;
        visit_f64x2_neg => simd_unop F64x2Neg;
        visit_f64x2_sqrt => simd_unop F64x2Sqrt;
        visit_f64x2_add => simd_binop F64x2Add;
        visit_f64x2_sub => simd_binop F64x2Sub;
        visit_f64x2_mul => simd_binop F64x2Mul;
        visit_f64x2_div => simd_binop F64x2Div;
        visit_f64x2_min => simd_binop F64x2Min;
        visit_f64x2_max => simd_binop F64x2Max;
        visit_f64x2_pmin => simd_binop F64x2PMin;
        visit_f64x2_pmax => simd_binop F64x2PMax;
        visit_i32x4_trunc_sat_f32x4_s => simd_unop I32x4TruncSatF32x4S;
        visit_i32x4_trunc_sat_f32x4_u => simd_unop I32x4TruncSatF32x4U;
        visit_f32x4_convert_i32x4_s => simd_unop F32x4ConvertI32x4S;
        visit_f32x4_convert_i32x4_u => simd_unop F32x4ConvertI32x4U;
        visit_i32x4_trunc_sat_f64x2_s_zero => simd_unop I32x4TruncSatF64x2SZero;
        visit_i32x4_trunc_sat_f64x2_u_zero => simd_unop I32x4TruncSatF64x2UZero;
        visit_f64x2_convert_low_i32x4_s => simd_unop F64x2ConvertLowI32x4S;
        visit_f64x2_convert_low_i32x4_u => simd_unop F64x2ConvertLowI32x4U;
        visit_f32x4_demote_f64x2_zero => simd_unop F32x4DemoteF64x2Zero;
        visit_f64x2_promote_low_f32x4 => simd_unop F64x2PromoteLowF32x4;
        visit_i8x16_relaxed_swizzle => simd_binop I8x16RelaxedSwizzle;
        visit_i32x4_relaxed_trunc_f32x4_s => simd_unop I32x4RelaxedTruncF32x4S;
        visit_i32x4_relaxed_trunc_f32x4_u => simd_unop I32x4RelaxedTruncF32x4U;
        visit_i32x4_relaxed_trunc_f64x2_s_zero => simd_unop I32x4RelaxedTruncF64x2SZero;
        visit_i32x4_relaxed_trunc_f64x2_u_zero => simd_unop I32x4RelaxedTruncF64x2UZero;
        visit_f32x4_relaxed_madd => simd_ternary F32x4RelaxedMadd;
        visit_f32x4_relaxed_nmadd => simd_ternary F32x4RelaxedNmadd;
        visit_f64x2_relaxed_madd => simd_ternary F64x2RelaxedMadd;
        visit_f64x2_relaxed_nmadd => simd_ternary F64x2RelaxedNmadd;
        visit_i8x16_relaxed_laneselect => simd_ternary I8x16RelaxedLaneselect;
        visit_i16x8_relaxed_laneselect => simd_ternary I16x8RelaxedLaneselect;
        visit_i32x4_relaxed_laneselect => simd_ternary I32x4RelaxedLaneselect;
        visit_i64x2_relaxed_laneselect => simd_ternary I64x2RelaxedLaneselect;
        visit_f32x4_relaxed_min => simd_binop F32x4RelaxedMin;
        visit_f32x4_relaxed_max => simd_binop F32x4RelaxedMax;
        visit_f64x2_relaxed_min => simd_binop F64x2RelaxedMin;
        visit_f64x2_relaxed_max => simd_binop F64x2RelaxedMax;
        visit_i16x8_relaxed_q15mulr_s => simd_binop I16x8RelaxedQ15mulrS;
        visit_i16x8_relaxed_dot_i8x16_i7x16_s => simd_binop I16x8RelaxedDotI8x16I7x16S;
        visit_i32x4_relaxed_dot_i8x16_i7x16_add_s => simd_ternary I32x4RelaxedDotI8x16I7x16AddS;
    }

    simd_lane! {
        visit_i8x16_extract_lane_s => simd_unop I8x16ExtractLaneS;
        visit_i8x16_extract_lane_u => simd_unop I8x16ExtractLaneU;
        visit_i8x16_replace_lane => simd_binop I8x16ReplaceLane;
        visit_i16x8_extract_lane_s => simd_unop I16x8ExtractLaneS;
        visit_i16x8_extract_lane_u => simd_unop I16x8ExtractLaneU;
        visit_i16x8_replace_lane => simd_binop I16x8ReplaceLane;
        visit_i32x4_extract_lane => simd_unop I32x4ExtractLane;
        visit_i32x4_replace_lane => simd_binop I32x4ReplaceLane;
        visit_i64x2_extract_lane => simd_unop I64x2ExtractLane;
        visit_i64x2_replace_lane => simd_binop I64x2ReplaceLane;
        visit_f32x4_extract_lane => simd_unop F32x4ExtractLane;
        visit_f32x4_replace_lane => simd_binop F32x4ReplaceLane;
        visit_f64x2_extract_lane => simd_unop F64x2ExtractLane;
        visit_f64x2_replace_lane => simd_binop F64x2ReplaceLane;
    }

    simd_memarg! {
        visit_v128_load => simd_unop V128Load;
        visit_v128_load8x8_s => simd_unop V128Load8x8S;
        visit_v128_load8x8_u => simd_unop V128Load8x8U;
        visit_v128_load16x4_s => simd_unop V128Load16x4S;
        visit_v128_load16x4_u => simd_unop V128Load16x4U;
        visit_v128_load32x2_s => simd_unop V128Load32x2S;
        visit_v128_load32x2_u => simd_unop V128Load32x2U;
        visit_v128_load8_splat => simd_unop V128Load8Splat;
        visit_v128_load16_splat => simd_unop V128Load16Splat;
        visit_v128_load32_splat => simd_unop V128Load32Splat;
        visit_v128_load64_splat => simd_unop V128Load64Splat;
        visit_v128_load32_zero => simd_unop V128Load32Zero;
        visit_v128_load64_zero => simd_unop V128Load64Zero;
        visit_v128_store => simd_store V128Store;
    }

    simd_memarg_lane! {
        visit_v128_load8_lane => simd_binop V128Load8Lane;
        visit_v128_load16_lane => simd_binop V128Load16Lane;
        visit_v128_load32_lane => simd_binop V128Load32Lane;
        visit_v128_load64_lane => simd_binop V128Load64Lane;
        visit_v128_store8_lane => simd_store V128Store8Lane;
        visit_v128_store16_lane => simd_store V128Store16Lane;
        visit_v128_store32_lane => simd_store V128Store32Lane;
        visit_v128_store64_lane => simd_store V128Store64Lane;
    }

    pub(in crate::module::compile) fn visit_v128_const(
        &mut self,
        value: wasmparser::V128,
    ) -> Result<()> {
        if self.reachable {
            let index = self.pool_v128(*value.bytes())?;
            self.simd(0, 1, SimdOp::V128Const(index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_i8x16_shuffle(
        &mut self,
        lanes: [u8; 16],
    ) -> Result<()> {
        if self.reachable {
            let index = self.pool_v128(lanes)?;
            self.simd_binop(SimdOp::I8x16Shuffle(index));
        }
        Ok(())
    }

    /// Appends a 16-byte immediate to the module-wide pool (see `CodeArenas::v128s`).
    fn pool_v128(&mut self, bytes: [u8; 16]) -> Result<u32> {
        let index = u32::try_from(self.code.v128s.len())
            .map_err(|_| crate::Error::msg("too many SIMD immediates in module"))?;
        self.code.v128s.push(bytes);
        Ok(index)
    }
}

/// The uniform `VisitSimdOperator` body: validate via the validator's SIMD visitor, then delegate to
/// the translator's inherent `visit_*` of the same name.
macro_rules! validate_then_lower_simd {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*) )*) => {$(
        fn $visit(&mut self $($(, $arg: $argty)*)?) -> Result<()> {
            self.validator
                .simd_visitor(self.offset)
                .$visit($($($arg.clone()),*)?)
                .map_err(wp_err)?;
            self.translator.cur_offset = self.offset as u32;
            self.translator.$visit($($($arg),*)?)
        }
    )*};
}

impl<'a> VisitSimdOperator<'a> for ValidateThenLower<'a, '_> {
    for_each_visit_simd_operator!(validate_then_lower_simd);
}
