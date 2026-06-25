//! #27g mark-sweep collector: collector selection, reclamation (incl. cycles), reachability, the
//! host stale-handle generation fault, and `Collector::Null` staying allocate-only.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    Collector, Config, Engine, ExternRef, FieldType, Func, FuncType, GcHeapOutOfMemory, Instance,
    Module, Mutability, RootScope, StorageType, Store, StoreLimitsBuilder, StructRef, StructRefPre,
    StructType, Val, ValType,
};

fn engine_with(collector: Collector) -> Engine {
    let mut cfg = Config::new();
    cfg.collector(collector);
    Engine::new(&cfg).unwrap()
}

#[test]
fn collector_selection_accepts_supported_rejects_unsupported() {
    // Supported: Auto (default → mark-sweep), our own MarkSweep, and Null.
    for c in [Collector::Auto, Collector::MarkSweep, Collector::Null] {
        assert!(Engine::new(Config::new().collector(c)).is_ok(), "{c:?}");
    }
    // Unsupported collectors are rejected at Engine::new (a documented deviation, matching how
    // wasmtime errors when a selected collector is unavailable).
    for c in [Collector::DeferredReferenceCounting, Collector::Copying] {
        assert!(Engine::new(Config::new().collector(c)).is_err(), "{c:?}");
    }
}

/// A module that allocates `n` immediately-dropped structs (unreachable garbage), plus one that
/// builds a `head → tail` chain through a reference field and parks `head` in a global.
fn churn_module() -> &'static str {
    r#"(module
      (type $s (struct (field i32) (field i32) (field i32) (field i32)))
      (type $node (struct (field (mut i32)) (field (mut (ref null any)))))
      (global $g (mut anyref) (ref.null any))
      (func (export "churn") (param $n i32)
        (block $done
          (loop $l
            (br_if $done (i32.eqz (local.get $n)))
            (drop (struct.new $s (i32.const 1) (i32.const 2) (i32.const 3) (i32.const 4)))
            (local.set $n (i32.sub (local.get $n) (i32.const 1)))
            (br $l))))
      (func (export "build")
        (local $tail (ref $node))
        (local.set $tail (struct.new $node (i32.const 42) (ref.null any)))
        (global.set $g (struct.new $node (i32.const 1) (local.get $tail))))
      (func (export "read_tail") (result i32)
        (struct.get $node 0
          (ref.cast (ref $node)
            (struct.get $node 1
              (ref.cast (ref $node) (global.get $g)))))))"#
}

fn instance(engine: &Engine, store: &mut Store<()>) -> Instance {
    let bytes = wat::parse_str(churn_module()).unwrap();
    let module = Module::new(engine, &bytes).unwrap();
    Instance::new(&mut *store, &module, &[]).unwrap()
}

#[test]
fn guest_garbage_is_reclaimed_under_mark_sweep() {
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());
    let inst = instance(&engine, &mut store);
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();

    // 200k immediately-unreachable structs (~6.4 MB of bodies). The in-run reservation flow
    // collects as the budget fills, so the live footprint stays bounded far below that.
    churn.call(&mut store, 200_000).unwrap();
    assert!(
        store.gc_heap_capacity() < 1_000_000,
        "mark-sweep keeps the garbage bounded, got {} bytes",
        store.gc_heap_capacity()
    );
    // A final forced collection reclaims the last batch (nothing is rooted).
    store.gc();
    assert_eq!(store.gc_heap_capacity(), 0);
}

#[test]
fn within_reservation_store_does_not_collect() {
    // Collect-then-grow only applies to growth *beyond* the pre-authorized `gc_heap_reservation`. A
    // store that stays within its budget grows freely and never collects — so garbage churned within
    // the budget is RETAINED (capacity ≈ everything allocated), which proves no collection ran. A
    // forced `gc()` then reclaims it.
    let mut cfg = Config::new();
    cfg.collector(Collector::Auto).gc_heap_reservation(4 << 20); // 4 MiB free budget
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = instance(&engine, &mut store);
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();

    // ~1.2 MB of garbage, well within the 4 MiB budget → grows freely, no collection → all retained.
    churn.call(&mut store, 20_000).unwrap();
    assert!(
        store.gc_heap_capacity() > 800_000,
        "within-budget garbage is retained (no collection ran): {} bytes",
        store.gc_heap_capacity()
    );
    store.gc(); // a forced collection reclaims it
    assert_eq!(store.gc_heap_capacity(), 0);
}

