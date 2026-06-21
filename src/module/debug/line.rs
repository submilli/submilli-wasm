//! `.debug_line` → a compact, sorted `code-offset → (file, line, column)` table, via `gimli`.
//!
//! Parsing runs lazily (first lookup) and treats the DWARF as **untrusted**: every step uses
//! `gimli`'s fallible API and degrades to `None`/skips the row on any error — never panics.

use std::collections::HashMap;
use std::sync::Arc;

use gimli::{EndianSlice, LittleEndian};

use super::SourceLoc;

type Slice<'a> = EndianSlice<'a, LittleEndian>;

/// A compact, sorted code-offset → source-location table distilled from `.debug_line`.
#[derive(Debug)]
pub(super) struct LineTable {
    /// Sorted ascending by `addr`; binary-searched on lookup.
    rows: Vec<Row>,
    /// Interned file paths; `Row::file` indexes here.
    files: Vec<Arc<str>>,
}

#[derive(Debug)]
struct Row {
    addr: u32,
    file: u32,
    line: u32,
    column: u32,
}

impl LineTable {
    /// Resolves a DWARF code address to its enclosing line-table row (the last row at or before
    /// `addr`). Returns `None` before the first row or when the file can't be resolved.
    pub(super) fn lookup(&self, addr: u32) -> Option<SourceLoc> {
        let idx = match self.rows.binary_search_by(|r| r.addr.cmp(&addr)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let row = self.rows.get(idx)?;
        Some(SourceLoc {
            file: self.files.get(row.file as usize)?.clone(),
            line: row.line,
            column: row.column,
        })
    }
}

/// Parses the retained `.debug_*` sections into a sorted [`LineTable`]. Returns `None` when there
/// is no usable line information; never panics on malformed input.
pub(super) fn build(sections: &HashMap<String, Box<[u8]>>) -> Option<LineTable> {
    let dwarf = gimli::Dwarf::load(|id| -> gimli::Result<Slice<'_>> {
        let bytes = sections.get(id.name()).map_or(&[][..], |b| &b[..]);
        Ok(EndianSlice::new(bytes, LittleEndian))
    })
    .ok()?;

    let mut table = LineTable {
        rows: Vec::new(),
        files: Vec::new(),
    };
    let mut file_ids: HashMap<String, u32> = HashMap::new();
    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let Ok(unit) = dwarf.unit(header) else { break };
        // A malformed unit just contributes no rows; keep going with the rest.
        let _ = collect_unit(&dwarf, &unit, &mut table, &mut file_ids);
    }

    if table.rows.is_empty() {
        return None;
    }
    table.rows.sort_by_key(|r| r.addr);
    Some(table)
}

fn collect_unit(
    dwarf: &gimli::Dwarf<Slice<'_>>,
    unit: &gimli::Unit<Slice<'_>>,
    table: &mut LineTable,
    file_ids: &mut HashMap<String, u32>,
) -> gimli::Result<()> {
    let Some(program) = unit.line_program.clone() else {
        return Ok(());
    };
    let mut rows = program.rows();
    while let Some((header, row)) = rows.next_row()? {
        if row.end_sequence() {
            continue;
        }
        let addr = row.address();
        if addr > u64::from(u32::MAX) {
            continue;
        }
        let line = row.line().map_or(0, |l| l.get() as u32);
        let column = match row.column() {
            gimli::ColumnType::Column(c) => c.get() as u32,
            gimli::ColumnType::LeftEdge => 0,
        };
        let path = file_path(dwarf, unit, header, row.file_index());
        let file = intern_file(table, file_ids, path);
        table.rows.push(Row {
            addr: addr as u32,
            file,
            line,
            column,
        });
    }
    Ok(())
}

fn intern_file(table: &mut LineTable, file_ids: &mut HashMap<String, u32>, path: String) -> u32 {
    if let Some(&id) = file_ids.get(&path) {
        return id;
    }
    let id = table.files.len() as u32;
    table.files.push(Arc::from(path.as_str()));
    file_ids.insert(path, id);
    id
}

/// `directory/path_name` for a file-table entry, best-effort. Falls back to `<unknown>`.
fn file_path(
    dwarf: &gimli::Dwarf<Slice<'_>>,
    unit: &gimli::Unit<Slice<'_>>,
    header: &gimli::LineProgramHeader<Slice<'_>>,
    file_index: u64,
) -> String {
    let Some(file) = header.file(file_index) else {
        return "<unknown>".to_string();
    };
    let name = attr_str(dwarf, unit, file.path_name());
    let dir = file
        .directory(header)
        .map(|d| attr_str(dwarf, unit, d))
        .unwrap_or_default();
    match (dir.is_empty(), name.is_empty()) {
        (_, true) => "<unknown>".to_string(),
        (true, false) => name,
        (false, false) if dir.ends_with('/') => format!("{dir}{name}"),
        (false, false) => format!("{dir}/{name}"),
    }
}

fn attr_str(
    dwarf: &gimli::Dwarf<Slice<'_>>,
    unit: &gimli::Unit<Slice<'_>>,
    attr: gimli::AttributeValue<Slice<'_>>,
) -> String {
    dwarf
        .attr_string(unit, attr)
        .ok()
        .and_then(|r| std::str::from_utf8(r.slice()).ok().map(str::to_owned))
        .unwrap_or_default()
}
