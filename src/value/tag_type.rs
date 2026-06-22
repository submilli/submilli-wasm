//! The exception tag type (split from `types.rs` to hold the file-size cap).

use super::FuncType;

/// An exception tag type: the function type whose params are the exception's argument types
/// (results are always empty). Identity at runtime is by store address (see [`Tag`](crate::Tag)),
/// not by this type; this is the signature used for import matching and payload access.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TagType {
    func: FuncType,
}

impl TagType {
    pub fn new(func: FuncType) -> TagType {
        TagType { func }
    }

    pub fn ty(&self) -> &FuncType {
        &self.func
    }
}
