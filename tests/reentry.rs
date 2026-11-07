//! Host re-entry isolation (unified shared `Execution`): when a host function re-enters wasm via
//! `Func::call`, the inner call shares the outer call's operand/frame stacks, separated by a
//! delimiter. These tests pin the boundary guarantees — a trap/exception in the inner call must not
//! corrupt or be caught by the outer call, the outer operand stack must survive a re-entry round
//! trip, `max_wasm_stack` must be enforced across the boundary, and a backtrace captured deep in a
//! re-entry chain must span all the guest frames.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    AsContextMut, Caller, Config, Engine, Extern, Func, FuncType, Instance, Module, Store, Tag,
    TagType, ThrownException, Val, ValType, WasmBacktrace,
};

/// Pulls export `name` as a `Func` from inside a host call.
fn export_func<T: 'static>(caller: &mut Caller<'_, T>, name: &str) -> Func {
    match caller.get_export(name) {
        Some(Extern::Func(f)) => f,
        _ => panic!("missing export {name}"),
    }
}

/// Case 1 — a trap in the inner (re-entered) call surfaces only to the host's `Func::call`; when the
/// host swallows it, the outer call resumes with its operand stack intact.
#[test]
fn inner_trap_isolated_outer_resumes() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    // Host: call export "b" (which traps), swallow the error, return Ok.
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller, _args, _rets| {
            let b = export_func(&mut caller, "b");
            let _ = b.call(&mut caller, &[], &mut []); // inner trap, deliberately swallowed
            Ok(())
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (func $b (export "b") unreachable)
                (func (export "a") (result i32)
                  (i32.const 100)        ;; outer operand parked across the host call
                  (call $reenter)
                  (i32.const 42)
                  (i32.add)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();
    let a = inst.get_func(&mut store, "a").unwrap();
    let mut out = [Val::I32(0)];
    a.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(
        out[0].unwrap_i32(),
        142,
        "outer operand 100 survived the re-entry trap"
    );
}

/// An interpreter-owned ABI failure in a re-entered call cannot be swallowed by the host callback.
/// It terminates the outer execution, cleans up the shared stacks, and leaves the store reusable.
#[test]
fn inner_internal_failure_terminates_outer_and_cleans_up() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let bad = Func::new(
        &mut store,
        FuncType::new(&engine, [], [ValType::I32]),
        |_caller, _args, results| {
            results[0] = Val::I64(1);
            Ok(())
        },
    );
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |mut caller, _args, _results| {
            let mut ignored = [Val::I32(0)];
            let _ = bad.call(&mut caller, &[], &mut ignored);
            Ok(())
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (func (export "outer") call $reenter)
                (func (export "healthy") (result i32) i32.const 7))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();

    let outer = inst.get_func(&mut store, "outer").unwrap();
    let error = outer.call(&mut store, &[], &mut []).unwrap_err();
    assert!(error
        .to_string()
        .contains("internal result shape invariant violated"));

    let healthy = inst.get_func(&mut store, "healthy").unwrap();
    let mut result = [Val::I32(0)];
    healthy.call(&mut store, &[], &mut result).unwrap();
    assert_eq!(result[0].unwrap_i32(), 7);
}

/// Case 2 — an exception thrown in the inner call (no matching handler there) surfaces to the host's
/// `Func::call` as `ThrownException`, not to the outer call.
#[test]
fn inner_throw_surfaces_to_host_call() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], [ValType::I32]),
        |mut caller, _args, rets| {
            let b = export_func(&mut caller, "b");
            let err = b.call(&mut caller, &[], &mut []).unwrap_err();
            rets[0] = Val::I32(i32::from(err.is::<ThrownException>()));
            Ok(())
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter (result i32)))
                (tag $t)
                (func $b (export "b") throw $t)
                (func (export "a") (result i32) (call $reenter)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();
    let a = inst.get_func(&mut store, "a").unwrap();
    let mut out = [Val::I32(0)];
    a.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(
        out[0].unwrap_i32(),
        1,
        "inner throw seen by the host as ThrownException"
    );
}

