//! Flags an unoptimized build (`opt-level = 0`) as `cfg(submilli_unoptimized)`, so the
//! interpreter keeps its giant per-op `step` out of line there (see `exec::step`). Keyed on the
//! optimization level itself: profiles set `debug-assertions` independently of it.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(submilli_unoptimized)");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("OPT_LEVEL").is_ok_and(|level| level == "0") {
        println!("cargo::rustc-cfg=submilli_unoptimized");
    }
}