#[test]
fn null_collector_never_reclaims() {
    let engine = engine_with(Collector::Null);
    let mut store = Store::new(&engine, ());
    let inst = instance(&engine, &mut store);
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();

    churn.call(&mut store, 50_000).unwrap();
    let before = store.gc_heap_capacity();
    assert!(
        before > 1_000_000,
        "null collector accumulates: {before} bytes"
    );
    store.gc(); // a no-op under Null
    assert_eq!(store.gc_heap_capacity(), before);
}

#[test]
fn null_collector_traps_when_limiter_denies_gc_growth() {
    // Guest GC allocation routes its byte reservation through the limiter even under `Null` (which
    // never collects to make room). With a zero `gc_heap_reservation` (limiter-strict — every grow
    // is limiter-gated), a denying limiter makes the first allocation trap rather than grow
    // unbounded. `memory_size` bounds the GC heap (it has no maximum of its own); the module has no
    // linear memory, so this caps only the GC heap.
    let mut cfg = Config::new();
    cfg.collector(Collector::Null).gc_heap_reservation(0);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, StoreLimitsBuilder::new().memory_size(1024).build());
    store.limiter(|s| s);

    let bytes = wat::parse_str(churn_module()).unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();

    // The first `struct.new` needs a reservation larger than the 1 KiB the limiter allows → trap.
    let err = churn.call(&mut store, 10).unwrap_err();
    assert!(
        err.downcast_ref::<GcHeapOutOfMemory<()>>().is_some(),
        "limiter-denied GC growth is GcHeapOutOfMemory, got: {err:#}"
    );
    assert_eq!(store.gc_heap_capacity(), 0, "nothing was allocated");
}

#[test]
fn array_new_data_routes_through_limiter() {
    // Regression for #31: `array.new_data` now routes its allocation through the limiter's GC-byte
    // reservation (it previously checked only the static abort ceiling, ignoring the limiter). With a
    // zero free budget and a 1 KiB cap, building a 2 KiB array from a data segment must trap as
    // `GcHeapOutOfMemory` rather than silently allocate up to the abort cap.
    let mut cfg = Config::new();
    cfg.collector(Collector::Null).gc_heap_reservation(0);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, StoreLimitsBuilder::new().memory_size(1024).build());
    store.limiter(|s| s);

    let wat = format!(
        "(module (type $a (array (mut i8))) (data $d \"{}\") \
         (func (export \"make\") i32.const 0 i32.const 2048 array.new_data $a $d drop))",
        "a".repeat(2048)
    );
    let module = Module::new(&engine, wat::parse_str(&wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let make = inst.get_typed_func::<(), ()>(&mut store, "make").unwrap();

    let err = make.call(&mut store, ()).unwrap_err();
    assert!(
        err.downcast_ref::<GcHeapOutOfMemory<()>>().is_some(),
        "array.new_data growth must be limiter-gated, got: {err:#}"
    );
    assert_eq!(store.gc_heap_capacity(), 0, "nothing was allocated");
}

#[test]
fn gc_heap_reservation_is_free_budget_then_limiter_gated() {
    // With a pre-authorized `gc_heap_reservation`, growth *within* it skips the limiter entirely —
    // so even a deny-all limiter doesn't stop allocation up to the reservation; only growth beyond
    // it is limiter-gated.
    let mut cfg = Config::new();
    cfg.collector(Collector::Null).gc_heap_reservation(1 << 20); // 1 MiB free budget
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, StoreLimitsBuilder::new().memory_size(0).build());
    store.limiter(|s| s); // denies *all* growth

    let bytes = wat::parse_str(churn_module()).unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();

    // A few thousand small structs (well under 1 MiB) allocate from the free reservation — the
    // deny-all limiter is never consulted, so this succeeds.
    churn.call(&mut store, 2_000).unwrap();
    assert!(store.gc_heap_capacity() > 0);

    // Allocating past the 1 MiB reservation needs a limiter-gated grow, which is denied → trap.
    let err = churn.call(&mut store, 200_000).unwrap_err();
    assert!(
        err.downcast_ref::<GcHeapOutOfMemory<()>>().is_some(),
        "limiter-denied GC growth is GcHeapOutOfMemory, got: {err:#}"
    );
}