/// Case 3 — the outer call has a `try_table` for tag `$t` spanning its host call; the inner call
/// throws `$t`. The outer handler must NOT catch it (the delimiter bounds the handler search): the
/// throw stays contained to the inner `Func::call`.
#[test]
fn outer_handler_does_not_catch_inner_throw() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller, _args, _rets| {
            let b = export_func(&mut caller, "b");
            let _ = b.call(&mut caller, &[], &mut []); // inner throw, swallowed
            Ok(())
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (tag $t)
                (func $b (export "b") throw $t)
                (func (export "a") (result i32)
                  (block $h
                    (try_table (catch $t $h) (call $reenter))
                    (return (i32.const 1)))  ;; reached: host swallowed, no exception in $a
                  (i32.const 999)))"#, // 999 is the catch landing — must NOT be reached
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();
    let a = inst.get_func(&mut store, "a").unwrap();
    let mut out = [Val::I32(0)];
    a.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(
        out[0].unwrap_i32(),
        1,
        "outer try_table wrongly caught the inner throw"
    );
}

/// Case 4 — a host function's OWN throw (`Store::throw`) is still catchable by the outer guest's
/// `try_table` (pins `raise_host_exception` unwinding to the outer call's depth, not an inner one).
#[test]
fn host_throw_caught_by_outer_handler() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let tt = TagType::new(FuncType::new(&engine, [], []));
    let tag = Tag::new(&mut store, &tt).unwrap();
    let pre = submilli_wasm::ExnRefPre::new(
        &mut store,
        submilli_wasm::ExnType::from_tag_type(&tt).unwrap(),
    );
    let thrower = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |mut caller, _args, _rets| {
            let exn = submilli_wasm::ExnRef::new(&mut caller, &pre, &tag, &[])?;
            caller.as_context_mut().throw(exn).map_err(Into::into)
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "" "t" (tag $t))
                (import "" "h" (func $h))
                (func (export "a") (result i32)
                  (block $c (try_table (catch $t $c) (call $h)) (return (i32.const 0)))
                  (i32.const 1)))"#,
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
    let a = inst.get_func(&mut store, "a").unwrap();
    let mut out = [Val::I32(0)];
    a.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(
        out[0].unwrap_i32(),
        1,
        "host throw caught by the outer try_table"
    );
}

/// Measures the maximum recursion depth of `sink` (which traps on stack overflow) reached by the
/// given `export`, read back from the module's exported `max` global. A fresh instance per call so
/// the counters start at zero.
fn max_depth(engine: &Engine, export: &str, arg: &[Val]) -> i32 {
    let mut store = Store::new(engine, ());
    // Host: call "sink" and swallow the (expected) stack-overflow error.
    let reenter = Func::new(
        &mut store,
        FuncType::new(engine, [], []),
        |mut caller, _args, _rets| {
            let sink = export_func(&mut caller, "sink");
            let _ = sink.call(&mut caller, &[], &mut []);
            Ok(())
        },
    );
    let module = Module::new(
        engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (global $cur (mut i32) (i32.const 0))
                (global $max (export "max") (mut i32) (i32.const 0))
                (func $sink (export "sink")
                  (global.set $cur (i32.add (global.get $cur) (i32.const 1)))
                  (if (i32.gt_s (global.get $cur) (global.get $max))
                    (then (global.set $max (global.get $cur))))
                  (call $sink))
                ;; recurse $n deep in pure wasm, then re-enter the host (which calls $sink)
                (func $outer (export "outer") (param $n i32)
                  (if (local.get $n)
                    (then (call $outer (i32.sub (local.get $n) (i32.const 1))))
                    (else (call $reenter)))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();
    let f = inst.get_func(&mut store, export).unwrap();
    let _ = f.call(&mut store, arg, &mut []); // may trap (top-level sink) or return (via outer)
    let g = inst.get_global(&mut store, "max").unwrap();
    g.get(&mut store).unwrap_i32()
}

/// Case 5 — `max_wasm_stack` is enforced ACROSS the host boundary. The inner `sink` recursion
/// reaches a strictly smaller depth when entered through a deep outer call than when called at top
/// level, because the parked outer frames count against the same budget.
#[test]
fn max_wasm_stack_spans_host_boundary() {
    let engine = Engine::new(Config::new().max_wasm_stack(64 * 1024)).unwrap();
    let top = max_depth(&engine, "sink", &[]);
    let via_reentry = max_depth(&engine, "outer", &[Val::I32(80)]);
    assert!(top > 0 && via_reentry > 0, "both runs must recurse");
    assert!(
        via_reentry < top,
        "re-entry depth {via_reentry} must be < top-level depth {top} (outer frames share the budget)"
    );
}

/// Case 6 — a backtrace captured inside a host call reached via re-entry spans BOTH guest segments
/// (the inner callee and the outer caller), proving capture walks the unified frame stack rather
/// than a single host window.
#[test]
fn backtrace_spans_reentry_chain() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, Vec::<String>::new());
    // Inner host: capture here; record the wasm frame names.
    let probe = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller: Caller<'_, Vec<String>>, _args, _rets| {
            let names: Vec<String> = WasmBacktrace::capture(&caller)
                .frames()
                .iter()
                .map(|f| f.func_name().unwrap_or("?").to_string())
                .collect();
            *caller.data_mut() = names;
            Ok(())
        },
    );
    // Outer host: re-enter wasm export "inner".
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller: Caller<'_, Vec<String>>, _args, _rets| {
            let inner = export_func(&mut caller, "inner");
            inner.call(&mut caller, &[], &mut [])
        },
    );
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (import "h" "probe" (func $probe))
                (func $inner (export "inner") call $probe)
                (func $a (export "a") call $reenter))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(
        &mut store,
        &module,
        &[Extern::Func(reenter), Extern::Func(probe)],
    )
    .unwrap();
    let a = inst.get_func(&mut store, "a").unwrap();
    a.call(&mut store, &[], &mut []).unwrap();
    let names = store.data().clone();
    assert!(
        names.iter().any(|n| n == "inner") && names.iter().any(|n| n == "a"),
        "capture must span the re-entry boundary (saw {names:?})"
    );
}

