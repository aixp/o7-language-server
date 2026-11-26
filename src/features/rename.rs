//! Rename.
//!
//! The edit changes all references of the symbol (as Find References gives
//! them). Before the server returns the edit, it checks it: it analyses the
//! changed files with the new texts and refuses the rename if a new error
//! occurs or if a reference resolves to another symbol (the new name hides
//! another name, or another name hides the new name).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use lsp_types::{Position, Range, TextEdit, Url, WorkspaceEdit};
use o7_front::diag::{Pos, Span};
use o7_front::index::{Extern, Origin, SymKind};
use o7_front::lexer::KEYWORDS;

use super::{declaration, front_pos, range, references_of, symbol};
use crate::vfs::{Encoding, path_to_uri};
use crate::workspace::{Context, ScratchUnit, Unit, Workspace};

/// Checks that the symbol at the position can be renamed; returns the range
/// of the identifier and its text.
pub fn prepare(
	ws: &Workspace,
	unit: &Rc<Unit>,
	p: Position,
	enc: Encoding,
) -> Result<(Range, String), String> {
	let pos = front_pos(unit, p, enc);
	let r = *unit
		.analysis
		.index
		.ref_at(pos)
		.ok_or("no symbol at this position")?;
	check_symbol(ws, unit, r.sym)?;
	Ok((range(unit, r.span, enc), symbol(unit, r.sym).name.clone()))
}

fn check_symbol(ws: &Workspace, unit: &Rc<Unit>, sym: u32) -> Result<(Rc<Unit>, u32), String> {
	let s = symbol(unit, sym);
	match (&s.origin, s.kind) {
		(Origin::Predeclared, _) => return Err(format!("{} is a predeclared name", s.name)),
		(_, SymKind::Module) => {
			return Err("a module cannot be renamed (the file name must change too)".to_string());
		}
		(Origin::Imported { module, .. }, SymKind::Import) if *module == s.name => {
			return Err(format!(
				"{} is the name of the imported module; use an alias (`X := {}`)",
				s.name, s.name
			));
		}
		_ => {}
	}
	let (du, id) =
		declaration(ws, unit, sym).ok_or("the declaration of the symbol is not available")?;
	let fresh = ws
		.unit(du.target, &du.path)
		.is_some_and(|l| Rc::ptr_eq(l, &du));
	if !fresh {
		return Err(format!(
			"module {} has errors; the declaration is not in its current text",
			du.name
		));
	}
	if symbol(&du, id).name_span.is_none() {
		return Err("the symbol has no declaration in the text".to_string());
	}
	Ok((du, id))
}

pub fn is_identifier(s: &str) -> bool {
	let mut c = s.bytes();
	c.next().is_some_and(|b| b.is_ascii_alphabetic()) && c.all(|b| b.is_ascii_alphanumeric())
}

/// The edit that renames the symbol at the position to `new_name`. The
/// analyses of the workspace index must be up to date
/// (`Workspace::refresh_index`).
pub fn rename(
	ws: &Workspace,
	unit: &Rc<Unit>,
	p: Position,
	new_name: &str,
	enc: Encoding,
) -> Result<WorkspaceEdit, String> {
	if !is_identifier(new_name) {
		return Err(format!("{new_name} is not an identifier"));
	}
	if KEYWORDS.contains(&new_name) {
		return Err(format!("{new_name} is a reserved word"));
	}
	let pos = front_pos(unit, p, enc);
	let r = *unit
		.analysis
		.index
		.ref_at(pos)
		.ok_or("no symbol at this position")?;
	let (du, decl) = check_symbol(ws, unit, r.sym)?;
	let old_name = symbol(&du, decl).name.clone();
	let refs = references_of(ws, unit, r.sym);
	// The ranges to change in each file.
	let mut edits: BTreeMap<PathBuf, (Rc<Unit>, Vec<Span>)> = BTreeMap::new();
	for (u, r) in refs {
		let e = edits
			.entry(u.path.clone())
			.or_insert_with(|| (u.clone(), Vec::new()));
		if !e.1.contains(&r.span) {
			e.1.push(r.span);
		}
	}
	if old_name == new_name {
		return Ok(WorkspaceEdit::default());
	}
	// Each file must be the current analysis of its text.
	for (path, (u, _)) in &edits {
		if ws.text(path).as_deref() != Some(&*u.src) {
			return Err(format!("{} changed; try again", path.display()));
		}
	}
	// The check of the new texts is for one target. A module of programs
	// for different targets needs a check for each target: not available.
	let mut contexts: BTreeMap<PathBuf, Context> = BTreeMap::new();
	for path in edits.keys() {
		let mut list = ws.contexts_of(path)?;
		let mut targets: Vec<&str> = list.iter().map(|c| c.target.name).collect();
		targets.dedup();
		if targets.len() > 1 {
			return Err(format!(
				"{} is in programs for different targets ({}); a rename in such a module is not available",
				path.display(),
				targets.join(", ")
			));
		}
		contexts.insert(path.clone(), list.swap_remove(0));
	}
	let delta = new_name.len() as i64 - old_name.len() as i64;
	let mut texts: HashMap<PathBuf, Vec<u8>> = HashMap::new();
	for (path, (u, spans)) in &mut edits {
		spans.sort();
		let mut text = u.src.to_vec();
		for s in spans.iter().rev() {
			let a = u.lines.offset(s.start);
			let b = u.lines.offset(s.end);
			if text.get(a..b) != Some(old_name.as_bytes()) {
				return Err(format!(
					"internal error: unexpected text at {}:{}",
					path.display(),
					s.start
				));
			}
			text.splice(a..b, new_name.bytes());
		}
		texts.insert(path.clone(), text);
	}
	let decl_start = symbol(&du, decl)
		.name_span
		.map(|s| s.start)
		.unwrap_or_default();
	let decl_spans = edits
		.get(&du.path)
		.map(|e| e.1.as_slice())
		.ok_or("internal error: the declaration has no reference")?;
	let decl_key = (du.path.clone(), shift(decl_spans, decl_start, delta));
	check(ws, &edits, &contexts, &texts, delta, &decl_key)?;

	let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
	for (path, (u, spans)) in &edits {
		let uri = path_to_uri(path).ok_or_else(|| format!("{} has no URI", path.display()))?;
		changes.insert(
			uri,
			spans
				.iter()
				.map(|s| TextEdit::new(range(u, *s, enc), new_name.to_string()))
				.collect(),
		);
	}
	Ok(WorkspaceEdit {
		changes: Some(changes),
		..WorkspaceEdit::default()
	})
}

