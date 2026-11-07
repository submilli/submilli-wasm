//! Step 2b operand providers: `local.get` emits nothing and consumers read the local's slot, so
//! every place a local can change while an earlier read of it is still on the stack — or where
//! control flow merges — must still see the right value. Each case runs over a grid of inputs
//! against a Rust oracle.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Val};

/// `(prelude, body, oracle)`: `body` is `f (param i32 i32) (result i32)`.
type Case = (&'static str, &'static str, fn(i32, i32) -> i32);

const CASES: &[Case] = &[
    // A local overwritten while its earlier `local.get` is still on the stack.
    ("", "local.get 0 (local.set 0 (i32.const 5)) local.get 0 i32.add", |a, _| a + 5),
    ("", "local.get 0 (local.tee 0 (i32.const 7)) i32.add", |a, _| a + 7),
    ("", "local.get 0 local.get 0 local.get 0 (local.set 0 (i32.const 1)) i32.add i32.add local.get 0 i32.add",
        |a, _| 3 * a + 1),
    // Swap through the stack, and a no-op self-assignment.
    ("", "local.get 0 local.get 1 local.set 0 local.set 1 local.get 0 local.get 1 i32.sub", |a, b| b - a),
    ("", "local.get 0 local.set 0 local.get 0", |a, _| a),
    ("", "local.get 0 (local.tee 1 (local.get 0)) (local.set 0 (i32.const 9)) i32.add local.get 1 i32.add",
        |a, _| 3 * a),
    // `local.set`/`local.tee` fusion: the producing op writes the local directly.
    ("", "(local.set 0 (i32.add (local.get 0) (i32.const 1))) local.get 0", |a, _| a + 1),
    ("", "(local.tee 1 (i32.mul (local.get 0) (i32.const 3))) local.get 1 i32.add", |a, _| 6 * a),
    ("", "(local.tee 1 (i32.add (local.get 0) (i32.const 1))) (local.set 1 (i32.const 0)) local.get 1 i32.add",
        |a, _| a + 1),
    ("(memory 1)", "(i32.store (i32.const 16) (local.get 1)) (local.set 0 (i32.load (i32.const 16))) local.get 0",
        |_, b| b),
    ("", "(block (local.set 1 (i32.lt_s (local.get 0) (i32.const 5))) (br_if 0 (local.get 1)) \
            (return (i32.const 100))) i32.const 200",
        |a, _| if a < 5 { 200 } else { 100 }),
    // Merges: an outer read must see the old value on every path into the label.
    ("", "local.get 0 (block (br_if 0 (local.get 1)) (local.set 0 (i32.const 100))) local.get 0 i32.add",
        |a, b| if b != 0 { 2 * a } else { a + 100 }),
    ("", "local.get 0 (if (local.get 1) (then (local.set 0 (i32.const 3)))) local.get 0 i32.mul",
        |a, b| if b != 0 { 3 * a } else { a * a }),
    ("", "local.get 0 (loop (local.set 0 (i32.add (local.get 0) (i32.const 1))) \
            (br_if 0 (i32.lt_u (local.get 0) (i32.const 10)))) local.get 0 i32.add",
        |a, _| a + if ((a + 1) as u32) < 10 { 10 } else { a + 1 }),
    ("", "(if (result i32) (local.get 1) (then local.get 0) (else local.get 1))",
        |a, b| if b != 0 { a } else { b }),
    // Branches carrying values from locals (and a condition / index read from a local).
    ("", "(block (result i32) local.get 0 local.get 1 br_if 0 drop i32.const 9)",
        |a, b| if b != 0 { a } else { 9 }),
    ("", "(block (result i32) (block (result i32) local.get 0 local.get 1 br_table 0 1 1) \
            i32.const 1000 i32.add)",
        |a, b| if b == 0 { a + 1000 } else { a }),
    ("", "(block local.get 0 local.get 1 br_if 1 drop) i32.const 42", |a, b| if b != 0 { a } else { 42 }),
    ("", "local.get 0 local.get 1 local.get 1 select", |a, b| if b != 0 { a } else { b }),
    // A pending compare must not fuse into a `br_if` testing something pushed after it.
    ("", "(block (drop (block (result i32) (i32.lt_s (local.get 0) (local.get 1)) local.get 1 br_if 1)) \
            (return (i32.const 7))) i32.const 8",
        |_, b| if b != 0 { 8 } else { 7 }),
    // Calls fed straight from locals.
    ("(type $t (func (param i32 i32) (result i32))) (table 1 funcref) (elem (i32.const 0) $sub) \
      (func $sub (type $t) local.get 0 local.get 1 i32.sub)",
        "local.get 1 local.get 0 call $sub local.get 0 local.get 1 i32.const 0 call_indirect (type $t) \
            i32.const 1 i32.add i32.mul",
        |a, b| (b - a) * (a - b + 1)),
    // Memory, globals and exceptions.
    ("(memory 1)", "(i32.store (i32.and (local.get 0) (i32.const 0xff0)) (local.get 1)) \
            (i32.load (i32.and (local.get 0) (i32.const 0xff0)))", |_, b| b),
    ("(global $g (mut i32) (i32.const 0))",
        "(global.set $g (local.get 0)) (local.set 0 (i32.const 0)) global.get $g local.get 0 i32.add", |a, _| a),
    ("(tag $e (param i32))",
        "local.get 0 (block $h (result i32) (try_table (catch $e $h) \
            (local.set 0 (i32.const 5)) (throw $e (local.get 1))) unreachable) i32.add local.get 0 i32.add",
        |a, b| a + b + 5),
];

const INPUTS: &[(i32, i32)] = &[
    (0, 0),
    (1, 2),
    (5, 0),
    (7, 7),
    (-1, 3),
    (20, 1),
    (3, -5),
    (8, 0),
];

#[test]
fn providers_match_the_oracle() {
    let engine = Engine::default();
    for (i, (prelude, body, oracle)) in CASES.iter().enumerate() {
        let wat =
            format!("(module {prelude} (func (export \"f\") (param i32 i32) (result i32) {body}))");
        let module = Module::new(&engine, wat::parse_str(&wat).unwrap())
            .unwrap_or_else(|e| panic!("case {i} failed to compile: {e}"));
        for &(a, b) in INPUTS {
            let mut store = Store::new(&engine, ());
            let inst = Instance::new(&mut store, &module, &[]).unwrap();
            let f = inst.get_func(&mut store, "f").unwrap();
            let mut out = [Val::I32(0)];
            f.call(&mut store, &[Val::I32(a), Val::I32(b)], &mut out)
                .unwrap_or_else(|e| panic!("case {i} ({a}, {b}) trapped: {e}"));
            let want = oracle(a, b);
            assert_eq!(out[0].unwrap_i32(), want, "case {i} ({a}, {b}): {body}");
        }
    }
}