/// A callee's frame starts inside its caller's operand area, so its slots can end below the
/// caller's: a host re-entry from such a small callee must not cut the buffer under the caller's
/// live slots (`run` keeps computing after `$g` returns).
#[test]
fn reentry_from_a_small_callee_keeps_the_callers_slots() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let reenter = Func::wrap(&mut store, |mut caller: Caller<'_, ()>| {
        let nop = export_func(&mut caller, "nop");
        nop.call(&mut caller, &[], &mut [])
    });
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "reenter" (func $reenter))
                (func $nop (export "nop"))
                (func $g (call $reenter))
                (func (export "run") (result i32)
                    (call $g)
                    (i32.add (i32.const 40) (i32.add (i32.const 1) (i32.const 1)))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(reenter)]).unwrap();
    let run = inst.get_typed_func::<(), i32>(&mut store, "run").unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

/// A host reached by a tail call takes the popped callee's slots as its args; once popped they
/// must leave the caller's live range, or a re-entry (which trims the buffer to the live top)
/// followed by a collection finds that range cut short. Both shapes: tail-called from a callee,
/// and from the call's bottom frame (where the boundary itself is the top).
#[test]
fn tail_called_host_reentry_survives_collection() {
    let consts = (0..16)
        .map(|i| format!("(i32.const {i})"))
        .collect::<Vec<_>>()
        .join(" ");
    for run_body in ["(call $g)", &format!("(return_call $h {consts})")] {
        let engine = Engine::default();
        let wat = format!(
            r#"(module
                (import "h" "tail" (func $h (param{params})))
                (type $s (struct (field i32) (field i32)))
                (func $g (return_call $h {consts}))
                (func (export "run") {run_body})
                (func (export "churn") (param $n i32)
                    (loop $l
                        (drop (struct.new $s (i32.const 1) (i32.const 2)))
                        (br_if $l (local.tee $n (i32.sub (local.get $n) (i32.const 1)))))))"#,
            params = " i32".repeat(16),
        );
        let module = Module::new(&engine, wat::parse_str(&wat).unwrap()).unwrap();
        let mut store = Store::new(&engine, ());
        let ty = FuncType::new(&engine, vec![ValType::I32; 16], []);
        let host = Func::new(&mut store, ty, |mut caller, _, _| {
            let churn = export_func(&mut caller, "churn");
            churn.call(&mut caller, &[Val::I32(200_000)], &mut [])
        });
        let inst = Instance::new(&mut store, &module, &[Extern::Func(host)]).unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();
        run.call(&mut store, &[], &mut [])
            .unwrap_or_else(|e| panic!("{run_body}: {e:#}"));
    }
}

/// `run` holds a struct and keeps computing after `$g`, whose `$arm` call trips the epoch deadline.
const SMALL_CALLEE_EPOCH_WAT: &str = r#"(module
                (type $s (struct (field i32) (field i32)))
                (import "h" "arm" (func $arm))
                (global $gl (mut i32) (i32.const 0))
                (func $one (result i32) (i32.const 1))
                (func (export "nop"))
                (func $g (call $arm) (global.set $gl (i32.const 7)))
                (func (export "run") (result i32)
                    (local $r (ref null $s))
                    (local.set $r (struct.new $s (i32.const 20) (i32.const 21)))
                    (i32.add (call $one)
                        (i32.add (struct.get $s 0 (local.get $r))
                            (i32.add (block (result i32) (call $g) (i32.const 0))
                                (struct.get $s 1 (local.get $r)))))))"#;

