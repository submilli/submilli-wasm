//! The engine-owned canonical type registry: hash-cons whole rec groups → canonical type ids
//! (cross-module identity by structure), refcounted per group (drop-reclaimed), and materialize
//! the public handle types (`FuncType`/`StructType`/`ArrayType`) from a canonical id.

use std::collections::HashMap;

use super::keys::{
    abs_decode, array_body, body_key, body_kind, func_body, num_decode, resolve, resolve_body,
    struct_body, CBody, CField, CGroup, CHeap, CStore, CType, CVal, CanonRef,
};
use super::{AggKind, CanonicalTypeId, Finality, GroupId, ModuleType};
use crate::engine::Engine;
use crate::value::{
    ArrayType, FieldType, FuncType, HeapType, Mutability, RefType, StorageType, StructType, ValType,
};

/// A registered canonical type (body refs resolved to absolute canonical ids).
#[derive(Clone)]
struct CanonType {
    kind: AggKind,
    finality: Finality,
    supertype: Option<CanonicalTypeId>,
    body: CBody,
}

struct GroupRecord {
    key: CGroup,
    members: Vec<CanonicalTypeId>,
    refcount: u32,
}

/// The engine-owned canonical type registry (held behind a `RwLock`).
#[derive(Default)]
pub(crate) struct TypeRegistry {
    types: Vec<Option<CanonType>>,
    free_types: Vec<CanonicalTypeId>,
    groups: Vec<Option<GroupRecord>>,
    free_groups: Vec<GroupId>,
    interned: HashMap<CGroup, GroupId>,
}

impl core::fmt::Debug for TypeRegistry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TypeRegistry")
            .field("types", &self.types.len())
            .field("groups", &self.interned.len())
            .finish_non_exhaustive()
    }
}

impl TypeRegistry {
    /// Interns a module's types (rec-group order). Returns per-module-type canonical ids + the
    /// registered group ids (with multiplicity) for the `Module` to release on drop.
    pub(crate) fn intern_module(
        &mut self,
        types: &[ModuleType],
    ) -> (Vec<CanonicalTypeId>, Vec<GroupId>) {
        let mut module_to_canon = vec![CanonicalTypeId::new(u32::MAX); types.len()];
        let mut registered = Vec::new();
        let mut i = 0usize;
        while i < types.len() {
            let base = i;
            let group = types[base].group;
            while i < types.len() && types[i].group == group {
                i += 1;
            }
            let key = self.build_key(types, base, i, &module_to_canon);
            let group_id = self.intern_group(key, i - base);
            let canon = self.groups[group_id.index()]
                .as_ref()
                .expect("just interned")
                .members
                .clone();
            for (pos, m) in (base..i).enumerate() {
                module_to_canon[m] = canon[pos];
            }
            registered.push(group_id);
        }
        (module_to_canon, registered)
    }

    /// Interns a single host-built composite type (a singleton rec group); `body`'s refs are
    /// already absolute canonical ids. Returns its canonical id and the registered group id.
    fn intern_one(
        &mut self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        body: CBody,
    ) -> (CanonicalTypeId, GroupId) {
        let key = vec![CType {
            finality,
            supertype: supertype.map(CanonRef::Canon),
            body,
        }];
        let group_id = self.intern_group(key, 1);
        let id = self.groups[group_id.index()]
            .as_ref()
            .expect("interned")
            .members[0];
        (id, group_id)
    }

