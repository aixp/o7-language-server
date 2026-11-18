//! Semantic tokens: one token for each identifier of the text that has a
//! reference in the index.
//!
//! Token types: `namespace` (modules and import names), `type`, `variable`
//! (also constants), `parameter`, `property` (fields), `function`
//! (procedures and predeclared procedures). Modifiers: `declaration`,
//! `readonly` (constants, imported variables, structured value parameters),
//! `defaultLibrary` (predeclared names and `SYSTEM`).

use lsp_types::{
	SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};
use o7_front::hir::ParamMode;
use o7_front::index::{Origin, Role, SymKind, Symbol};

use crate::vfs::Encoding;
use crate::workspace::Unit;

pub const TYPES: [SemanticTokenType; 6] = [
	SemanticTokenType::NAMESPACE,
	SemanticTokenType::TYPE,
	SemanticTokenType::VARIABLE,
	SemanticTokenType::PARAMETER,
	SemanticTokenType::PROPERTY,
	SemanticTokenType::FUNCTION,
];

pub const MODIFIERS: [SemanticTokenModifier; 3] = [
	SemanticTokenModifier::DECLARATION,
	SemanticTokenModifier::READONLY,
	SemanticTokenModifier::DEFAULT_LIBRARY,
];

const DECLARATION: u32 = 1;
const READONLY: u32 = 2;
const DEFAULT_LIBRARY: u32 = 4;

pub fn legend() -> SemanticTokensLegend {
	SemanticTokensLegend {
		token_types: TYPES.to_vec(),
		token_modifiers: MODIFIERS.to_vec(),
	}
}

/// The token type (index into [`TYPES`]) and the modifiers of a reference.
pub fn classify(s: &Symbol, role: Role) -> (u32, u32) {
	let ty = match s.kind {
		SymKind::Module | SymKind::Import => 0,
		SymKind::Type => 1,
		SymKind::Var | SymKind::Const => 2,
		SymKind::Param => 3,
		SymKind::Field => 4,
		SymKind::Proc | SymKind::Builtin => 5,
	};
	let mut m = 0;
	if role == Role::Decl {
		m |= DECLARATION;
	}
	let imported = matches!(s.origin, Origin::Imported { .. });
	if s.kind == SymKind::Const
		|| (s.kind == SymKind::Var && imported)
		|| (s.kind == SymKind::Param && s.mode == Some(ParamMode::Ref))
	{
		m |= READONLY;
	}
	if s.origin == Origin::Predeclared {
		m |= DEFAULT_LIBRARY;
	}
	(ty, m)
}

pub fn full(unit: &Unit, enc: Encoding) -> SemanticTokens {
	let ix = &unit.analysis.index;
	let src = &unit.src;
	let mut data = Vec::with_capacity(ix.refs().len());
	let (mut line, mut col) = (0u32, 0u32);
	for r in ix.refs() {
		let a = unit.lines.offset(r.span.start);
		let b = unit.lines.offset(r.span.end).max(a);
		let start = unit.lines.lsp_position(src, a, enc);
		let end = unit.lines.lsp_position(src, b, enc);
		if end.line != start.line || end.character <= start.character {
			continue;
		}
		let (ty, m) = classify(ix.symbol(r.sym), r.role);
		let delta_line = start.line - line;
		let delta_start = if delta_line == 0 {
			start.character - col
		} else {
			start.character
		};
		data.push(SemanticToken {
			delta_line,
			delta_start,
			length: end.character - start.character,
			token_type: ty,
			token_modifiers_bitset: m,
		});
		line = start.line;
		col = start.character;
	}
	SemanticTokens {
		result_id: None,
		data,
	}
}
