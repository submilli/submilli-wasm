//! #28b unit tests: the exception-instance arena round-trip and the `noexn <: exn` subtyping rule.

use super::*;
use crate::value::{FuncType, HeapType, TagType, ValType};

#[test]
fn exn_arena_round_trip() {
    let engine = Engine::default();
    let mut inner = StoreInner::new(engine.clone());

    let tag = inner.alloc_tag(TagEntity {
        ty: TagType::new(FuncType::new(&engine, [ValType::I32], [])),
    });
    let handle = inner.alloc_exn(ExnEntity {
        tag,
        args: vec![Val::I32(7), Val::ExnRef(None)],
        backtrace: None,
    });

    let exn = inner.exn(handle);
    assert_eq!(exn.tag.index, tag.index);
    assert_eq!(exn.args.len(), 2);
    assert_eq!(exn.args[0].unwrap_i32(), 7);
    assert!(matches!(exn.args[1], Val::ExnRef(None)));
}

#[test]
fn noexn_is_bottom_of_exn_hierarchy() {
    assert!(HeapType::NoExn.matches(&HeapType::Exn));
    assert!(!HeapType::Exn.matches(&HeapType::NoExn));
    // …but `noexn` does not leak into a different hierarchy.
    assert!(!HeapType::NoExn.matches(&HeapType::Extern));
}
