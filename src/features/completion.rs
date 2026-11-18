//! Completion.
//!
//! After a `.`: the exported objects of an imported module (the procedures
//! of `SYSTEM` for `SYSTEM`), or the fields of a record (also the inherited
//! fields; a pointer gives the fields of its record). Else: the names that
//! are visible at the position, the predeclared names and the reserved
//! words.

use std::rc::Rc;

use lsp_types::{CompletionItem, CompletionItemKind, Position};
use o7_front::diag::{Diagnostics, Pos};
use o7_front::front::{PredeclaredKind, predeclared_names};
use o7_front::hir::{Form, Module};
use o7_front::index::{DotBase, Index, Origin, SymId, SymKind};
use o7_front::lexer::{KEYWORDS, Lexer, Tok};
use o7_front::target_info::TargetInfo;

use crate::vfs::Encoding;
use crate::workspace::{Unit, Workspace};

pub fn completion(
	ws: &Workspace,
	unit: &Rc<Unit>,
	p: Position,
	enc: Encoding,
) -> Vec<CompletionItem> {
	let src = &unit.src;
	let off = unit.lines.lsp_offset(src, p, enc);
	if in_comment_or_string(src, off) {
		return Vec::new();
	}
	// The start of the identifier before the cursor.
	let mut start = off;
	while start > 0 && src[start - 1].is_ascii_alphanumeric() {
		start -= 1;
	}
	if start < off && src[start].is_ascii_digit() {
		return Vec::new();
	}
	let mut j = start;
	while j > 0 && matches!(src[j - 1], b' ' | b'\t') {
		j -= 1;
	}
	// `x.` but not `1..` (a range).
	if j > 0 && src[j - 1] == b'.' && !(j > 1 && src[j - 2] == b'.') {
		return members(ws, unit, j - 1);
	}
	scope_items(ws, unit, unit.lines.pos(start))
}

/// The cursor at `off` is inside a comment or a string: no completion.
fn in_comment_or_string(src: &[u8], off: usize) -> bool {
	let li = crate::vfs::LineIndex::new(src);
	let pos = li.pos(off);
	let mut d = Diagnostics::new("");
	let mut lx = Lexer::new(src);
	let mut prev_end = Pos::new(1, 1);
	loop {
		let (tok, start) = lx.next(&mut d);
		let end = lx.token_end();
		if tok == Tok::Eof || start >= pos {
			// Between two symbols: in a comment if the text from the end of
			// the last symbol to the cursor opens more comments than it
			// closes.
			let a = li.offset(prev_end);
			let gap = &src[a.min(off)..off];
			let opens = gap.windows(2).filter(|w| w == b"(*").count();
			let closes = gap.windows(2).filter(|w| w == b"*)").count();
			return opens > closes || (tok == Tok::Eof && lx.unterminated_comment() && opens > 0);
		}
		if end > pos {
			// The cursor is inside this symbol.
			return matches!(tok, Tok::Str(_));
		}
		prev_end = end;
	}
}

/// The completion items after the `.` at the byte offset `dot`.
fn members(ws: &Workspace, unit: &Rc<Unit>, dot: usize) -> Vec<CompletionItem> {
	let ix = &unit.analysis.index;
	let dot_pos = unit.lines.pos(dot);
	match ix.dot_base(dot_pos) {
		Some(DotBase::Module(sym)) => module_members(ws, unit, sym),
		Some(DotBase::Value(ty)) => {
			let m = &unit.analysis.module;
			ix.record_fields(m, ty)
				.into_iter()
				.enumerate()
				.map(|(i, f)| CompletionItem {
					label: f.name.clone(),
					kind: Some(CompletionItemKind::FIELD),
					detail: (f.ty < m.types.len()).then(|| m.type_name(f.ty)),
					sort_text: Some(format!("{i:04}")),
					..CompletionItem::default()
				})
				.collect()
		}
		None => {
			// The analysis stopped before the `.`: an import name before it
			// still gives the module.
			let src = &unit.src;
			let mut s = dot;
			while s > 0 && src[s - 1].is_ascii_alphanumeric() {
				s -= 1;
			}
			let name = String::from_utf8_lossy(&src[s..dot]);
			ix.visible_at(dot_pos)
				.into_iter()
				.find(|&id| {
					let sym = ix.symbol(id);
					sym.kind == SymKind::Import && sym.name == name
				})
				.map(|id| module_members(ws, unit, id))
				.unwrap_or_default()
		}
	}
}