#[test]
fn mark_sweep_frees_memory_under_limiter_pressure() {
    // A tight limiter caps the GC heap. The mark-sweep collector reclaims the (unreachable) garbage
    // each time the budget fills, so a churn that allocates *far more than the cap* still completes
    // — the GC frees memory to make room instead of the limiter trapping. The null collector, which
    // can't reclaim, hits the cap and traps.
    let cap = 1 << 20; // 1 MiB — vs ~9.6 MB of bodies below
    let count = 200_000;
    let bytes = wat::parse_str(churn_module()).unwrap();

    let setup = |collector| {
        let engine = engine_with(collector);
        let mut store = Store::new(&engine, StoreLimitsBuilder::new().memory_size(cap).build());
        store.limiter(|s| s);
        let module = Module::new(&engine, &bytes).unwrap();
        let inst = Instance::new(&mut store, &module, &[]).unwrap();
        let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();
        (store, churn)
    };

    // Mark-sweep: completes despite allocating ~9.6 MB through a 1 MiB cap; live footprint stays
    // bounded under the cap (the collector kept reclaiming the garbage).
    let (mut store, churn) = setup(Collector::Auto);
    churn.call(&mut store, count).unwrap();
    assert!(
        store.gc_heap_capacity() < cap,
        "GC held the heap under the cap: {} bytes",
        store.gc_heap_capacity()
    );

    // Null collector, same cap: garbage accumulates past 1 MiB → the limiter denies the grow → trap.
    let (mut store, churn) = setup(Collector::Null);
    let err = churn.call(&mut store, count).unwrap_err();
    assert!(
        err.downcast_ref::<GcHeapOutOfMemory<()>>().is_some(),
        "limiter-denied GC growth is GcHeapOutOfMemory, got: {err:#}"
    );
}

#[test]
fn host_built_garbage_is_collectable_across_calls() {
    // A host fn that builds a GC object per call (like Submilli's `fs.lines` building one string
    // per line) must NOT pin every object for the store's life — the per-call host-root scope
    // unwinds them, so a guest-driven collection reclaims the transient ones. Here the guest loops
    // calling the host fn (which builds + drops a struct) and allocating its own struct to drive
    // collection; under an 8 MiB cap, ~200k host structs (~13 MB if leaked) stays bounded.
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(
        &engine,
        StoreLimitsBuilder::new().memory_size(8 << 20).build(),
    );
    store.limiter(|s| s);

    let fields: Vec<_> = (0..8)
        .map(|_| FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32)))
        .collect();
    let st = StructType::new(&engine, fields).unwrap();
    let pre = StructRefPre::new(&mut store, st);
    let make = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |mut caller, _params, _results| {
            // Build a struct and drop the handle — pure garbage once the call returns.
            StructRef::new(&mut caller, &pre, &[Val::I32(0); 8])?;
            Ok(())
        },
    );

    let wat = r#"(module
        (import "host" "make" (func $make))
        (type $g (struct (field i32)))
        (func (export "run") (param $n i32)
          (block $done
            (loop $l
              (br_if $done (i32.eqz (local.get $n)))
              (call $make)                            ;; host builds + drops a struct
              (drop (struct.new $g (i32.const 0)))    ;; guest alloc drives collection
              (local.set $n (i32.sub (local.get $n) (i32.const 1)))
              (br $l)))))"#;
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[make.into()]).unwrap();
    inst.get_typed_func::<i32, ()>(&mut store, "run")
        .unwrap()
        .call(&mut store, 200_000)
        .unwrap();
    assert!(
        store.gc_heap_capacity() < (8 << 20),
        "host garbage stayed bounded: {} bytes",
        store.gc_heap_capacity()
    );
}

