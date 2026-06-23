//! #27g GC stress + edge cases: engine-wide pressure with many stores (sequential and concurrent),
//! deep reference chains (iterative trace, no stack overflow), cycle reclamation, multi-frame root
//! enumeration during nested calls, and live-retention under a tight cap. Aims to be bullet-proof.

#![allow(clippy::unwrap_used, clippy::too_many_lines)] // inline wat strings inflate test fns

use std::sync::Arc;

use submilli_wasm::{
    Collector, Config, Engine, ExternRef, Instance, Module, Store, StoreLimitsBuilder, Val,
};

fn auto_engine() -> Engine {
    Engine::new(Config::new().collector(Collector::Auto)).unwrap()
}

/// Engine with a low GC-pressure threshold so the mailbox axis fires across stores.
fn pressured_engine(threshold: usize) -> Engine {
    let mut cfg = Config::new();
    cfg.collector(Collector::Auto)
        .gc_memory_threshold(threshold);
    Engine::new(&cfg).unwrap()
}

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

/// Allocates `n` immediately-dropped structs (unreachable garbage).
const CHURN: &str = r#"(module
  (type $s (struct (field i32) (field i32) (field i32) (field i32)))
  (func (export "churn") (param $n i32)
    (block $d (loop $l
      (br_if $d (i32.eqz (local.get $n)))
      (drop (struct.new $s (i32.const 1) (i32.const 2) (i32.const 3) (i32.const 4)))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br $l)))))"#;

#[test]
fn engine_pressure_many_stores_sequential() {
    // Many stores on one pressured engine, each churning garbage. Every store must complete and
    // stay bounded — the per-store reservation axis plus the engine-pressure mailbox keep each heap
    // small despite ~3.2 MB of garbage churned per store.
    let engine = pressured_engine(1 << 20); // 1 MiB engine-wide threshold
    let module = module(&engine, CHURN);
    for _ in 0..32 {
        let mut store = Store::new(&engine, ());
        let inst = Instance::new(&mut store, &module, &[]).unwrap();
        let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();
        churn.call(&mut store, 100_000).unwrap();
        assert!(
            store.gc_heap_capacity() < (1 << 20),
            "store stayed bounded under pressure: {} bytes",
            store.gc_heap_capacity()
        );
    }
}

#[test]
fn concurrent_stores_under_engine_pressure() {
    // The headline "server pressure with a lot of stores" case: a shared engine, many threads each
    // driving its own store, all churning concurrently. Exercises `Engine: Send + Sync`, the
    // per-store mailboxes posted/pruned from multiple threads, and that none trap, panic, or leak.
    let engine = Arc::new(pressured_engine(2 << 20));
    let module = Arc::new(module(&engine, CHURN));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let engine = Arc::clone(&engine);
            let module = Arc::clone(&module);
            std::thread::spawn(move || {
                let mut store = Store::new(&engine, ());
                let inst = Instance::new(&mut store, &module, &[]).unwrap();
                let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();
                churn.call(&mut store, 150_000).unwrap();
                store.gc_heap_capacity()
            })
        })
        .collect();

    for h in handles {
        let cap = h.join().expect("thread did not panic");
        assert!(
            cap < (4 << 20),
            "concurrent store stayed bounded: {cap} bytes"
        );
    }
}

/// A singly-linked list held by a global, with integrity walk + clear.
const CHAIN: &str = r#"(module
  (type $node (struct (field i32) (field (ref null $node))))
  (global $head (mut (ref null $node)) (ref.null $node))
  (func (export "build") (param $n i32)
    (local $h (ref null $node))
    (block $d (loop $l
      (br_if $d (i32.eqz (local.get $n)))
      (local.set $h (struct.new $node (local.get $n) (local.get $h)))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br $l)))
    (global.set $head (local.get $h)))
  (func (export "sum") (result i32)
    (local $p (ref null $node)) (local $s i32)
    (local.set $p (global.get $head))
    (block $d (loop $l
      (br_if $d (ref.is_null (local.get $p)))
      (local.set $s (i32.add (local.get $s) (struct.get $node 0 (local.get $p))))
      (local.set $p (struct.get $node 1 (local.get $p)))
      (br $l)))
    (local.get $s))
  (func (export "clear") (global.set $head (ref.null $node))))"#;

