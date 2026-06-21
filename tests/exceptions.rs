//! `throw`/`throw_ref` (#28c) and `try_table` catch + unwinding (#28d/#28e). The spec `throw.wast` /
//! `throw_ref.wast` already exercise the catch matrix exhaustively; these add focused, directly
//! readable cases (and the uncaught paths the spec gates behind tail calls in `try_table.wast`).

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    AsContextMut, Engine, ExnRef, ExnRefPre, ExnType, Extern, Func, FuncType, Instance, Module,
    Store, Tag, TagType, ThrownException, Trap, Val, ValType,
};

fn run(wat: &str, export: &str, args: &[Val]) -> submilli_wasm::Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, export).unwrap();
    let mut out = vec![Val::I32(0); f.ty(&store).results().len()];
    f.call(&mut store, args, &mut out)?;
    Ok(())
}

/// Runs an exported `() -> i32` and returns its result.
fn run_i32(wat: &str, export: &str) -> i32 {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, export).unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut store, &[], &mut out).unwrap();
    out[0].unwrap_i32()
}

/// An uncaught `throw` surfaces as an error that is *not* a trap.
#[test]
fn uncaught_throw_is_a_nontrap_error() {
    let err = run(
        r#"(module (tag $e) (func (export "f") (throw $e)))"#,
        "f",
        &[],
    )
    .unwrap_err();
    assert!(
        err.downcast_ref::<Trap>().is_none(),
        "a thrown exception must not be a trap: {err}"
    );
}

/// `throw` with arguments (the args are popped to build the exception instance).
#[test]
fn throw_with_args_is_uncaught() {
    let err = run(
        r#"(module
            (tag $e (param i32 i64))
            (func (export "f") (i32.const 7) (i64.const 9) (throw $e)))"#,
        "f",
        &[],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<Trap>().is_none());
}

/// An exception thrown in a callee propagates out through the caller's frame to the embedder.
#[test]
fn throw_propagates_across_a_call() {
    let err = run(
        r#"(module
            (tag $e)
            (func $thrower (throw $e))
            (func (export "f") (call $thrower)))"#,
        "f",
        &[],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<Trap>().is_none());
}

/// `throw` is stack-polymorphic: code after it (and a block expecting a result) is dead but still
/// compiles, and the first throw is what surfaces.
#[test]
fn throw_is_stack_polymorphic() {
    run(
        r#"(module
            (tag $e0)
            (tag $e-i32 (param i32))
            (func (export "f") (throw $e0) (throw $e-i32))
            (func (export "g") (result i32) (block (result i32) (throw $e0))))"#,
        "f",
        &[],
    )
    .unwrap_err();
    // `g` likewise throws rather than returning.
    assert!(run(
        r#"(module
            (tag $e0)
            (func (export "g") (result i32) (block (result i32) (throw $e0))))"#,
        "g",
        &[],
    )
    .is_err());
}

/// `throw_ref` on a null `exnref` traps (the only reachable `throw_ref` path until #28d can supply a
/// non-null `exnref` via `catch_ref`).
#[test]
fn throw_ref_null_traps() {
    let err = run(
        r#"(module (func (export "f") (ref.null exn) (throw_ref)))"#,
        "f",
        &[],
    )
    .unwrap_err();
    assert_eq!(err.downcast_ref::<Trap>(), Some(&Trap::NullReference));
}

/// `catch` binds the tag's arguments and branches to the handler label — across a call frame (the
/// thrower is a separate function), exercising cross-frame unwinding.
#[test]
fn catch_binds_args_across_a_call() {
    let v = run_i32(
        r#"(module
            (tag $e (param i32))
            (func $thrower (i32.const 42) (throw $e))
            (func (export "f") (result i32)
              (block $h (result i32)
                (try_table (catch $e $h) (call $thrower))
                (unreachable))))"#,
        "f",
    );
    assert_eq!(v, 42);
}