#[test]
fn reachable_through_field_survives_collection() {
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());
    let inst = instance(&engine, &mut store);

    inst.get_typed_func::<(), ()>(&mut store, "build")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    // `head` is rooted by the global; `tail` is reachable only through `head`'s reference field.
    store.gc();
    // Following head → tail still reads the tail's value, so the field-reachable node survived.
    let read = inst
        .get_typed_func::<(), i32>(&mut store, "read_tail")
        .unwrap();
    assert_eq!(read.call(&mut store, ()).unwrap(), 42);
}

#[test]
fn host_root_keeps_object_alive_then_stale_handle_faults() {
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());
    let st = StructType::new(
        &engine,
        [FieldType::new(
            Mutability::Var,
            StorageType::ValType(ValType::I32),
        )],
    )
    .unwrap();
    let pre = StructRefPre::new(&mut store, st);

    // A handle created outside a scope is rooted for the store's life: it survives a collection.
    let live = StructRef::new(&mut store, &pre, &[Val::I32(5)]).unwrap();
    store.gc();
    assert_eq!(live.field(&store, 0).unwrap().unwrap_i32(), 5);

    // A handle whose RootScope has dropped is unrooted: a collection frees its slot, and a fresh
    // allocation reuses it — the stale handle must fault via the generation check, not read the
    // new object.
    let stale = {
        let mut scope = RootScope::new(&mut store);
        StructRef::new(&mut scope, &pre, &[Val::I32(7)]).unwrap()
    };
    store.gc();
    let _reused = StructRef::new(&mut store, &pre, &[Val::I32(9)]).unwrap();
    assert!(
        stale.field(&store, 0).is_err(),
        "stale handle after slot reuse must fault"
    );
}

#[test]
fn host_allocation_collects_then_grows_within_one_call() {
    // Host GC allocation is now a first-class safe point: it reserves through the limiter and
    // collects-then-grows on its own (the parked guest operands/params are rooted via `gc_roots`
    // for the call's duration). A host fn that builds far more garbage than the cap — each object
    // in a per-iteration `RootScope` that drops it — completes under a tight limiter WITHOUT any
    // guest-driven collection, because host-side collection keeps reclaiming. `gc_heap_reservation`
    // is 0 (every grow is limiter-gated), so this exercises the host collect-then-grow path, not the
    // free budget. The null collector (can't reclaim) hits the cap and traps — proving the contrast.
    let cap = 1 << 20; // 1 MiB — vs ~7.6 MB of bodies the host churns below
    let count = 100_000;

    let run = |collector| -> submilli_wasm::Result<usize> {
        let mut cfg = Config::new();
        cfg.collector(collector).gc_heap_reservation(0);
        let engine = Engine::new(&cfg).unwrap();
        let mut store = Store::new(&engine, StoreLimitsBuilder::new().memory_size(cap).build());
        store.limiter(|s| s);

        let fields: Vec<_> = (0..8)
            .map(|_| FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32)))
            .collect();
        let st = StructType::new(&engine, fields).unwrap();
        let pre = StructRefPre::new(&mut store, st);
        let churn = Func::new(
            &mut store,
            FuncType::new(&engine, [], []),
            move |mut caller, _params, _results| {
                for _ in 0..count {
                    // Each struct is rooted only for its scope, then dropped → pure garbage that
                    // host-side collection reclaims to make room for the next.
                    let mut scope = RootScope::new(&mut caller);
                    StructRef::new(&mut scope, &pre, &[Val::I32(0); 8])?;
                }
                Ok(())
            },
        );

        let wat = r#"(module
            (import "host" "churn" (func $churn))
            (func (export "run") (call $churn)))"#;
        let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
        let inst = Instance::new(&mut store, &module, &[churn.into()]).unwrap();
        inst.get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())?;
        Ok(store.gc_heap_capacity())
    };

    // Mark-sweep: the host call completes despite churning ~7.6 MB through a 1 MiB cap, and the live
    // heap stays under the cap (host-side collection reclaimed each iteration's garbage).
    let held =
        run(Collector::Auto).expect("host-side collect-then-grow keeps the call under the cap");
    assert!(held < cap, "host garbage stayed bounded: {held} bytes");

    // Null collector, same cap: garbage accumulates (no reclamation) until the limiter denies → trap.
    let err = run(Collector::Null).unwrap_err();
    assert!(
        err.downcast_ref::<GcHeapOutOfMemory<()>>().is_some(),
        "limiter-denied GC growth is GcHeapOutOfMemory, got: {err:#}"
    );
}

