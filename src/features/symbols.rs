//! Document symbols (outline) and workspace symbols.

use std::collections::HashMap;
use std::rc::Rc;

use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind};
use o7_front::hir::{Form, Module};
use o7_front::index::{Index, Origin, SymId, SymKind, Symbol};

use super::range;
use crate::vfs::{Encoding, path_to_uri};
use crate::workspace::{Unit, Workspace};

/// Maximum number of results of a workspace symbol query.
const MAX_WORKSPACE_SYMBOLS: usize = 1000;

fn kind(m: &Module, s: &Symbol) -> Option<SymbolKind> {
	Some(match s.kind {
		SymKind::Module => SymbolKind::MODULE,
		SymKind::Const => SymbolKind::CONSTANT,
		SymKind::Type => match s.ty.and_then(|t| m.types.get(t)).map(|t| t.form) {
			Some(Form::Record | Form::Pointer) => SymbolKind::STRUCT,
			_ => SymbolKind::TYPE_PARAMETER,
		},
		SymKind::Var => SymbolKind::VARIABLE,
		SymKind::Field => SymbolKind::FIELD,
		SymKind::Proc => SymbolKind::FUNCTION,
		SymKind::Import | SymKind::Param | SymKind::Builtin => return None,
	})
}

/// A short text after the name in the outline.
fn detail(ix: &Index, m: &Module, id: SymId) -> Option<String> {
	let s = ix.symbol(id);
	let type_name = |t: Option<usize>| t.filter(|&t| t < m.types.len()).map(|t| m.type_name(t));
	match s.kind {
		SymKind::Var | SymKind::Field => type_name(s.ty),
		SymKind::Const => {
			let d = ix.describe(m, id);
			d.split_once(" = ").map(|(_, v)| v.to_string())
		}
		SymKind::Proc => {
			// "PROCEDURE P*(x: INTEGER): INTEGER" -> "(x: INTEGER): INTEGER"
			let d = ix.describe(m, id);
			d.find('(').map(|i| d[i..].to_string())
		}
		_ => None,
	}
}

/// The outline of the file: the module, its constants, types (with their
/// fields), variables and procedures (nested).
#[allow(deprecated)]
pub fn document_symbols(unit: &Unit, enc: Encoding) -> Vec<DocumentSymbol> {
	let ix = &unit.analysis.index;
	let m = &unit.analysis.module;
	let mut nodes: HashMap<SymId, DocumentSymbol> = HashMap::new();
	let mut parents: Vec<(SymId, Option<SymId>)> = Vec::new();
	for (i, s) in ix.symbols().iter().enumerate() {
		let id = i as SymId;
		if s.origin != Origin::Local {
			continue;
		}
		let (Some(name_span), Some(k)) = (s.name_span, kind(m, s)) else {
			continue;
		};
		let selection = range(unit, name_span, enc);
		let mut full = s.decl_span.map_or(selection, |d| range(unit, d, enc));
		// The selection must be inside the range.
		if full.start > selection.start {
			full.start = selection.start;
		}
		if full.end < selection.end {
			full.end = selection.end;
		}
		let name = if s.exported && s.kind != SymKind::Module {
			format!("{}*", s.name)
		} else {
			s.name.clone()
		};
		nodes.insert(
			id,
			DocumentSymbol {
				name,
				detail: detail(ix, m, id),
				kind: k,
				tags: None,
				deprecated: None,
				range: full,
				selection_range: selection,
				children: None,
			},
		);
		parents.push((id, s.parent));
	}
	// Attach each node to the nearest ancestor that has a node. Children
	// first: a procedure gets its local procedures before it is attached.
	let parent_of = |mut p: Option<SymId>| {
		let mut steps = 0;
		while let Some(id) = p {
			if nodes.contains_key(&id) {
				return Some(id);
			}
			p = ix.symbol(id).parent;
			steps += 1;
			if steps > ix.symbols().len() {
				break;
			}
		}
		None
	};
	let links: HashMap<SymId, Option<SymId>> =
		parents.iter().map(|&(id, p)| (id, parent_of(p))).collect();
	let depth = |mut id: SymId| {
		let mut d = 0;
		while let Some(&Some(p)) = links.get(&id) {
			id = p;
			d += 1;
			if d > links.len() {
				break;
			}
		}
		d
	};
	let mut order: Vec<(usize, SymId, Option<SymId>)> =
		links.iter().map(|(&id, &p)| (depth(id), id, p)).collect();
	// The deepest nodes first.
	order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
	let mut roots = Vec::new();
	for (_, id, parent) in order {
		let Some(node) = nodes.remove(&id) else {
			continue;
		};
		match parent.and_then(|p| nodes.get_mut(&p)) {
			Some(pn) => pn.children.get_or_insert_with(Vec::new).push(node),
			None => roots.push(node),
		}
	}
	fn sort(list: &mut [DocumentSymbol]) {
		list.sort_by_key(|n| {
			(
				n.selection_range.start.line,
				n.selection_range.start.character,
			)
		});
		for n in list {
			if let Some(c) = &mut n.children {
				sort(c);
			}
		}
	}
	sort(&mut roots);
	roots
}

/// `query` is a sub-sequence of `name` (letters without regard to case).
pub fn matches(query: &str, name: &str) -> bool {
	let mut q = query.chars().map(|c| c.to_ascii_lowercase()).peekable();
	for c in name.chars() {
		if q.peek() == Some(&c.to_ascii_lowercase()) {
			q.next();
		}
	}
	q.peek().is_none()
}

/// The module-level symbols of all analysed files whose name matches the
/// query.
#[allow(deprecated)]
pub fn workspace_symbols(ws: &Workspace, query: &str, enc: Encoding) -> Vec<SymbolInformation> {
	let mut units: Vec<&Rc<Unit>> = ws.units().collect();
	units.sort_by(|a, b| (&a.path, a.target.name).cmp(&(&b.path, b.target.name)));
	// A file that is analysed for more than one target: one time.
	units.dedup_by(|a, b| a.path == b.path);
	let mut out = Vec::new();
	for u in units {
		let ix = &u.analysis.index;
		let m = &u.analysis.module;
		let Some(uri) = path_to_uri(&u.path) else {
			continue;
		};
		let mut ids: Vec<SymId> = ix.module_symbol().into_iter().collect();
		if let Some(scope) = ix.scopes().first() {
			ids.extend(scope.symbols.iter().copied());
		}
		for id in ids {
			let s = ix.symbol(id);
			let (Some(span), Some(k)) = (s.name_span, kind(m, s)) else {
				continue;
			};
			if s.origin != Origin::Local || !matches(query, &s.name) {
				continue;
			}
			out.push(SymbolInformation {
				name: s.name.clone(),
				kind: k,
				tags: None,
				deprecated: None,
				location: Location::new(uri.clone(), range(u, span, enc)),
				container_name: (s.kind != SymKind::Module).then(|| u.name.clone()),
			});
			if out.len() >= MAX_WORKSPACE_SYMBOLS {
				return out;
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::matches;

	#[test]
	fn sub_sequence() {
		assert!(matches("", "Anything"));
		assert!(matches("ostr", "Out.String"));
		assert!(matches("WRS", "WriteString"));
		assert!(!matches("xs", "String"));
	}
}
