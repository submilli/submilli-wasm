//! Tests for entity behavior, exercising the store→inner→entity path.
#![allow(clippy::unwrap_used)]

use super::{Global, Memory, Table};
use crate::engine::Engine;
use crate::store::Store;
use crate::value::{
    GlobalType, HeapType, MemoryType, Mutability, Ref, RefType, TableType, Val, ValType,
};

fn store() -> Store<()> {
    Store::new(&Engine::default(), ())
}

#[test]
fn memory_grow_and_read_write() {
    let mut store = store();
    let mem = Memory::new(&mut store, MemoryType::new(1, Some(2))).unwrap();
    assert_eq!(mem.size(&store), 1);
    assert_eq!(mem.grow(&mut store, 1).unwrap(), 1);
    assert_eq!(mem.size(&store), 2);
    assert!(mem.grow(&mut store, 1).is_err()); // would exceed the max of 2

    mem.write(&mut store, 0, &[1, 2, 3, 4]).unwrap();
    let mut buf = [0u8; 4];
    mem.read(&store, 0, &mut buf).unwrap();
    assert_eq!(buf, [1, 2, 3, 4]);

    // Reading at the very end is out of bounds.
    let size = mem.data_size(&store);
    assert!(mem.read(&store, size, &mut [0u8; 1]).is_err());
}

#[test]
fn global_get_set_and_const() {
    let mut store = store();
    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Var),
        Val::I32(1),
    )
    .unwrap();
    assert_eq!(g.get(&mut store).unwrap_i32(), 1);
    g.set(&mut store, Val::I32(42)).unwrap();
    assert_eq!(g.get(&mut store).unwrap_i32(), 42);

    let c = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(7),
    )
    .unwrap();
    assert!(c.set(&mut store, Val::I32(8)).is_err());
}

#[test]
fn table_get_set_grow_fill() {
    let mut store = store();
    let ty = TableType::new(RefType::new(true, HeapType::Func), 1, Some(3));
    let t = Table::new(&mut store, ty, Ref::Func(None)).unwrap();
    assert_eq!(t.size(&store), 1);
    assert!(matches!(t.get(&mut store, 0), Some(Ref::Func(None))));

    assert_eq!(t.grow(&mut store, 1, Ref::Func(None)).unwrap(), 1);
    assert_eq!(t.size(&store), 2);

    t.set(&mut store, 0, Ref::Func(None)).unwrap();
    assert!(t.set(&mut store, 10, Ref::Func(None)).is_err());

    t.fill(&mut store, 0, Ref::Func(None), 2).unwrap();
    assert!(t.fill(&mut store, 1, Ref::Func(None), 5).is_err());
}
