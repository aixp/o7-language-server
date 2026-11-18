//! The requests of the protocol. Each module answers one kind of request
//! with the data of the [`Workspace`].
//!
//! Rules (from the plan of the IDE support): the server uses the typed tree
//! of the front end (`hir::Module`) only through the functions of the index
//! and `Module::type_name`. A symbol of another module is identified by
//! `(module name, Extern)`; its declaration is found in the index of that
//! module.

pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod hover;
pub mod references;
pub mod rename;
pub mod semantic_tokens;
pub mod symbols;

use std::rc::Rc;

use lsp_types::{Location, Position, Range};
use o7_front::diag::{Pos, Span};
use o7_front::index::{Extern, Origin, Reference, Role, SymId, SymKind, Symbol};

use crate::vfs::{Encoding, path_to_uri};
use crate::workspace::{Unit, Workspace};

pub fn range(unit: &Unit, span: Span, enc: Encoding) -> Range {
	unit.lines.lsp_range(&unit.src, span, enc)
}

pub fn location(unit: &Unit, span: Span, enc: Encoding) -> Option<Location> {
	Some(Location::new(
		path_to_uri(&unit.path)?,
		range(unit, span, enc),
	))
}

/// The front-end position of an LSP position in the text of the unit.
pub fn front_pos(unit: &Unit, p: Position, enc: Encoding) -> Pos {
	unit.lines.front_pos(&unit.src, p, enc)
}

pub fn symbol(unit: &Unit, sym: SymId) -> &Symbol {
	unit.analysis.index.symbol(sym)
}

/// Follows a symbol to its declaration: the unit of the file that declares
/// it and the symbol in the index of that file. `None` for a predeclared
/// object and for an imported object whose module is not available.
pub fn declaration(ws: &Workspace, unit: &Rc<Unit>, sym: SymId) -> Option<(Rc<Unit>, SymId)> {
	let s = symbol(unit, sym);
	if s.name_span.is_some() {
		return Some((unit.clone(), sym));
	}
	match &s.origin {
		Origin::Local => Some((unit.clone(), sym)),
		Origin::Predeclared => None,
		Origin::Imported { module, item } => {
			let path = unit.dep_path(module)?;
			extern_declaration(ws, path, item)
		}
	}
}

/// The declaration of the object `item` of the module in the file `path`.
///
/// The last analysis of the file has priority. If the file has errors now,
/// the object can be absent from it; then the analysis of the last
/// snapshot gives the declaration. A field is identified by a type index of
/// the symbol file, thus through the snapshot.
pub fn extern_declaration(
	ws: &Workspace,
	path: &std::path::Path,
	item: &Extern,
) -> Option<(Rc<Unit>, SymId)> {
	let latest = ws.unit(path);
	let snap = ws.snapshot(path);
	match item {
		Extern::Object(name) => {
			if let Some(u) = latest
				&& let Some(id) = u.analysis.index.export(name)
			{
				return Some((u.clone(), id));
			}
			let s = snap?;
			let id = s.unit.analysis.index.export(name)?;
			Some((s.unit.clone(), id))
		}
		Extern::Module => {
			let u = latest.or(snap.map(|s| &s.unit))?;
			Some((u.clone(), u.analysis.index.module_symbol()?))
		}
		Extern::Field { type_index, name } => {
			let s = snap?;
			let record = *s.type_map.get(*type_index as usize)?;
			let id = s.unit.analysis.index.field_symbol(record, name)?;
			match latest {
				Some(l) if Rc::ptr_eq(l, &s.unit) => Some((l.clone(), id)),
				Some(l) => match same_symbol(&s.unit, id, l) {
					Some(lid) => Some((l.clone(), lid)),
					None => Some((s.unit.clone(), id)),
				},
				None => Some((s.unit.clone(), id)),
			}
		}
	}
}