// --- externref / exn arena reclamation (#27g follow-up) ---------------------------------------

/// A module that throws + catches (discarding the `exnref`) `n` times — each iteration allocates an
/// exception instance that is unreachable once caught, so the exn arena must reclaim them.
const EXN_SPIN: &str = r#"(module
    (tag $t)
    (func $boom (throw $t))
    (func (export "spin") (param $n i32)
      (loop $l
        (block $c (try_table (catch_all $c) (call $boom)))
        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
        (br_if $l (local.get $n)))))"#;

#[test]
fn exn_arena_reclaimed_under_mark_sweep() {
    // A throw-loop allocates an `ExnEntity` per iteration; caught with `catch_all` (no binding), each
    // is immediately unreachable. The exns charge the GC budget, so the throw path's reservation flow
    // collects as it fills — the live footprint stays bounded far below the ~32 MB it would reach if
    // the arena were still grow-only (the pre-fix OOM-abort DoS).
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());
    let module = Module::new(&engine, wat::parse_str(EXN_SPIN).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let spin = inst.get_typed_func::<i32, ()>(&mut store, "spin").unwrap();

    spin.call(&mut store, 500_000).unwrap();
    assert!(
        store.gc_heap_capacity() < 1_000_000,
        "exn garbage stayed bounded: {} bytes",
        store.gc_heap_capacity()
    );
    store.gc();
    assert_eq!(
        store.gc_heap_capacity(),
        0,
        "all exns reclaimed once unreachable"
    );
}

#[test]
fn exn_throw_loop_completes_under_tight_limiter() {
    // The same throw-loop under a 256 KiB cap with no free budget: every reservation grow is
    // limiter-gated, but the collector keeps reclaiming the unreachable exns, so the loop completes
    // far past the cap instead of trapping. (Grow-only, this would OOM/deny long before finishing.)
    let mut cfg = Config::new();
    cfg.collector(Collector::Auto).gc_heap_reservation(0);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(
        &engine,
        StoreLimitsBuilder::new().memory_size(256 * 1024).build(),
    );
    store.limiter(|s| s);
    let module = Module::new(&engine, wat::parse_str(EXN_SPIN).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let spin = inst.get_typed_func::<i32, ()>(&mut store, "spin").unwrap();

    spin.call(&mut store, 200_000).unwrap();
    assert!(store.gc_heap_capacity() <= 256 * 1024);
}

#[test]
fn externref_arena_reclaimed_after_scope() {
    // Host externrefs are rooted only for their `RootScope`; once it drops they're unreachable, so a
    // collection reclaims the arena (no longer grow-only).
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());
    {
        let mut scope = RootScope::new(&mut store);
        for i in 0..2_000u32 {
            ExternRef::new(&mut scope, i).unwrap();
        }
    } // scope drop → the 2,000 host roots are removed
    store.gc();
    assert_eq!(
        store.gc_heap_capacity(),
        0,
        "externrefs reclaimed once their RootScope dropped"
    );
}

#[test]
fn rooted_externref_survives_then_stale_after_reclaim() {
    let engine = engine_with(Collector::Auto);
    let mut store = Store::new(&engine, ());

    // Created outside a scope → rooted for the store's life: survives a collection, payload readable.
    let live = ExternRef::new(&mut store, 123u32).unwrap();
    store.gc();
    let got = live.data(&store).unwrap().unwrap();
    assert_eq!(got.downcast_ref::<u32>(), Some(&123));

    // Created in a scope that then drops → unrooted: a collection frees its slot and bumps the
    // generation, so the escaped handle faults rather than reading a reused entry.
    let stale = {
        let mut scope = RootScope::new(&mut store);
        ExternRef::new(&mut scope, 99u32).unwrap()
    };
    store.gc();
    assert!(
        stale.data(&store).is_err(),
        "stale externref handle must fault after its entry is reclaimed"
    );
}
