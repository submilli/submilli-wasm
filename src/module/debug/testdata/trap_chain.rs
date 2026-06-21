// Multi-frame DWARF fixture for the #29f Phase-7 gate. A 3-deep call chain ending in a raw
// out-of-bounds load (a clean wasm `MemoryOutOfBounds` trap, no Rust panic machinery). Rebuild:
//   cd src/module/debug/testdata
//   rustc --target wasm32-unknown-unknown -C debuginfo=2 -C opt-level=1 -C panic=abort \
//         --crate-type cdylib --remap-path-prefix "$(pwd)=." -o trap_chain.wasm trap_chain.rs
//   wasm-tools strip -d producers -d target_features -d .debug_ranges \
//         trap_chain.wasm -o trap_chain.wasm
// Each function is on its own line so the three backtrace frames map to distinct source lines.
#![no_std]
#[panic_handler]
fn ph(_: &core::panic::PanicInfo) -> ! { loop {} }
#[no_mangle] #[inline(never)] pub extern "C" fn level_c() -> i32 { unsafe { *(0xffff_fff0 as *const i32) } }
#[no_mangle] #[inline(never)] pub extern "C" fn level_b() -> i32 { level_c() }
#[no_mangle] #[inline(never)] pub extern "C" fn trap_chain() -> i32 { level_b() }
