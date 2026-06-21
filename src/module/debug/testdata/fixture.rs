// Source for `fixture.wasm`, the DWARF test fixture for #29a. Rebuild with:
//   cd src/module/debug/testdata
//   rustc --target wasm32-unknown-unknown -C debuginfo=2 -C opt-level=1 \
//         -C overflow-checks=off -C panic=abort --crate-type cdylib \
//         --remap-path-prefix "$(pwd)=." -o fixture.wasm fixture.rs
//   wasm-tools strip -d producers -d target_features -d .debug_ranges \
//         fixture.wasm -o fixture.wasm
// `boom`'s body is line 9; the exported function carries .debug_line rows.
#![no_std]
#[panic_handler]
fn ph(_: &core::panic::PanicInfo) -> ! { loop {} }
#[no_mangle]
pub extern "C" fn boom(x: i32) -> i32 {
    let y = x.wrapping_add(1);
    y.wrapping_mul(2)
}