#[test]
fn deep_chain_traces_without_stack_overflow_and_reclaims() {
    // A 20k-deep chain held by a global. `gc()` must trace all 20k nodes — the worklist trace is
    // iterative, so this depth doesn't overflow the host stack — and leave them intact (the sum is
    // unchanged after collection). Clearing the root then collecting reclaims the whole chain.
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, CHAIN), &[]).unwrap();
    let depth = 20_000i32;
    inst.get_typed_func::<i32, ()>(&mut store, "build")
        .unwrap()
        .call(&mut store, depth)
        .unwrap();
    let before = store.gc_heap_capacity();

    store.gc(); // traces the full 20k-deep chain (reachable via the global) — must not overflow

    let expected = (depth as i64 * (depth as i64 + 1) / 2) as i32;
    let sum = inst.get_typed_func::<(), i32>(&mut store, "sum").unwrap();
    assert_eq!(
        sum.call(&mut store, ()).unwrap(),
        expected,
        "chain intact after gc"
    );
    assert_eq!(
        store.gc_heap_capacity(),
        before,
        "reachable chain not reclaimed"
    );

    inst.get_typed_func::<(), ()>(&mut store, "clear")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    store.gc();
    assert!(
        store.gc_heap_capacity() < before / 10,
        "chain reclaimed once unrooted: {} bytes",
        store.gc_heap_capacity()
    );
}

#[test]
fn cycle_is_reclaimed() {
    // Build a ring (each node references the next; the tail closes back to the head), drop the root,
    // and collect. A tracing collector reclaims the cycle that reference counting would leak.
    let wat = r#"(module
      (type $node (struct (field i32) (field (mut (ref null $node)))))
      (global $head (mut (ref null $node)) (ref.null $node))
      (func (export "build_ring") (param $n i32)
        (local $h (ref null $node)) (local $tail (ref null $node))
        (local.set $h (struct.new $node (i32.const 0) (ref.null $node)))
        (local.set $tail (local.get $h))
        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
        (block $d (loop $l
          (br_if $d (i32.eqz (local.get $n)))
          (local.set $h (struct.new $node (local.get $n) (local.get $h)))
          (local.set $n (i32.sub (local.get $n) (i32.const 1)))
          (br $l)))
        (struct.set $node 1 (local.get $tail) (local.get $h)) ;; close the ring
        (global.set $head (local.get $h)))
      (func (export "clear") (global.set $head (ref.null $node))))"#;
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();
    inst.get_typed_func::<i32, ()>(&mut store, "build_ring")
        .unwrap()
        .call(&mut store, 10_000)
        .unwrap();
    store.gc(); // the rooted ring survives
    let live = store.gc_heap_capacity();
    assert!(live > 0);

    inst.get_typed_func::<(), ()>(&mut store, "clear")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    store.gc();
    assert!(
        store.gc_heap_capacity() < live / 10,
        "cyclic garbage reclaimed: {} bytes",
        store.gc_heap_capacity()
    );
}

#[test]
fn nested_call_locals_survive_collection() {
    // Recurse `d` deep; each frame holds a fresh box in a local and allocates garbage (driving
    // collections during the deeper recursion). After the recursive call each frame reads its box —
    // so every frame's local must be enumerated as a root across the collections, or a box would be
    // freed-and-reused and the returned checksum would be wrong (or it would trap).
    let wat = r#"(module
      (type $box (struct (field i32)))
      (func $rec (param $d i32) (result i32)
        (local $b (ref $box))
        (local.set $b (struct.new $box (local.get $d)))
        (drop (struct.new $box (i32.const -1))) ;; garbage
        (if (result i32) (i32.eqz (local.get $d))
          (then (struct.get $box 0 (local.get $b)))
          (else (i32.add
            (struct.get $box 0 (local.get $b))
            (call $rec (i32.sub (local.get $d) (i32.const 1)))))))
      (func (export "run") (param $d i32) (result i32) (call $rec (local.get $d))))"#;
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();
    let d = 1_000i32;
    let got = inst
        .get_typed_func::<i32, i32>(&mut store, "run")
        .unwrap()
        .call(&mut store, d)
        .unwrap();
    assert_eq!(
        got,
        d * (d + 1) / 2,
        "every frame's live local survived collection"
    );
}