/// `catch_all` catches any exception and pushes no payload.
#[test]
fn catch_all_catches_any() {
    let v = run_i32(
        r#"(module
            (tag $e)
            (func $thrower (throw $e))
            (func (export "f") (result i32)
              (block $h
                (try_table (catch_all $h) (call $thrower))
                (return (i32.const 1)))
              (i32.const 2)))"#,
        "f",
    );
    assert_eq!(v, 2); // reached via catch_all; the normal-completion path would return 1
}

/// A `try_table` whose body does not throw completes normally (handlers never fire).
#[test]
fn try_table_without_throw_runs_body() {
    let v = run_i32(
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (block $h
                (try_table (catch_all $h) (nop))
                (return (i32.const 7)))
              (i32.const 9)))"#,
        "f",
    );
    assert_eq!(v, 7);
}

/// `catch_ref` exposes the caught `exnref`; `throw_ref` re-raises it (here left uncaught).
#[test]
fn catch_ref_then_throw_ref_reraises() {
    let err = run(
        r#"(module
            (tag $e)
            (func $thrower (throw $e))
            (func (export "f")
              (block $h (result exnref)
                (try_table (catch_ref $e $h) (call $thrower))
                (unreachable))
              (throw_ref)))"#,
        "f",
        &[],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<Trap>().is_none());
}

/// An exception not matched by the inner `try_table` propagates to the enclosing one.
#[test]
fn inner_miss_outer_catches() {
    let v = run_i32(
        r#"(module
            (tag $e1)
            (tag $e2)
            (func $thrower (throw $e2))
            (func (export "f") (result i32)
              (block $outer
                (try_table (catch $e2 $outer)
                  (block $inner
                    (try_table (catch $e1 $inner) (call $thrower))
                    (unreachable)))
                (return (i32.const 1)))
              (i32.const 2)))"#,
        "f",
    );
    assert_eq!(v, 2); // $e2 escapes the inner (catch $e1) try_table and is caught by the outer
}

// ----- #28g: host exception API -----

/// The host constructs an exception object and reads its fields/tag back.
#[test]
fn host_builds_and_reads_exception() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let tt = TagType::new(FuncType::new(&engine, [ValType::I32], []));
    let tag = Tag::new(&mut store, &tt).unwrap();
    let et = ExnType::from_tag_type(&tt).unwrap();
    let pre = ExnRefPre::new(&mut store, et);
    let exn = ExnRef::new(&mut store, &pre, &tag, &[Val::I32(7)]).unwrap();
    assert_eq!(exn.field(&mut store, 0).unwrap().unwrap_i32(), 7);
    let _ = exn.tag(&mut store).unwrap();
}

/// An uncaught guest exception surfaces as `ThrownException`, with the `exnref` on the store's
/// pending slot (recoverable + inspectable).
#[test]
fn uncaught_surfaces_as_thrown_exception() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module (tag $e (param i32)) (func (export "f") (i32.const 9) (throw $e)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    let err = f.call(&mut store, &[], &mut []).unwrap_err();
    assert!(err.is::<ThrownException>(), "got: {err}");
    let exn = store.take_pending_exception().expect("pending exception");
    assert_eq!(exn.field(&mut store, 0).unwrap().unwrap_i32(), 9);
    assert!(
        store.take_pending_exception().is_none(),
        "slot cleared by take"
    );
}

/// A host function throws (`Store::throw`); the guest's `try_table` catches it, and an uncaught
/// host throw surfaces to the embedder as `ThrownException`.
#[test]
fn host_throw_is_guest_catchable() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let tt = TagType::new(FuncType::new(&engine, [], []));
    let tag = Tag::new(&mut store, &tt).unwrap();
    let pre = ExnRefPre::new(&mut store, ExnType::from_tag_type(&tt).unwrap());
    let thrower = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |mut caller, _args, _rets| {
            let exn = ExnRef::new(&mut caller, &pre, &tag, &[])?;
            caller.as_context_mut().throw(exn).map_err(Into::into)
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "" "t" (tag $t))
                (import "" "h" (func $h))
                (func (export "caught") (result i32)
                  (block $c (try_table (catch $t $c) (call $h)) (return (i32.const 0)))
                  (i32.const 1))
                (func (export "uncaught") (call $h)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(
        &mut store,
        &module,
        &[Extern::Tag(tag), Extern::Func(thrower)],
    )
    .unwrap();

    let caught = inst.get_func(&mut store, "caught").unwrap();
    let mut out = [Val::I32(0)];
    caught.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 1); // host threw → guest caught → 1

    let uncaught = inst.get_func(&mut store, "uncaught").unwrap();
    let err = uncaught.call(&mut store, &[], &mut []).unwrap_err();
    assert!(err.is::<ThrownException>(), "got: {err}");
}