    /// Interns a host-built func type, returning its canonical id and group id.
    pub(crate) fn intern_func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
    ) -> (CanonicalTypeId, GroupId) {
        self.intern_one(Finality::Final, None, func_body(params, results))
    }

    /// Interns a host-built struct type, returning its canonical id and group id.
    pub(crate) fn intern_struct(
        &mut self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        fields: &[FieldType],
    ) -> (CanonicalTypeId, GroupId) {
        self.intern_one(finality, supertype, struct_body(fields))
    }

    /// Interns a host-built array type, returning its canonical id and group id.
    pub(crate) fn intern_array(
        &mut self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        field: &FieldType,
    ) -> (CanonicalTypeId, GroupId) {
        self.intern_one(finality, supertype, array_body(field))
    }

    /// Releases group ids (one decrement each); reclaims a group at refcount 0.
    pub(crate) fn release(&mut self, group_ids: &[GroupId]) {
        for &g in group_ids {
            let Some(rec) = self.groups[g.index()].as_mut() else {
                continue;
            };
            rec.refcount -= 1;
            if rec.refcount > 0 {
                continue;
            }
            let rec = self.groups[g.index()].take().expect("present");
            self.interned.remove(&rec.key);
            for &m in &rec.members {
                self.types[m.index()] = None;
                self.free_types.push(m);
            }
            self.free_groups.push(g);
        }
    }

    /// Is canonical type `sub` a declared subtype of `sup`? Walks `sub`'s supertype chain.
    pub(crate) fn is_subtype(&self, sub: CanonicalTypeId, sup: CanonicalTypeId) -> bool {
        let mut cur = Some(sub);
        while let Some(id) = cur {
            if id == sup {
                return true;
            }
            cur = self.types[id.index()].as_ref().and_then(|t| t.supertype);
        }
        false
    }

    pub(crate) fn kind(&self, id: CanonicalTypeId) -> Option<AggKind> {
        self.types
            .get(id.index())
            .and_then(|t| t.as_ref())
            .map(|t| t.kind)
    }

    pub(crate) fn finality(&self, id: CanonicalTypeId) -> Finality {
        self.types[id.index()].as_ref().expect("live type").finality
    }

    /// The materialized (public) parameter + result types of a func type.
    pub(crate) fn func_sig(
        &self,
        engine: &Engine,
        id: CanonicalTypeId,
    ) -> (Vec<ValType>, Vec<ValType>) {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Func(p, r) => (
                p.iter().map(|v| self.mat_val(engine, v)).collect(),
                r.iter().map(|v| self.mat_val(engine, v)).collect(),
            ),
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// The materialized fields of a struct type.
    pub(crate) fn struct_fields(&self, engine: &Engine, id: CanonicalTypeId) -> Vec<FieldType> {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Struct(fields) => fields.iter().map(|f| self.mat_field(engine, f)).collect(),
            _ => Vec::new(),
        }
    }

    /// The materialized element of an array type.
    pub(crate) fn array_field(&self, engine: &Engine, id: CanonicalTypeId) -> FieldType {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Array(f) => self.mat_field(engine, f),
            _ => FieldType::new(Mutability::Const, StorageType::I8),
        }
    }

    // --- interning internals ---

    fn intern_group(&mut self, key: CGroup, len: usize) -> GroupId {
        if let Some(&g) = self.interned.get(&key) {
            self.groups[g.index()]
                .as_mut()
                .expect("interned group")
                .refcount += 1;
            return g;
        }
        let mut members = Vec::with_capacity(len);
        for _ in 0..len {
            let id = self.free_types.pop().unwrap_or_else(|| {
                self.types.push(None);
                CanonicalTypeId::new((self.types.len() - 1) as u32)
            });
            members.push(id);
        }
        for (pos, ct) in key.iter().enumerate() {
            self.types[members[pos].index()] = Some(CanonType {
                kind: body_kind(&ct.body),
                finality: ct.finality,
                supertype: ct.supertype.as_ref().map(|r| resolve(r, &members)),
                body: resolve_body(&ct.body, &members),
            });
        }
        let g = self.free_groups.pop().unwrap_or_else(|| {
            self.groups.push(None);
            GroupId::new((self.groups.len() - 1) as u32)
        });
        self.groups[g.index()] = Some(GroupRecord {
            key: key.clone(),
            members,
            refcount: 1,
        });
        self.interned.insert(key, g);
        g
    }

    fn build_key(
        &self,
        types: &[ModuleType],
        base: usize,
        end: usize,
        module_to_canon: &[CanonicalTypeId],
    ) -> CGroup {
        let cref = |m: u32| -> CanonRef {
            let mi = m as usize;
            if mi >= base && mi < end {
                CanonRef::Rel((mi - base) as u32)
            } else {
                CanonRef::Canon(module_to_canon[mi])
            }
        };
        types[base..end]
            .iter()
            .map(|t| CType {
                finality: t.finality,
                supertype: t.supertype.map(&cref),
                body: body_key(&t.body, &cref),
            })
            .collect()
    }

    // --- materialization (canonical → public handle types) ---

    fn mat_val(&self, engine: &Engine, v: &CVal) -> ValType {
        match v {
            CVal::Num(c) => num_decode(*c),
            CVal::Ref(nullable, h) => {
                ValType::Ref(RefType::new(*nullable, self.mat_heap(engine, h)))
            }
        }
    }

    fn mat_heap(&self, engine: &Engine, h: &CHeap) -> HeapType {
        match h {
            CHeap::Abs(c) => abs_decode(*c),
            CHeap::Concrete(kind, CanonRef::Canon(id)) => match kind {
                AggKind::Func => HeapType::ConcreteFunc(FuncType::from_id(engine, *id)),
                AggKind::Struct => HeapType::ConcreteStruct(StructType::from_id(engine, *id)),
                AggKind::Array => HeapType::ConcreteArray(ArrayType::from_id(engine, *id)),
            },
            CHeap::Concrete(_, CanonRef::Rel(_)) => unreachable!("stored bodies use absolute ids"),
        }
    }

    fn mat_field(&self, engine: &Engine, f: &CField) -> FieldType {
        let mutability = if f.mutable {
            Mutability::Var
        } else {
            Mutability::Const
        };
        let storage = match &f.storage {
            CStore::Packed(0) => StorageType::I8,
            CStore::Packed(_) => StorageType::I16,
            CStore::Val(v) => StorageType::ValType(self.mat_val(engine, v)),
        };
        FieldType::new(mutability, storage)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
