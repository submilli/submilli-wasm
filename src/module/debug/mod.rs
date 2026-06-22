//! Lazy DWARF / `name`-section index for symbolicated backtraces (#29a).
//!
//! Retains the module's `.debug_*` and `name` custom sections at parse time (gated by the caller's
//! `retain_debug`), then builds a `code-offset → (file, line, column)` line table on first
//! [`lookup`](DebugSections::lookup) — off the compile/startup path. Function names come from the
//! `name` section (parsed eagerly; cheap). All DWARF parsing is panic-free on adversarial input.
//!
//! The retained inputs (raw `.debug_*` bytes, func-name map, code base) are serialized with the
//! compiled artifact, so backtraces survive `Module::serialize`/`deserialize` (matching wasmtime);
//! only the lazily-rebuilt `gimli` line table is `#[serde(skip)]`. The public
//! `FrameInfo`/`FrameSymbol` surface that consumes this lands in #29b; throw-time capture that
//! supplies the offsets is #29d.

mod func;
mod line;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use wasmparser::{BinaryReader, Name, NameSectionReader};

use self::func::FuncTable;
use self::line::LineTable;

/// A resolved source location for a wasm code offset.
#[derive(Debug, Clone)]
pub(crate) struct SourceLoc {
    pub file: Arc<str>,
    pub line: u32,
    pub column: u32,
}

/// Retained debug inputs plus the lazily-built line index. `Default` = empty (no debug info).
/// The inputs serialize with the artifact; the line table is a rebuilt-on-demand cache.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct DebugSections {
    /// `.debug_*` section name → raw bytes, retained for lazy `gimli` parsing.
    dwarf: HashMap<String, Box<[u8]>>,
    /// Function index → name, from the `name` custom section.
    func_names: HashMap<u32, Box<str>>,
    /// Module-relative byte offset of the code section body; DWARF code addresses are measured
    /// from here (see [`lookup`](Self::lookup)).
    code_base: u32,
    /// Lazily-built line table; `None` once built if absent or unparsable. Rebuilt from `dwarf`
    /// after deserialize, so it is not part of the artifact.
    #[serde(skip)]
    index: OnceLock<Option<LineTable>>,
    /// Lazily-built subprogram-name table (DWARF); rebuilt from `dwarf` like `index`.
    #[serde(skip)]
    funcs: OnceLock<Option<FuncTable>>,
}

impl DebugSections {
    /// Records the code section's base offset, so absolute operator offsets can be made
    /// code-section-relative to match DWARF addresses.
    pub(crate) fn set_code_base(&mut self, base: u32) {
        self.code_base = base;
    }

    /// Retains a `.debug_*` custom section's raw bytes for lazy parsing.
    pub(crate) fn add_dwarf_section(&mut self, name: &str, data: &[u8]) {
        self.dwarf
            .insert(name.to_string(), data.to_vec().into_boxed_slice());
    }

    /// Eagerly parses the `name` custom section's function-name subsection. Malformed entries are
    /// skipped (never fatal) — names are a best-effort symbolication aid.
    pub(crate) fn add_name_section(&mut self, data: &[u8], offset: usize) {
        let reader = NameSectionReader::new(BinaryReader::new(data, offset));
        for sub in reader {
            let Ok(Name::Function(map)) = sub else {
                continue;
            };
            for naming in map {
                let Ok(naming) = naming else { break };
                self.func_names.insert(naming.index, naming.name.into());
            }
        }
    }

    /// The name of the function at `func_index`, from the `name` section, if present.
    pub(crate) fn func_name(&self, func_index: u32) -> Option<&str> {
        self.func_names.get(&func_index).map(|s| &**s)
    }

    /// The name of the subprogram covering an absolute (module-relative) `code_offset`, from DWARF.
    /// The fallback when there is no wasm `name` section (matches wasmtime's DWARF-backed frame
    /// names); builds the subprogram index on first call, like [`lookup`](Self::lookup).
    pub(crate) fn dwarf_func_name(&self, code_offset: u32) -> Option<Arc<str>> {
        let table = self.funcs.get_or_init(|| func::build(&self.dwarf));
        let addr = code_offset.checked_sub(self.code_base)?;
        table.as_ref()?.lookup(addr)
    }

    /// Resolves an absolute (module-relative) wasm code offset to a source location, building the
    /// DWARF line index on first call. Returns `None` when there's no debug info or no covering
    /// row. `code_offset` is what `CompiledFunc::offsets[ip]` records.
    pub(crate) fn lookup(&self, code_offset: u32) -> Option<SourceLoc> {
        let table = self.index.get_or_init(|| line::build(&self.dwarf));
        let addr = code_offset.checked_sub(self.code_base)?;
        table.as_ref()?.lookup(addr)
    }

    /// Whether the lazy DWARF line index has been built yet — for the laziness regression test.
    #[cfg(test)]
    pub(crate) fn index_built(&self) -> bool {
        self.index.get().is_some()
    }
}