/// The exported objects of the module of the import name `sym`.
fn module_members(ws: &Workspace, unit: &Rc<Unit>, sym: SymId) -> Vec<CompletionItem> {
	let s = unit.analysis.index.symbol(sym);
	match &s.origin {
		Origin::Predeclared => predeclared_names(ws.config.target as &dyn TargetInfo)
			.system
			.into_iter()
			.map(|(name, k)| predeclared_item(name, k, "SYSTEM"))
			.collect(),
		Origin::Imported { module, .. } => {
			let Some(path) = unit.dep_path(module) else {
				return Vec::new();
			};
			// The interface that the importer sees: the last snapshot; else
			// the last analysis.
			let Some(t) = ws.snapshot(path).map(|s| &s.unit).or(ws.unit(path)) else {
				return Vec::new();
			};
			let tix = &t.analysis.index;
			tix.exports()
				.map(|id| item(tix, &t.analysis.module, id, "0"))
				.collect()
		}
		Origin::Local => Vec::new(),
	}
}

fn scope_items(ws: &Workspace, unit: &Rc<Unit>, pos: Pos) -> Vec<CompletionItem> {
	let ix = &unit.analysis.index;
	let m = &unit.analysis.module;
	let level = ix.level_at(pos);
	let mut out = Vec::new();
	for id in ix.visible_at(pos) {
		let s = ix.symbol(id);
		// A variable of an enclosing procedure is visible but not accessible.
		if matches!(s.kind, SymKind::Var | SymKind::Param) && s.level != 0 && s.level != level {
			continue;
		}
		out.push(item(ix, m, id, "0"));
	}
	for (name, k) in predeclared_names(ws.config.target as &dyn TargetInfo).universe {
		if !out.iter().any(|i| i.label == name) {
			out.push(predeclared_item(name, k, ""));
		}
	}
	for k in KEYWORDS {
		out.push(CompletionItem {
			label: k.to_string(),
			kind: Some(CompletionItemKind::KEYWORD),
			sort_text: Some(format!("2{k}")),
			..CompletionItem::default()
		});
	}
	out
}

fn item(ix: &Index, m: &Module, id: SymId, group: &str) -> CompletionItem {
	let s = ix.symbol(id);
	let kind = match s.kind {
		SymKind::Module | SymKind::Import => CompletionItemKind::MODULE,
		SymKind::Const => CompletionItemKind::CONSTANT,
		SymKind::Type => match s.ty.and_then(|t| m.types.get(t)).map(|t| t.form) {
			Some(Form::Record | Form::Pointer) => CompletionItemKind::STRUCT,
			_ => CompletionItemKind::TYPE_PARAMETER,
		},
		SymKind::Var | SymKind::Param => CompletionItemKind::VARIABLE,
		SymKind::Field => CompletionItemKind::FIELD,
		SymKind::Proc | SymKind::Builtin => CompletionItemKind::FUNCTION,
	};
	let detail = ix.describe(m, id);
	CompletionItem {
		label: s.name.clone(),
		kind: Some(kind),
		detail: (!detail.is_empty()).then_some(detail),
		sort_text: Some(format!("{group}{}", s.name)),
		..CompletionItem::default()
	}
}

fn predeclared_item(name: String, k: PredeclaredKind, module: &str) -> CompletionItem {
	let (kind, what) = match k {
		PredeclaredKind::Type => (CompletionItemKind::TYPE_PARAMETER, "predeclared type"),
		PredeclaredKind::Function => (CompletionItemKind::FUNCTION, "predeclared function"),
		PredeclaredKind::Procedure => (CompletionItemKind::FUNCTION, "predeclared procedure"),
	};
	let detail = if module.is_empty() {
		what.to_string()
	} else {
		format!("{what} of {module}")
	};
	CompletionItem {
		sort_text: Some(format!("1{name}")),
		label: name,
		kind: Some(kind),
		detail: Some(detail),
		..CompletionItem::default()
	}
}
