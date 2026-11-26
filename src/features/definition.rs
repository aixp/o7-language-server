//! Go to Definition.

use std::rc::Rc;

use lsp_types::{Location, Position, Range};
use o7_front::index::{Origin, SymKind};

use super::{declaration, front_pos, location, symbol};
use crate::vfs::{Encoding, path_to_uri};
use crate::workspace::{Unit, Workspace};

pub fn definition(ws: &Workspace, unit: &Rc<Unit>, p: Position, enc: Encoding) -> Option<Location> {
	let r = unit.analysis.index.ref_at(front_pos(unit, p, enc))?;
	let s = symbol(unit, r.sym);
	// An import name goes to the module.
	if s.kind == SymKind::Import {
		let Origin::Imported { module, .. } = &s.origin else {
			return None;
		};
		let path = unit.dep_path(module)?;
		if let Some(t) = ws.unit(unit.target, path)
			&& let Some(m) = t.analysis.index.module_symbol()
			&& let Some(span) = symbol(t, m).name_span
		{
			return location(t, span, enc);
		}
		return Some(Location::new(path_to_uri(path)?, Range::default()));
	}
	let (du, id) = declaration(ws, unit, r.sym)?;
	location(&du, symbol(&du, id).name_span?, enc)
}
