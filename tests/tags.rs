//! #28a Phase-6 foundation: exception **tags** decode, link, and surface through the public API.
//! Runtime throw/catch (which observes a tag's store-address identity) lands in #28c–#28e; here we
//! cover decode + import matching (both directions) + the `Tag`/`TagType` host surface.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    Engine, Extern, FuncType, Instance, Linker, Module, Store, Tag, TagType, ValType,
};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

/// A provider defining + exporting a tag `(tag (param i32))`.
const PROVIDER: &str = r#"
    (module
      (tag $t (param i32))
      (export "t" (tag $t)))
"#;

#[test]
fn defined_tag_export_has_signature() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, PROVIDER), &[]).unwrap();

    let Some(Extern::Tag(tag)) = inst.get_export(&mut store, "t") else {
        panic!("expected a tag export");
    };
    let params: Vec<ValType> = tag.ty(&store).ty().params().collect();
    assert_eq!(params, vec![ValType::I32]);
}

#[test]
fn matching_tag_import_links() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let provider = Instance::new(&mut store, &module(&engine, PROVIDER), &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", provider).unwrap();

    // Consumer imports the same `(tag (param i32))` and re-exports it.
    let consumer = module(
        &engine,
        r#"
        (module
          (import "lib" "t" (tag $t (param i32)))
          (export "t" (tag $t)))
    "#,
    );
    let cinst = linker.instantiate(&mut store, &consumer).unwrap();
    assert!(matches!(
        cinst.get_export(&mut store, "t"),
        Some(Extern::Tag(_))
    ));
}

#[test]
fn mismatched_tag_import_fails_to_link() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let provider = Instance::new(&mut store, &module(&engine, PROVIDER), &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", provider).unwrap();

    // A different param type is a distinct func type ⇒ tags are invariant ⇒ link fails.
    let consumer = module(
        &engine,
        r#"
        (module
          (import "lib" "t" (tag $t (param i64))))
    "#,
    );
    let err = linker.instantiate(&mut store, &consumer).unwrap_err();
    assert!(
        err.to_string().contains("imported tag type mismatch"),
        "unexpected link error: {err}"
    );
}

#[test]
fn host_tag_new_and_ty_round_trip() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let ty = TagType::new(FuncType::new(&engine, [ValType::I32, ValType::I64], []));
    let tag = Tag::new(&mut store, &ty).unwrap();

    let got: Vec<ValType> = tag.ty(&store).ty().params().collect();
    assert_eq!(got, vec![ValType::I32, ValType::I64]);
}
