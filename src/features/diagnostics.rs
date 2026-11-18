//! Diagnostics: the messages of the front end and of `project::collect`.
//!
//! Diagnostics are published for the open documents and for the files in
//! their import closure. The range of a message is the lexical symbol at its
//! position (`Index::diagnostic_span` for the messages of an analysis,
//! `lexer::token_span` for the messages of `project::collect`). The severity
//! is always "error": the compiler has no warnings.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};
use o7_front::diag::{self, Pos};
use o7_front::lexer;
use o7_front::project;

use crate::vfs::{Encoding, LineIndex, normalize};
use crate::workspace::Workspace;

pub const SOURCE: &str = "o7";

/// The diagnostics of one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileDiagnostics {
	/// The version of the open document that was analysed.
	pub version: Option<i32>,
	pub list: Vec<Diagnostic>,
}

/// Brings the programs of all open documents up to date and returns the
/// diagnostics of all files in their closures.
pub fn collect(ws: &mut Workspace, enc: Encoding) -> BTreeMap<PathBuf, FileDiagnostics> {
	let roots: Vec<PathBuf> = ws.docs.keys().cloned().collect();
	let mut files: BTreeMap<PathBuf, Vec<diag::Diagnostic>> = BTreeMap::new();
	let mut problems: BTreeMap<PathBuf, Vec<diag::Diagnostic>> = BTreeMap::new();
	for root in &roots {
		let info = ws.check_root(root);
		for p in info.order {
			files.entry(p).or_default();
		}
		for d in info.problems {
			let p = normalize(&PathBuf::from(&d.file));
			let list = problems.entry(p.clone()).or_default();
			if !list.contains(&d) {
				list.push(d);
			}
			files.entry(p).or_default();
		}
	}
	// The open documents have priority: a document can be in the closure of
	// another root with another search list. Its own program decides.
	for root in &roots {
		ws.check_root(root);
	}
	let mut out = BTreeMap::new();
	for path in files.keys() {
		let mut list = Vec::new();
		let probs = problems.get(path).map(Vec::as_slice).unwrap_or(&[]);
		let text = ws.text(path);
		let lines = text.as_deref().map(LineIndex::new);
		let mut version = None;
		if let Some(u) = ws.unit(path) {
			let at_problem: HashSet<Pos> = probs.iter().map(|p| p.pos).collect();
			for d in &u.diags {
				// The front end reports "cannot import module X" at an import
				// that `project::collect` already reported.
				if at_problem.contains(&d.pos) && d.msg.starts_with("cannot import module ") {
					continue;
				}
				let span = u.analysis.index.diagnostic_span(d);
				list.push(make(u.lines.lsp_range(&u.src, span, enc), &d.msg));
			}
			if let Some(doc) = ws.docs.get(path)
				&& doc.text.as_bytes() == &*u.src
			{
				version = Some(doc.version);
			}
		}
		for d in probs {
			let range = match (&text, &lines) {
				(Some(t), Some(li)) => {
					let pos = if d.pos.line == 0 && d.msg.contains("the file contains module") {
						project::scan_header(t).name_pos
					} else {
						d.pos
					};
					if pos.line == 0 {
						Range::default()
					} else {
						li.lsp_range(t, lexer::token_span(t, pos), enc)
					}
				}
				_ => Range::default(),
			};
			list.push(make(range, &d.msg));
		}
		list.sort_by_key(|d| (d.range.start.line, d.range.start.character));
		out.insert(path.clone(), FileDiagnostics { version, list });
	}
	out
}

fn make(range: Range, msg: &str) -> Diagnostic {
	Diagnostic {
		range,
		severity: Some(DiagnosticSeverity::ERROR),
		code: None::<NumberOrString>,
		source: Some(SOURCE.to_string()),
		message: msg.to_string(),
		..Diagnostic::default()
	}
}
