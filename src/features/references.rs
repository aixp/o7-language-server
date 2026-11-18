//! Find References.

use std::rc::Rc;

use lsp_types::{Location, Position};

use super::{front_pos, is_decl, location, references_of};
use crate::vfs::Encoding;
use crate::workspace::{Unit, Workspace};

/// The references of the symbol at the position. The analyses of the
/// workspace index must be up to date (`Workspace::refresh_index`).
pub fn references(
	ws: &Workspace,
	unit: &Rc<Unit>,
	p: Position,
	include_declaration: bool,
	enc: Encoding,
) -> Vec<Location> {
	let Some(r) = unit.analysis.index.ref_at(front_pos(unit, p, enc)) else {
		return Vec::new();
	};
	let mut out = Vec::new();
	for (u, r) in references_of(ws, unit, r.sym) {
		if !include_declaration && is_decl(&r) {
			continue;
		}
		if let Some(loc) = location(&u, r.span, enc)
			&& !out.contains(&loc)
		{
			out.push(loc);
		}
	}
	out
}
