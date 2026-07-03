//! Wasm stack backtraces (wasmtime-compatible): [`WasmBacktrace`] and its [`FrameInfo`] /
//! [`FrameSymbol`] frames. Source-level symbolication is resolved lazily from the module's DWARF
//! (#29a); throw-time capture that fills a backtrace lands in #29d.

use std::sync::{Arc, OnceLock};

use crate::module::Module;
use crate::store::AsContext;

/// A captured wasm stack backtrace, mirroring `wasmtime::WasmBacktrace`.
#[derive(Clone, Debug)]
pub struct WasmBacktrace {
    frames: Vec<FrameInfo>,
    /// Indices into `frames` where a host↔wasm re-entry boundary sits (a host fn re-entered wasm
    /// via `Func::call`): a boundary precedes `frames[i]`. Presentational — `frames()` stays pure
    /// wasm (wasmtime-compatible); `Display` renders a `<host>` line at each.
    host_boundaries: Vec<usize>,
}

impl std::fmt::Display for WasmBacktrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "wasm backtrace:")?;
        for (i, frame) in self.frames.iter().enumerate() {
            if self.host_boundaries.contains(&i) {
                writeln!(f, "  <host>")?;
            }
            let name = frame.func_name().unwrap_or("<unknown>");
            writeln!(f, "  {i}: func[{}] {name}", frame.func_index())?;
        }
        if self.host_boundaries.contains(&self.frames.len()) {
            writeln!(f, "  <host>")?;
        }
        Ok(())
    }
}

// A backtrace is attached as the *source* of a trap/exception error (the trap is the context), so
// the error's `Display` stays the trap message while `downcast_ref::<WasmBacktrace>()` recovers it.
impl std::error::Error for WasmBacktrace {}

impl WasmBacktrace {
    /// Captures the wasm backtrace of the current execution if `Config::wasm_backtrace` is on,
    /// else an empty trace. Meaningful when called from within a host function (it walks the wasm
    /// frames that called it); empty outside an active call.
    pub fn capture(store: impl AsContext) -> WasmBacktrace {
        let ctx = store.as_context();
        if !ctx.engine().wasm_backtrace_enabled() {
            return WasmBacktrace::from_frames(Vec::new());
        }
        crate::exec::trace::from_parked(ctx.inner())
    }

    /// Like [`capture`](Self::capture) but ignores `Config::wasm_backtrace`.
    pub fn force_capture(store: impl AsContext) -> WasmBacktrace {
        crate::exec::trace::from_parked(store.as_context().inner())
    }

    /// The captured frames, most-recent first.
    pub fn frames(&self) -> &[FrameInfo] {
        &self.frames
    }

    /// Builds a backtrace from already-collected frames (used by capture, #29d).
    pub(crate) fn from_frames(frames: Vec<FrameInfo>) -> WasmBacktrace {
        WasmBacktrace {
            frames,
            host_boundaries: Vec::new(),
        }
    }

    /// Builds a backtrace plus the host-re-entry boundary positions (for `Display` markers).
    pub(crate) fn from_frames_with_boundaries(
        frames: Vec<FrameInfo>,
        host_boundaries: Vec<usize>,
    ) -> WasmBacktrace {
        WasmBacktrace {
            frames,
            host_boundaries,
        }
    }
}

/// Information about a single wasm stack frame, mirroring `wasmtime::FrameInfo`.
///
/// Symbolication (file/line/column) is resolved lazily from the module's DWARF on first
/// [`symbols`](Self::symbols), keeping the trap/capture path off the DWARF cost.
#[derive(Debug)]
pub struct FrameInfo {
    module: Module,
    func_index: u32,
    /// Module-relative wasm byte offset of the trapping instruction (`CompiledFunc::offsets[ip]`);
    /// `None` when the offset table wasn't retained.
    code_offset: Option<u32>,
    symbols: OnceLock<Box<[FrameSymbol]>>,
}

impl Clone for FrameInfo {
    /// Clones the frame's identity; the lazily-symbolicated cache is not carried (re-resolved on
    /// demand). `OnceLock` isn't `Clone`, and the result is identical either way.
    fn clone(&self) -> Self {
        FrameInfo {
            module: self.module.clone(),
            func_index: self.func_index,
            code_offset: self.code_offset,
            symbols: OnceLock::new(),
        }
    }
}

impl FrameInfo {
    pub(crate) fn new(module: Module, func_index: u32, code_offset: Option<u32>) -> FrameInfo {
        FrameInfo {
            module,
            func_index,
            code_offset,
            symbols: OnceLock::new(),
        }
    }

    /// The index of the function in its defining module.
    pub fn func_index(&self) -> u32 {
        self.func_index
    }

    /// The module this frame executes in.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The function's name from the `name` custom section, if present.
    pub fn func_name(&self) -> Option<&str> {
        self.module.inner().debug.func_name(self.func_index)
    }

    /// The trapping instruction's offset relative to the start of the module's code section.
    pub fn module_offset(&self) -> Option<usize> {
        self.code_offset.map(|o| o as usize)
    }

    /// The trapping instruction's offset relative to the start of this function's body.
    pub fn func_offset(&self) -> Option<usize> {
        let off = self.code_offset?;
        off.checked_sub(self.func_start()?).map(|o| o as usize)
    }

    /// Source-level symbols for this frame, resolved lazily from DWARF. Empty when neither a name
    /// nor a source location is available (e.g. no debug info).
    pub fn symbols(&self) -> &[FrameSymbol] {
        self.symbols.get_or_init(|| self.build_symbols())
    }

