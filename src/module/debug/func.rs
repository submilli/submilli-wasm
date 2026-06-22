//! `.debug_info` subprogram-name index, via `gimli`: resolves a DWARF code address to the name of
//! the enclosing `DW_TAG_subprogram`. Built lazily like the line table ([`line`](super::line)).
//!
//! This is what symbolicates frame *names* when a module ships DWARF but no wasm `name` section
//! (the common case for DWARF-emitting toolchains; matches wasmtime's `wasm_backtrace_details`).
//! Treats the DWARF as untrusted: every step is fallible → skip/`None`, never panics.

use std::collections::HashMap;
use std::sync::Arc;

use gimli::{EndianSlice, LittleEndian};

type Slice<'a> = EndianSlice<'a, LittleEndian>;

/// Subprogram address ranges → name, distilled from `.debug_info`.
#[derive(Debug)]
pub(super) struct FuncTable {
    /// Sorted ascending by `start`; binary-searched on lookup.
    ranges: Vec<FuncRange>,
}

#[derive(Debug)]
struct FuncRange {
    start: u32,
    end: u32,
    name: Arc<str>,
}

impl FuncTable {
    /// The name of the subprogram whose `[start, end)` range covers `addr`, if any.
    pub(super) fn lookup(&self, addr: u32) -> Option<Arc<str>> {
        let idx = match self.ranges.binary_search_by(|r| r.start.cmp(&addr)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let r = self.ranges.get(idx)?;
        (addr < r.end).then(|| Arc::clone(&r.name))
    }
}

/// Parses the retained `.debug_*` sections into a sorted [`FuncTable`]. `None` when there are no
/// named subprograms with address ranges; never panics on malformed input.
pub(super) fn build(sections: &HashMap<String, Box<[u8]>>) -> Option<FuncTable> {
    let dwarf = gimli::Dwarf::load(|id| -> gimli::Result<Slice<'_>> {
        let bytes = sections.get(id.name()).map_or(&[][..], |b| &b[..]);
        Ok(EndianSlice::new(bytes, LittleEndian))
    })
    .ok()?;

    let mut ranges: Vec<FuncRange> = Vec::new();
    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let Ok(unit) = dwarf.unit(header) else { break };
        // A malformed unit just contributes no ranges; keep going with the rest.
        let _ = collect_unit(&dwarf, &unit, &mut ranges);
    }

    if ranges.is_empty() {
        return None;
    }
    ranges.sort_by_key(|r| r.start);
    Some(FuncTable { ranges })
}

fn collect_unit(
    dwarf: &gimli::Dwarf<Slice<'_>>,
    unit: &gimli::Unit<Slice<'_>>,
    ranges: &mut Vec<FuncRange>,
) -> gimli::Result<()> {
    let mut entries = unit.entries();
    while let Some(entry) = entries.next_dfs()? {
        if entry.tag() != gimli::DW_TAG_subprogram {
            continue;
        }
        let Some(name) = subprogram_name(dwarf, unit, entry) else {
            continue;
        };
        let mut die_ranges = dwarf.die_ranges(unit, entry)?;
        while let Some(range) = die_ranges.next()? {
            if range.begin > u64::from(u32::MAX) || range.end > u64::from(u32::MAX) {
                continue;
            }
            ranges.push(FuncRange {
                start: range.begin as u32,
                end: range.end as u32,
                name: Arc::clone(&name),
            });
        }
    }
    Ok(())
}

/// The `DW_AT_name` of a subprogram DIE as an owned `Arc<str>`, if present and valid UTF-8.
fn subprogram_name(
    dwarf: &gimli::Dwarf<Slice<'_>>,
    unit: &gimli::Unit<Slice<'_>>,
    entry: &gimli::DebuggingInformationEntry<Slice<'_>>,
) -> Option<Arc<str>> {
    let attr = entry.attr_value(gimli::DW_AT_name)?;
    dwarf
        .attr_string(unit, attr)
        .ok()
        .and_then(|r| std::str::from_utf8(r.slice()).ok().map(Arc::<str>::from))
}