/// The symbol of `to` that is the symbol `id` of `from` (another analysis of
/// the same file): same name, same kind, and a parent with the same name.
fn same_symbol(from: &Unit, id: SymId, to: &Unit) -> Option<SymId> {
	let s = symbol(from, id);
	let parent_name = |u: &Unit, s: &Symbol| s.parent.map(|p| symbol(u, p).name.clone());
	let want = parent_name(from, s);
	to.analysis
		.index
		.symbols()
		.iter()
		.position(|t| {
			t.name == s.name
				&& t.kind == s.kind
				&& t.name_span.is_some()
				&& parent_name(to, t) == want
		})
		.map(|i| i as SymId)
}

/// How other modules denote a declaration.
enum Exported {
	Object(String),
	/// The type indices of the record type in the symbol file, and the name
	/// of the field.
	Field(Vec<u32>, String),
	Module,
}

impl Exported {
	fn matches(&self, module: &str, s: &Symbol) -> bool {
		let Origin::Imported { module: m, item } = &s.origin else {
			return false;
		};
		if m != module {
			return false;
		}
		match (self, item) {
			(Exported::Object(n), Extern::Object(x)) => n == x,
			(Exported::Field(idx, n), Extern::Field { type_index, name }) => {
				n == name && idx.contains(type_index)
			}
			(Exported::Module, Extern::Module) => {
				// `alias := M`, or `M` without an alias.
				s.kind == SymKind::Module || s.name == *m
			}
			_ => false,
		}
	}
}

/// All references to the declaration `decl` of the unit `du`: in its own
/// file, and, for an exported object, in each analysed file that imports
/// the module. Call [`Workspace::refresh_index`] before, so that the
/// analyses of all files are up to date.
pub fn find_refs(ws: &Workspace, du: &Rc<Unit>, decl: SymId) -> Vec<(Rc<Unit>, Reference)> {
	let ix = &du.analysis.index;
	let mut out: Vec<(Rc<Unit>, Reference)> = ix.refs_of(decl).map(|r| (du.clone(), *r)).collect();
	let s = ix.symbol(decl);
	let exported = match s.kind {
		SymKind::Module => Some(Exported::Module),
		SymKind::Field if s.exported => {
			let fresh = ws.unit(&du.path).is_some_and(|l| Rc::ptr_eq(l, du));
			match (ws.snapshot(&du.path), s.record) {
				(Some(snap), Some(record)) if fresh && Rc::ptr_eq(&snap.unit, du) => {
					let idx: Vec<u32> = snap
						.type_map
						.iter()
						.enumerate()
						.filter(|(_, t)| **t == record)
						.map(|(i, _)| i as u32)
						.collect();
					(!idx.is_empty()).then(|| Exported::Field(idx, s.name.clone()))
				}
				_ => None,
			}
		}
		SymKind::Const | SymKind::Type | SymKind::Var | SymKind::Proc
			if ix.export(&s.name) == Some(decl) =>
		{
			Some(Exported::Object(s.name.clone()))
		}
		_ => None,
	};
	let Some(exported) = exported else {
		return out;
	};
	let mut importers: Vec<&Rc<Unit>> = ws
		.units()
		.filter(|u| u.path != du.path && u.dep_path(&du.name) == Some(du.path.as_path()))
		.collect();
	importers.sort_by(|a, b| a.path.cmp(&b.path));
	for u in importers {
		let uix = &u.analysis.index;
		for (i, t) in uix.symbols().iter().enumerate() {
			if exported.matches(&du.name, t) {
				out.extend(uix.refs_of(i as SymId).map(|r| (u.clone(), *r)));
			}
		}
	}
	out
}

/// The references of a symbol for the requests "references" and "rename":
/// the declaration and the result of [`find_refs`]. For a predeclared
/// object: its references in this file.
pub fn references_of(ws: &Workspace, unit: &Rc<Unit>, sym: SymId) -> Vec<(Rc<Unit>, Reference)> {
	match declaration(ws, unit, sym) {
		Some((du, id)) => find_refs(ws, &du, id),
		None => unit
			.analysis
			.index
			.refs_of(sym)
			.map(|r| (unit.clone(), *r))
			.collect(),
	}
}

pub fn is_decl(r: &Reference) -> bool {
	r.role == Role::Decl
}