/// An ordinary host `Err` (not a `throw`) is *not* an exception: `try_table` doesn't catch it and it
/// surfaces as a plain error, not `ThrownException`.
#[test]
fn ordinary_host_error_is_not_catchable() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let boomer = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |_caller, _args, _rets| Err(submilli_wasm::Error::msg("boom")),
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (tag $t)
                (import "" "h" (func $h))
                (func (export "f") (result i32)
                  (block $c (try_table (catch $t $c) (call $h)) (return (i32.const 0)))
                  (i32.const 1)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(boomer)]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    let mut out = [Val::I32(0)];
    let err = f.call(&mut store, &[], &mut out).unwrap_err();
    assert!(
        !err.is::<ThrownException>(),
        "ordinary host error must not be an exception"
    );
    assert!(err.to_string().contains("boom"));
}

/// A pending exception left undrained from an earlier uncaught throw must not be mistaken for a host
/// throw in a later call: an ordinary host error there is *not* phantom-caught by `try_table`.
#[test]
fn stale_pending_is_not_phantom_caught() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let boom = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |_caller, _args, _rets| Err(submilli_wasm::Error::msg("boom")),
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "" "h" (func $h))
                (tag $e)
                (func (export "throw_uncaught") (throw $e))
                (func (export "trap_in_try") (result i32)
                  (block $c (try_table (catch_all $c) (call $h)) (return (i32.const 0)))
                  (i32.const 1)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(boom)]).unwrap();

    // 1) Throw, uncaught — leaves a pending exception that we deliberately do NOT drain.
    let thrower = inst.get_func(&mut store, "throw_uncaught").unwrap();
    assert!(thrower
        .call(&mut store, &[], &mut [])
        .unwrap_err()
        .is::<ThrownException>());

    // 2) A later call whose host import returns an ordinary error must surface that error — not be
    //    caught by `catch_all` as if the stale pending exception had been thrown here.
    let g = inst.get_func(&mut store, "trap_in_try").unwrap();
    let mut out = [Val::I32(0)];
    let err = g.call(&mut store, &[], &mut out).unwrap_err();
    assert!(
        !err.is::<ThrownException>(),
        "stale pending exception was phantom-caught"
    );
    assert!(err.to_string().contains("boom"));
}

/// A host that calls `Store::throw` but returns `Ok` (swallowing the error) must not leave a phantom
/// pending exception after the — successful — call: the pending slot is scoped to the host call.
#[test]
fn host_throw_then_ok_leaves_no_pending() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let tt = TagType::new(FuncType::new(&engine, [], []));
    let tag = Tag::new(&mut store, &tt).unwrap();
    let pre = ExnRefPre::new(&mut store, ExnType::from_tag_type(&tt).unwrap());
    let swallow = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |mut caller, _args, _rets| {
            let exn = ExnRef::new(&mut caller, &pre, &tag, &[])?;
            let _ = caller.as_context_mut().throw::<()>(exn); // throw, then swallow (misuse)
            Ok(())
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(r#"(module (import "" "h" (func $h)) (func (export "f") (call $h)))"#)
            .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(swallow)]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    f.call(&mut store, &[], &mut []).unwrap(); // host swallowed its throw → the call succeeds
    assert!(
        store.take_pending_exception().is_none(),
        "a swallowed host throw left a phantom pending exception"
    );
}