#[test]
fn retained_set_survives_churn_under_cap() {
    // A table retains `keep` live structs; the guest then churns far more garbage than the cap. The
    // collector reclaims the garbage each time the budget fills but keeps the table-rooted set, so
    // the run completes under the cap and the retained values are still intact.
    let wat = r#"(module
      (type $s (struct (field i32)))
      (table $t 256 (ref null $s))
      (func (export "fill") (param $keep i32)
        (local $i i32)
        (block $d (loop $l
          (br_if $d (i32.ge_u (local.get $i) (local.get $keep)))
          (table.set $t (local.get $i) (struct.new $s (local.get $i)))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $l))))
      (func (export "churn") (param $n i32)
        (block $d (loop $l
          (br_if $d (i32.eqz (local.get $n)))
          (drop (struct.new $s (i32.const -1)))
          (local.set $n (i32.sub (local.get $n) (i32.const 1)))
          (br $l))))
      (func (export "checksum") (param $keep i32) (result i32)
        (local $i i32) (local $s i32)
        (block $d (loop $l
          (br_if $d (i32.ge_u (local.get $i) (local.get $keep)))
          (local.set $s (i32.add (local.get $s)
            (struct.get $s 0 (ref.cast (ref $s) (table.get $t (local.get $i))))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $l)))
        (local.get $s)))"#;
    let engine = auto_engine();
    let mut store = Store::new(
        &engine,
        StoreLimitsBuilder::new().memory_size(2 << 20).build(),
    );
    store.limiter(|s| s);
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();
    let keep = 200i32;

    inst.get_typed_func::<i32, ()>(&mut store, "fill")
        .unwrap()
        .call(&mut store, keep)
        .unwrap();
    // ~9.6 MB of garbage through a 2 MiB cap — only possible if the retained set is kept while the
    // garbage is reclaimed.
    inst.get_typed_func::<i32, ()>(&mut store, "churn")
        .unwrap()
        .call(&mut store, 200_000)
        .unwrap();
    let sum = inst
        .get_typed_func::<i32, i32>(&mut store, "checksum")
        .unwrap()
        .call(&mut store, keep)
        .unwrap();
    assert_eq!(
        sum,
        keep * (keep - 1) / 2,
        "retained set intact after churn"
    );
}