/// The position `p` after the replacement of the identifiers at `spans`
/// (all on one line each, all of the same length) by a name that is
/// `delta` bytes longer.
fn shift(spans: &[Span], p: Pos, delta: i64) -> Pos {
	let k = spans
		.iter()
		.filter(|s| s.start.line == p.line && s.start.col < p.col)
		.count() as i64;
	Pos::new(p.line, (p.col as i64 + k * delta).max(1) as u32)
}

/// The analysis of the new texts: no new error, and exactly the changed
/// identifiers denote the renamed declaration.
fn check(
	ws: &Workspace,
	edits: &BTreeMap<PathBuf, (Rc<Unit>, Vec<Span>)>,
	contexts: &BTreeMap<PathBuf, Context>,
	texts: &HashMap<PathBuf, Vec<u8>>,
	delta: i64,
	decl: &(PathBuf, Pos),
) -> Result<(), String> {
	// One cache for each target.
	let mut caches: HashMap<&str, HashMap<PathBuf, Rc<ScratchUnit>>> = HashMap::new();
	for (path, (old, spans)) in edits {
		let ctx = contexts
			.get(path)
			.ok_or_else(|| format!("{} cannot be analysed", path.display()))?;
		let cache = caches.entry(ctx.target.name).or_default();
		let program = ws.analyse_changed(ctx, path, texts, cache);
		let Some(new) = program.iter().find(|u| u.path == *path) else {
			return Err(format!("{} cannot be analysed", path.display()));
		};
		// No new error.
		let old_msgs: HashSet<&str> = old.diags.iter().map(|d| d.msg.as_str()).collect();
		let new_list = new.analysis.diags.list();
		if new.analysis.diags.error_count() > old.analysis.diags.error_count()
			|| (!old.has_errors() && new.analysis.diags.has_errors())
		{
			let first = new_list
				.iter()
				.find(|d| !old_msgs.contains(d.msg.as_str()))
				.or(new_list.first());
			let detail = first.map_or(String::new(), |d| {
				format!(": {}:{}: {}", d.file, d.pos, d.msg)
			});
			return Err(format!("the new name causes an error{detail}"));
		}
		// Exactly the changed identifiers denote the declaration.
		let expected: HashSet<Pos> = spans.iter().map(|s| shift(spans, s.start, delta)).collect();
		let actual: HashSet<Pos> = new
			.analysis
			.index
			.refs()
			.iter()
			.filter(|r| resolve(&program, new, r.sym).as_ref() == Some(decl))
			.map(|r| r.span.start)
			.collect();
		if expected != actual {
			let at = expected
				.symmetric_difference(&actual)
				.min()
				.map_or(String::new(), |p| format!(" at {}:{p}", path.display()));
			return Err(format!(
				"the new name changes the meaning of the program{at}"
			));
		}
	}
	Ok(())
}

/// The declaration of a symbol of the program: the file and the position of
/// the name.
fn resolve(program: &[Rc<ScratchUnit>], u: &ScratchUnit, sym: u32) -> Option<(PathBuf, Pos)> {
	let s = u.analysis.index.symbol(sym);
	if let Some(n) = s.name_span {
		return Some((u.path.clone(), n.start));
	}
	let Origin::Imported { module, item } = &s.origin else {
		return None;
	};
	let t = program.iter().find(|t| t.name == *module)?;
	let ix = &t.analysis.index;
	let id = match item {
		Extern::Object(name) => ix.export(name)?,
		Extern::Field { type_index, name } => {
			ix.field_symbol(*t.type_map.get(*type_index as usize)?, name)?
		}
		Extern::Module => ix.module_symbol()?,
	};
	Some((t.path.clone(), ix.symbol(id).name_span?.start))
}