/// The epoch-callback flavour of `reentry_from_a_small_callee_keeps_the_callers_slots`: the
/// deadline fires inside a small callee (`$g`), the callback re-enters wasm and collects, and the
/// caller keeps computing on its own slots (including a struct it holds) afterwards.
#[test]
fn epoch_reentry_from_a_small_callee_keeps_the_callers_slots() {
    use std::sync::{Arc, Mutex};
    use submilli_wasm::{
        Collector, FieldType, Mutability, StorageType, StructRef, StructRefPre, StructType,
        UpdateDeadline,
    };
    let mut cfg = Config::new();
    cfg.collector(Collector::Auto)
        .gc_heap_reservation(64 << 10)
        .epoch_interruption(true);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, ());
    let nop_slot: Arc<Mutex<Option<Func>>> = Arc::default();
    let nop_for_cb = nop_slot.clone();
    let struct_engine = engine.clone();
    store.epoch_deadline_callback(move |mut ctx| {
        let nop = nop_for_cb.lock().unwrap().unwrap();
        nop.call(&mut ctx, &[], &mut [])?;
        let field = FieldType::new(Mutability::Var, StorageType::ValType(ValType::I64));
        let pre = StructRefPre::new(&mut ctx, StructType::new(&struct_engine, [field])?);
        for _ in 0..20_000 {
            StructRef::new(&mut ctx, &pre, &[Val::I64(0)])?;
        }
        Ok(UpdateDeadline::Continue(u64::MAX))
    });
    store.set_epoch_deadline(1);
    let arm_engine = engine.clone();
    let arm = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |_, _, _| {
            arm_engine.increment_epoch();
            Ok(())
        },
    );
    let module = Module::new(&engine, wat::parse_str(SMALL_CALLEE_EPOCH_WAT).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(arm)]).unwrap();
    *nop_slot.lock().unwrap() = inst.get_func(&mut store, "nop");
    let run = inst.get_typed_func::<(), i32>(&mut store, "run").unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

/// The async flavour of `tail_called_host_reentry_survives_collection`: an async host reached
/// by a tail call (from a callee, and from the bottom frame) re-enters wasm, which collects.
#[cfg(feature = "async")]
#[test]
fn async_tail_called_host_reentry_survives_collection() {
    use pollster::block_on;
    use submilli_wasm::Collector;
    let consts = (0..16)
        .map(|i| format!("(i32.const {i})"))
        .collect::<Vec<_>>()
        .join(" ");
    for run_body in ["(call $g)", &format!("(return_call $h {consts})")] {
        let mut cfg = Config::new();
        cfg.async_support(true)
            .collector(Collector::Auto)
            .gc_heap_reservation(64 << 10);
        let engine = Engine::new(&cfg).unwrap();
        let mut store = Store::new(&engine, ());
        let ty = FuncType::new(&engine, vec![ValType::I32; 16], []);
        let host = Func::new_async(&mut store, ty, |mut caller: Caller<'_, ()>, _, _| {
            Box::new(async move {
                let churn = export_func(&mut caller, "churn");
                churn
                    .call_async(&mut caller, &[Val::I32(100_000)], &mut [])
                    .await
            })
        });
        let wat = format!(
            r#"(module
                (import "h" "tail" (func $h (param{params})))
                (type $s (struct (field i32) (field i32)))
                (func $g (return_call $h {consts}))
                (func (export "run") {run_body})
                (func (export "churn") (param $n i32)
                    (loop $l
                        (drop (struct.new $s (i32.const 1) (i32.const 2)))
                        (br_if $l (local.tee $n (i32.sub (local.get $n) (i32.const 1)))))))"#,
            params = " i32".repeat(16),
        );
        let module = Module::new(&engine, wat::parse_str(&wat).unwrap()).unwrap();
        let inst = block_on(Instance::new_async(
            &mut store,
            &module,
            &[Extern::Func(host)],
        ))
        .unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();
        block_on(run.call_async(&mut store, &[], &mut []))
            .unwrap_or_else(|e| panic!("{run_body}: {e:#}"));
    }
}