#[test]
fn i31_and_refs_mixed_in_array() {
    // An array of `anyref` mixes unboxed `i31`s (even indices) with heap struct refs (odd). The
    // collector must skip the `i31` handles (they are not heap slots) and trace the struct refs;
    // a misread would corrupt the heap and break the checksum after `gc()`.
    let wat = r#"(module
      (type $s (struct (field i32)))
      (type $arr (array (mut anyref)))
      (global $a (mut (ref null $arr)) (ref.null $arr))
      (func (export "build") (param $n i32)
        (local $arr (ref $arr)) (local $i i32)
        (local.set $arr (array.new_default $arr (local.get $n)))
        (block $d (loop $l
          (br_if $d (i32.ge_u (local.get $i) (local.get $n)))
          (array.set $arr (local.get $arr) (local.get $i)
            (if (result anyref) (i32.eqz (i32.and (local.get $i) (i32.const 1)))
              (then (ref.i31 (local.get $i)))
              (else (struct.new $s (local.get $i)))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $l)))
        (global.set $a (local.get $arr)))
      (func (export "checksum") (param $n i32) (result i32)
        (local $arr (ref $arr)) (local $i i32) (local $s i32) (local $e anyref)
        (local.set $arr (ref.cast (ref $arr) (global.get $a)))
        (block $d (loop $l
          (br_if $d (i32.ge_u (local.get $i) (local.get $n)))
          (local.set $e (array.get $arr (local.get $arr) (local.get $i)))
          (local.set $s (i32.add (local.get $s)
            (if (result i32) (ref.test (ref i31) (local.get $e))
              (then (i31.get_s (ref.cast (ref i31) (local.get $e))))
              (else (struct.get $s 0 (ref.cast (ref $s) (local.get $e)))))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $l)))
        (local.get $s)))"#;
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();
    let n = 2_000i32;
    inst.get_typed_func::<i32, ()>(&mut store, "build")
        .unwrap()
        .call(&mut store, n)
        .unwrap();
    store.gc();
    let sum = inst
        .get_typed_func::<i32, i32>(&mut store, "checksum")
        .unwrap()
        .call(&mut store, n)
        .unwrap();
    assert_eq!(sum, n * (n - 1) / 2, "i31s and refs both intact after gc");
}

#[test]
fn retained_object_survives_many_collection_cycles() {
    // A single struct is rooted by a global. Across 100 collect-while-churning cycles its slot must
    // never be reused (it is live) and its generation must stay valid — exercising the free-list and
    // generation bookkeeping under repeated sweeps.
    let wat = r#"(module
      (type $s (struct (field i32) (field i32) (field i32) (field i32)))
      (global $keep (mut (ref null $s)) (ref.null $s))
      (func (export "build") (global.set $keep (struct.new $s (i32.const 7) (i32.const 0) (i32.const 0) (i32.const 9))))
      (func (export "churn") (param $n i32)
        (block $d (loop $l
          (br_if $d (i32.eqz (local.get $n)))
          (drop (struct.new $s (i32.const -1) (i32.const -1) (i32.const -1) (i32.const -1)))
          (local.set $n (i32.sub (local.get $n) (i32.const 1)))
          (br $l))))
      (func (export "read") (result i32)
        (i32.add
          (struct.get $s 0 (ref.cast (ref $s) (global.get $keep)))
          (struct.get $s 3 (ref.cast (ref $s) (global.get $keep))))))"#;
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();
    inst.get_typed_func::<(), ()>(&mut store, "build")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    let churn = inst.get_typed_func::<i32, ()>(&mut store, "churn").unwrap();
    let read = inst.get_typed_func::<(), i32>(&mut store, "read").unwrap();
    for _ in 0..100 {
        churn.call(&mut store, 3_000).unwrap();
        store.gc();
        assert_eq!(
            read.call(&mut store, ()).unwrap(),
            16,
            "retained slot intact across sweeps"
        );
    }
}

#[test]
fn externref_reachable_only_through_gc_field_round_trips() {
    // A host externref is internalized to `any` and parked in a global (a GC extern-wrapper object).
    // After a collection — which must trace cross-hierarchy from the wrapper into the externref arena
    // — converting it back recovers the *same* host payload.
    let wat = r#"(module
      (global $g (mut anyref) (ref.null any))
      (func (export "store_ext") (param $e externref)
        (global.set $g (any.convert_extern (local.get $e))))
      (func (export "get_ext") (result externref)
        (extern.convert_any (global.get $g))))"#;
    let engine = auto_engine();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module(&engine, wat), &[]).unwrap();

    let er = ExternRef::new(&mut store, 0xCAFE_F00D_u32).unwrap();
    let store_ext = inst.get_func(&mut store, "store_ext").unwrap();
    store_ext
        .call(&mut store, &[Val::ExternRef(Some(er))], &mut [])
        .unwrap();

    store.gc(); // wrapper survives via the global; trace must reach the externref payload

    let get_ext = inst.get_func(&mut store, "get_ext").unwrap();
    let mut out = [Val::I32(0)];
    get_ext.call(&mut store, &[], &mut out).unwrap();
    let Val::ExternRef(Some(back)) = out[0] else {
        panic!("expected a non-null externref");
    };
    let payload = back
        .data(&store)
        .unwrap()
        .and_then(|d| d.downcast_ref::<u32>().copied());
    assert_eq!(
        payload,
        Some(0xCAFE_F00D),
        "host payload survived the collection"
    );
}