    /// The module-relative offset of this function's first instruction, from the offset table.
    fn func_start(&self) -> Option<u32> {
        // Imported functions have no compiled body (`code()` expects a defined index).
        self.func_index
            .checked_sub(self.module.inner().num_imported_funcs)?;
        self.module
            .code(self.func_index)
            .offsets()?
            .first()
            .copied()
    }

    fn build_symbols(&self) -> Box<[FrameSymbol]> {
        let debug = &self.module.inner().debug;
        // Prefer the `name` section (func-index-keyed, exact); fall back to the DWARF subprogram
        // name at this offset, so modules shipping only DWARF (no `name` section) still get names —
        // matching wasmtime's `wasm_backtrace_details`.
        let name = debug
            .func_name(self.func_index)
            .map(Arc::from)
            .or_else(|| self.code_offset.and_then(|o| debug.dwarf_func_name(o)));
        let loc = self.code_offset.and_then(|o| debug.lookup(o));
        if name.is_none() && loc.is_none() {
            return Box::new([]);
        }
        let nonzero = |v: u32| (v != 0).then_some(v);
        Box::new([FrameSymbol {
            name,
            file: loc.as_ref().map(|l| Arc::clone(&l.file)),
            line: loc.as_ref().and_then(|l| nonzero(l.line)),
            column: loc.as_ref().and_then(|l| nonzero(l.column)),
        }])
    }
}

/// A source-level symbol for a [`FrameInfo`], mirroring `wasmtime::FrameSymbol`.
#[derive(Clone, Debug)]
pub struct FrameSymbol {
    name: Option<Arc<str>>,
    file: Option<Arc<str>>,
    line: Option<u32>,
    column: Option<u32>,
}

impl FrameSymbol {
    /// The function name at this symbol.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The source file path.
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// The 1-based source line.
    pub fn line(&self) -> Option<u32> {
        self.line
    }

    /// The 1-based source column.
    pub fn column(&self) -> Option<u32> {
        self.column
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{FrameInfo, FrameSymbol, WasmBacktrace};
    use crate::{Config, Engine, Module, Trap};

    /// Load-bearing (#29d): with the backtrace as the error's *source* and the `Trap` as *context*,
    /// both `downcast_ref`s resolve AND `Display` stays the trap message (not the backtrace) — the
    /// spec harness and embedders read the trap text.
    #[test]
    fn backtrace_source_keeps_trap_display() {
        let err = crate::Error::new(WasmBacktrace::from_frames(Vec::new()))
            .context(Trap::UnreachableCodeReached);
        assert!(matches!(
            err.downcast_ref::<Trap>(),
            Some(Trap::UnreachableCodeReached)
        ));
        assert!(err.downcast_ref::<WasmBacktrace>().is_some());
        assert_eq!(err.to_string(), Trap::UnreachableCodeReached.to_string());
    }

    /// Real DWARF + `name` section module (`boom`, defined func 0, body on line 13). See
    /// `src/module/debug/testdata/fixture.rs`.
    const FIXTURE: &[u8] = include_bytes!("module/debug/testdata/fixture.wasm");

    #[test]
    fn frame_symbolicates_against_dwarf() {
        // The default engine no longer retains DWARF (#29c); opt in for file/line.
        let engine = Engine::new(Config::new().debug_info(true)).unwrap();
        let module = Module::new(&engine, FIXTURE).unwrap();
        let offset = module.code(0).offsets().unwrap()[0];

        let frame = FrameInfo::new(module.clone(), 0, Some(offset));
        assert_eq!(frame.func_index(), 0);
        assert_eq!(frame.func_name(), Some("boom"));
        assert_eq!(frame.module_offset(), Some(offset as usize));
        assert_eq!(frame.func_offset(), Some(0)); // first op of the function

        let symbols = frame.symbols();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name(), Some("boom"));
        assert!(symbols[0].file().unwrap().ends_with("fixture.rs"));
        assert_eq!(symbols[0].line(), Some(13));

        let _clone: FrameSymbol = symbols[0].clone(); // FrameSymbol: Clone
    }

    /// #29e: DWARF is parsed only when `symbols()` is inspected, never at frame construction
    /// (capture). The frame's `Module` clone shares the original's `OnceLock`, so we observe it
    /// through the original handle.
    #[test]
    fn symbolication_is_lazy() {
        let engine = Engine::new(Config::new().debug_info(true)).unwrap();
        let module = Module::new(&engine, FIXTURE).unwrap();
        let offset = module.code(0).offsets().unwrap()[0];

        let frame = FrameInfo::new(module.clone(), 0, Some(offset));
        assert!(
            !module.inner().debug.index_built(),
            "constructing a frame must not parse DWARF"
        );
        let _ = frame.symbols();
        assert!(
            module.inner().debug.index_built(),
            "inspecting symbols() parses DWARF"
        );
    }

    #[test]
    fn frame_without_debug_info_is_empty() {
        let engine = Engine::default();
        let module = Module::new(&engine, wat::parse_str("(module (func))").unwrap()).unwrap();

        let frame = FrameInfo::new(module, 0, None);
        assert!(frame.symbols().is_empty());
        assert_eq!(frame.func_name(), None);
        assert_eq!(frame.module_offset(), None);
        assert_eq!(frame.func_offset(), None);
    }
}
