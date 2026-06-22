//! wasmtime-compatible reference-type constants (`RefType::EXTERNREF`, `ValType::FUNCREF`, …).
//! All denote *nullable* references. Split from [`types`](super::types) to keep that file under
//! the size cap; these are inherent `const`s on the (already-exported) `RefType`/`ValType`.

use super::types::{HeapType, RefType, ValType};

impl RefType {
    /// The `funcref` type, aka `(ref null func)`.
    pub const FUNCREF: Self = RefType::new_nullable(HeapType::Func);
    /// The `nullfuncref` type, aka `(ref null nofunc)`.
    pub const NULLFUNCREF: Self = RefType::new_nullable(HeapType::NoFunc);
    /// The `externref` type, aka `(ref null extern)`.
    pub const EXTERNREF: Self = RefType::new_nullable(HeapType::Extern);
    /// The `nullexternref` type, aka `(ref null noextern)`.
    pub const NULLEXTERNREF: Self = RefType::new_nullable(HeapType::NoExtern);
    /// The `anyref` type, aka `(ref null any)`.
    pub const ANYREF: Self = RefType::new_nullable(HeapType::Any);
    /// The `eqref` type, aka `(ref null eq)`.
    pub const EQREF: Self = RefType::new_nullable(HeapType::Eq);
    /// The `i31ref` type, aka `(ref null i31)`.
    pub const I31REF: Self = RefType::new_nullable(HeapType::I31);
    /// The `structref` type, aka `(ref null struct)`.
    pub const STRUCTREF: Self = RefType::new_nullable(HeapType::Struct);
    /// The `arrayref` type, aka `(ref null array)`.
    pub const ARRAYREF: Self = RefType::new_nullable(HeapType::Array);
    /// The `nullref` type, aka `(ref null none)`.
    pub const NULLREF: Self = RefType::new_nullable(HeapType::None);
    /// The `exnref` type, aka `(ref null exn)`.
    pub const EXNREF: Self = RefType::new_nullable(HeapType::Exn);
    /// The `nullexnref` type, aka `(ref null noexn)`.
    pub const NULLEXNREF: Self = RefType::new_nullable(HeapType::NoExn);
}

impl ValType {
    /// The `funcref` type, aka `(ref null func)`.
    pub const FUNCREF: Self = ValType::Ref(RefType::FUNCREF);
    /// The `nullfuncref` type, aka `(ref null nofunc)`.
    pub const NULLFUNCREF: Self = ValType::Ref(RefType::NULLFUNCREF);
    /// The `externref` type, aka `(ref null extern)`.
    pub const EXTERNREF: Self = ValType::Ref(RefType::EXTERNREF);
    /// The `nullexternref` type, aka `(ref null noextern)`.
    pub const NULLEXTERNREF: Self = ValType::Ref(RefType::NULLEXTERNREF);
    /// The `anyref` type, aka `(ref null any)`.
    pub const ANYREF: Self = ValType::Ref(RefType::ANYREF);
    /// The `eqref` type, aka `(ref null eq)`.
    pub const EQREF: Self = ValType::Ref(RefType::EQREF);
    /// The `i31ref` type, aka `(ref null i31)`.
    pub const I31REF: Self = ValType::Ref(RefType::I31REF);
    /// The `structref` type, aka `(ref null struct)`.
    pub const STRUCTREF: Self = ValType::Ref(RefType::STRUCTREF);
    /// The `arrayref` type, aka `(ref null array)`.
    pub const ARRAYREF: Self = ValType::Ref(RefType::ARRAYREF);
    /// The `nullref` type, aka `(ref null none)`.
    pub const NULLREF: Self = ValType::Ref(RefType::NULLREF);
    /// The `exnref` type, aka `(ref null exn)`.
    pub const EXNREF: Self = ValType::Ref(RefType::EXNREF);
    /// The `nullexnref` type, aka `(ref null noexn)`.
    pub const NULLEXNREF: Self = ValType::Ref(RefType::NULLEXNREF);
}
