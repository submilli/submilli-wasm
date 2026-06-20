//! The engine-owned canonical type registry: hash-cons whole rec groups → canonical type ids
//! (cross-module identity by structure), refcounted per group (drop-reclaimed), and materialize
//! the public handle types (`FuncType`/`StructType`/`ArrayType`) from a canonical id.

use std::collections::HashMap;

use super::keys::{
    array_body, body_key, body_kind, func_body, resolve, resolve_body, struct_body, CBody, CField,
    CGroup, CHeap, CStore, CType, CVal, CanonRef,
};
use super::{AggKind, CanonicalTypeId, Finality, GroupId, ModuleType};
use crate::value::{FieldType, ValType};

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
    /// Parallel to `types`: each live canonical type's owning group (so a handle holding a
    /// `CanonicalTypeId` can find its group to incref/decref). `None` for free slots.
    type_to_group: Vec<Option<GroupId>>,
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

    /// Interns a host-built rec group (`RecGroupBuilder`). `members` is the group's IR; a body's
    /// concrete ref index `mi` resolves to a *sibling* (`mi < members.len()` → `Rel`) or to an
    /// already-canonical `externals[mi - members.len()]` (`Canon`). Returns the members' canonical
    /// ids + the group id.
    pub(crate) fn intern_host_group(
        &mut self,
        members: &[ModuleType],
        externals: &[CanonicalTypeId],
    ) -> (Vec<CanonicalTypeId>, GroupId) {
        let n = members.len();
        let cref = |mi: u32| {
            let mi = mi as usize;
            if mi < n {
                CanonRef::Rel(mi as u32)
            } else {
                CanonRef::Canon(externals[mi - n])
            }
        };
        let key: CGroup = members
            .iter()
            .map(|t| CType {
                finality: t.finality,
                supertype: t.supertype.map(&cref),
                body: body_key(&t.body, &cref),
            })
            .collect();
        let group_id = self.intern_group(key, n);
        let ids = self.groups[group_id.index()]
            .as_ref()
            .expect("just interned")
            .members
            .clone();
        (ids, group_id)
    }

    /// Releases group ids (one decrement each); reclaims a group at refcount 0 (with the
    /// edge-decref cascade in [`decref_group`]).
    pub(crate) fn release(&mut self, group_ids: &[GroupId]) {
        for &g in group_ids {
            self.decref_group(g);
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

    /// Number of currently-registered (live) rec groups — for leak/reclamation tests.
    pub(crate) fn live_group_count(&self) -> usize {
        self.interned.len()
    }

    /// Clones a func type's canonical (params, results) under the lock — phase 1 of materialization
    /// (the handle-building phase 2 runs lock-free, see the free `func_sig`).
    pub(super) fn func_body_raw(&self, id: CanonicalTypeId) -> (Vec<CVal>, Vec<CVal>) {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Func(p, r) => (p.clone(), r.clone()),
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// Clones a struct type's canonical fields under the lock (phase 1).
    pub(super) fn struct_fields_raw(&self, id: CanonicalTypeId) -> Vec<CField> {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Struct(fields) => fields.clone(),
            _ => Vec::new(),
        }
    }

    /// Clones an array type's canonical element under the lock (phase 1).
    pub(super) fn array_field_raw(&self, id: CanonicalTypeId) -> CField {
        match &self.types[id.index()].as_ref().expect("live type").body {
            CBody::Array(f) => f.clone(),
            _ => CField {
                mutable: false,
                storage: CStore::Packed(0),
            },
        }
    }

    // --- interning internals ---

    fn intern_group(&mut self, key: CGroup, len: usize) -> GroupId {
        if let Some(&g) = self.interned.get(&key) {
            self.incref_group(g);
            return g;
        }
        let g = self.free_groups.pop().unwrap_or_else(|| {
            self.groups.push(None);
            GroupId::new((self.groups.len() - 1) as u32)
        });
        let mut members = Vec::with_capacity(len);
        for _ in 0..len {
            let id = self.free_types.pop().unwrap_or_else(|| {
                self.types.push(None);
                self.type_to_group.push(None);
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
            self.type_to_group[members[pos].index()] = Some(g);
        }
        // Pin the groups this new group references (cross-group edges) — they must outlive it.
        for id in edge_canon_ids(&key) {
            if let Some(og) = self.type_to_group[id.index()] {
                self.incref_group(og);
            }
        }
        self.groups[g.index()] = Some(GroupRecord {
            key: key.clone(),
            members,
            refcount: 1,
        });
        self.interned.insert(key, g);
        g
    }

    pub(crate) fn incref_group(&mut self, g: GroupId) {
        self.groups[g.index()]
            .as_mut()
            .expect("live group")
            .refcount += 1;
    }

    /// Decrements a group's refcount; at zero, reclaims it — decref-ing its outgoing edges (which
    /// may cascade) via an explicit worklist, then freeing its type + group slots for reuse.
    pub(crate) fn decref_group(&mut self, g: GroupId) {
        let mut stack = vec![g];
        while let Some(g) = stack.pop() {
            let Some(rec) = self.groups[g.index()].as_mut() else {
                continue;
            };
            rec.refcount -= 1;
            if rec.refcount > 0 {
                continue;
            }
            let rec = self.groups[g.index()].take().expect("present");
            self.interned.remove(&rec.key);
            for id in edge_canon_ids(&rec.key) {
                if let Some(og) = self.type_to_group[id.index()] {
                    stack.push(og); // cascade: decref the referenced group next
                }
            }
            for &m in &rec.members {
                self.types[m.index()] = None;
                self.type_to_group[m.index()] = None;
                self.free_types.push(m);
            }
            self.free_groups.push(g);
        }
    }

    /// Adds a registration to the group owning `id` (a handle was cloned / materialized).
    pub(crate) fn incref_type(&mut self, id: CanonicalTypeId) {
        let g = self.type_to_group[id.index()].expect("live type");
        self.incref_group(g);
    }

    /// Removes a registration from the group owning `id` (a handle was dropped).
    pub(crate) fn decref_type(&mut self, id: CanonicalTypeId) {
        let g = self.type_to_group[id.index()].expect("live type");
        self.decref_group(g);
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
}

// --- cross-group edge tracing (the absolute canonical ids a group's key references) ---

/// The canonical ids a group's key references *outside itself* — i.e. `CanonRef::Canon` entries in
/// supertypes and bodies (siblings are `Rel` and excluded). One entry per occurrence (refcount
/// incref/decref are symmetric over this, so duplicates balance).
fn edge_canon_ids(key: &CGroup) -> Vec<CanonicalTypeId> {
    let mut out = Vec::new();
    for ct in key {
        if let Some(CanonRef::Canon(id)) = &ct.supertype {
            out.push(*id);
        }
        body_canon_ids(&ct.body, &mut out);
    }
    out
}

fn body_canon_ids(b: &CBody, out: &mut Vec<CanonicalTypeId>) {
    match b {
        CBody::Func(p, r) => {
            for v in p.iter().chain(r) {
                val_canon_id(v, out);
            }
        }
        CBody::Struct(fields) => {
            for f in fields {
                field_canon_id(f, out);
            }
        }
        CBody::Array(f) => field_canon_id(f, out),
    }
}

fn field_canon_id(f: &CField, out: &mut Vec<CanonicalTypeId>) {
    if let CStore::Val(v) = &f.storage {
        val_canon_id(v, out);
    }
}

fn val_canon_id(v: &CVal, out: &mut Vec<CanonicalTypeId>) {
    if let CVal::Ref(_, CHeap::Concrete(_, CanonRef::Canon(id))) = v {
        out.push(*id);
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
