//! Type-section decoding: wasm rec groups → module-relative IR [`ModuleType`]s. Kinds of all
//! types are resolved first so forward/recursive references inside a rec group convert cleanly;
//! engine interning (canonical ids) happens later, in `ModuleInner::intern`.

use crate::canon::{AggKind, CompositeBody, IrField, IrStorage, IrVal, ModuleType};
use crate::module::compile::conv_valtype;
use crate::value::Finality;
use crate::{Error, Result};

fn wp_err(e: wasmparser::BinaryReaderError) -> Error {
    Error::msg(e.to_string())
}

pub(super) fn parse_types(
    out: &mut Vec<ModuleType>,
    reader: wasmparser::TypeSectionReader<'_>,
) -> Result<()> {
    let mut raw: Vec<(u32, wasmparser::SubType)> = Vec::new();
    for (group, rec) in reader.into_iter().enumerate() {
        for sub in rec.map_err(wp_err)?.into_types() {
            raw.push((group as u32, sub));
        }
    }
    let kinds: Vec<AggKind> = raw
        .iter()
        .map(|(_, s)| composite_kind(&s.composite_type.inner))
        .collect::<Result<_>>()?;
    for (g, sub) in &raw {
        let finality = if sub.is_final {
            Finality::Final
        } else {
            Finality::NonFinal
        };
        let supertype = sub.supertype_idx.and_then(|p| p.unpack().as_module_index());
        let body = conv_composite(&kinds, &sub.composite_type.inner)?;
        out.push(ModuleType {
            group: *g,
            finality,
            supertype,
            body,
        });
    }
    Ok(())
}

fn composite_kind(inner: &wasmparser::CompositeInnerType) -> Result<AggKind> {
    use wasmparser::CompositeInnerType as C;
    Ok(match inner {
        C::Func(_) => AggKind::Func,
        C::Struct(_) => AggKind::Struct,
        C::Array(_) => AggKind::Array,
        C::Cont(_) => return Err(Error::msg("continuation types not supported")),
    })
}

fn conv_composite(
    kinds: &[AggKind],
    inner: &wasmparser::CompositeInnerType,
) -> Result<CompositeBody> {
    use wasmparser::CompositeInnerType as C;
    Ok(match inner {
        C::Func(ft) => CompositeBody::Func {
            params: conv_valtypes(kinds, ft.params())?,
            results: conv_valtypes(kinds, ft.results())?,
        },
        C::Struct(st) => CompositeBody::Struct(
            st.fields
                .iter()
                .map(|f| conv_fieldtype(kinds, f))
                .collect::<Result<_>>()?,
        ),
        C::Array(at) => CompositeBody::Array(conv_fieldtype(kinds, &at.0)?),
        C::Cont(_) => return Err(Error::msg("continuation types not supported")),
    })
}

fn conv_fieldtype(kinds: &[AggKind], f: &wasmparser::FieldType) -> Result<IrField> {
    let storage = match f.element_type {
        wasmparser::StorageType::I8 => IrStorage::I8,
        wasmparser::StorageType::I16 => IrStorage::I16,
        wasmparser::StorageType::Val(v) => IrStorage::Val(conv_valtype(kinds, v)?),
    };
    Ok(IrField {
        mutable: f.mutable,
        storage,
    })
}

fn conv_valtypes(kinds: &[AggKind], tys: &[wasmparser::ValType]) -> Result<Vec<IrVal>> {
    tys.iter()
        .copied()
        .map(|t| conv_valtype(kinds, t))
        .collect()
}
