//! Hover: the description of the symbol at the position, or the type of
//! the expression at the position.

use std::rc::Rc;

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};
use o7_front::index::{Origin, SymKind};

use super::{declaration, front_pos, range};
use crate::vfs::Encoding;
use crate::workspace::{Unit, Workspace};

pub fn hover(ws: &Workspace, unit: &Rc<Unit>, p: Position, enc: Encoding) -> Option<Hover> {
	let ix = &unit.analysis.index;
	let m = &unit.analysis.module;
	let pos = front_pos(unit, p, enc);
	if let Some(r) = ix.ref_at(pos) {
		let s = ix.symbol(r.sym);
		let mut text = String::new();
		let mut module = None;
		if let Origin::Imported { module: name, .. } = &s.origin
			&& !matches!(s.kind, SymKind::Import | SymKind::Module)
		{
			module = Some(name.clone());
			// The index of the exporting module has the parameter names.
			if let Some((du, id)) = declaration(ws, unit, r.sym) {
				text = du.analysis.index.describe(&du.analysis.module, id);
			}
		}
		if text.is_empty() {
			text = ix.describe(m, r.sym);
		}
		if text.is_empty() {
			return None;
		}
		let mut value = format!("```oberon\n{text}\n```");
		if let Some(name) = module {
			value.push_str(&format!("\n\nModule `{name}`"));
		}
		return Some(make(value, Some(range(unit, r.span, enc))));
	}
	let (span, ty) = ix.type_at(pos)?;
	let t = m.types.get(ty)?;
	let text = if t.name.is_empty() {
		ix.describe_type(m, ty)
	} else {
		m.type_name(ty)
	};
	Some(make(
		format!("```oberon\n{text}\n```"),
		Some(range(unit, span, enc)),
	))
}

fn make(value: String, range: Option<lsp_types::Range>) -> Hover {
	Hover {
		contents: HoverContents::Markup(MarkupContent {
			kind: MarkupKind::Markdown,
			value,
		}),
		range,
	}
}
