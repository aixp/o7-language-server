//! Diagnostics: the messages of the front end, of `project::collect` and of
//! the project file.
//!
//! Diagnostics are published for the open documents and for the files in
//! their import closure. The range of a message is the lexical symbol at its
//! position (`Index::diagnostic_span` for the messages of an analysis,
//! `lexer::token_span` for the messages of `project::collect`). The severity
//! is always "error": the compiler has no warnings.
//!
//! A module can have more than one context (`crate::workspace`): programs
//! for different targets import it. Its diagnostics are those of all its
//! contexts. A message that all contexts give is shown as it is. A message
//! that only some contexts give gets the names of these contexts at its end,
//! for example `(program rv, board esp32h2)`.
//!
//! The errors of a project file `o7.toml` are published for that file.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};
use o7_front::diag::{self, Pos, Span};
use o7_front::lexer;
use o7_front::project;
use o7_front::target_info::TargetProps;

use crate::vfs::{Encoding, LineIndex, normalize};
use crate::workspace::{Context, Workspace, is_project_file};

pub const SOURCE: &str = "o7";

/// The diagnostics of one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileDiagnostics {
	/// The version of the open document that was analysed.
	pub version: Option<i32>,
	pub list: Vec<Diagnostic>,
}

/// The messages of one file while they are collected.
#[derive(Default)]
struct FileState {
	version: Option<i32>,
	/// Messages that do not come from an analysis (no target).
	plain: Vec<Diagnostic>,
	/// The contexts that contain the file: the context, its name for a
	/// message, and its messages for the file.
	contexts: Vec<(Context, String, Vec<Diagnostic>)>,
}

/// Brings the analyses of all open documents up to date, in all their
/// contexts, and returns the diagnostics of all files in their closures and
/// of their project files.
pub fn collect(ws: &mut Workspace, enc: Encoding) -> BTreeMap<PathBuf, FileDiagnostics> {
	let roots: Vec<PathBuf> = ws
		.docs
		.keys()
		.filter(|p| !is_project_file(p))
		.cloned()
		.collect();
	let mut files: BTreeMap<PathBuf, FileState> = BTreeMap::new();
	let mut project_files: BTreeSet<PathBuf> = BTreeSet::new();
	for root in &roots {
		project_files.extend(ws.project_file_of(root));
		let contexts = match ws.contexts_of(root) {
			Ok(list) => list,
			Err(msg) => {
				// No analysis: one message for the current text.
				let state = files.entry(root.clone()).or_default();
				state.version = ws.docs.get(root).map(|d| d.version);
				state.plain.push(make(Range::default(), &msg));
				continue;
			}
		};
		for ctx in contexts {
			let info = ws.check_root(&ctx, root);
			let mut problems: BTreeMap<PathBuf, Vec<diag::Diagnostic>> = BTreeMap::new();
			for d in info.problems {
				let list = problems.entry(normalize(Path::new(&d.file))).or_default();
				if !list.contains(&d) {
					list.push(d);
				}
			}
			let mut paths: BTreeSet<PathBuf> = info.order.into_iter().collect();
			paths.extend(problems.keys().cloned());
			// The messages are read now: a later context with the same
			// target can replace the analyses of these files.
			for path in paths {
				let probs = problems.get(&path).map(Vec::as_slice).unwrap_or(&[]);
				let (version, list) = messages(ws, ctx.target, &path, probs, enc);
				let state = files.entry(path).or_default();
				state.version = version;
				if !state.contexts.iter().any(|c| c.0.same_analysis(&ctx)) {
					state.contexts.push((ctx.clone(), ctx.label(root), list));
				}
			}
		}
	}
	let mut out = BTreeMap::new();
	for (path, state) in files {
		let version = state.version;
		let mut list = merge(state);
		list.sort_by_key(|d| (d.range.start.line, d.range.start.character));
		out.insert(path, FileDiagnostics { version, list });
	}
	for file in project_files {
		let list = project_file_messages(ws, &file, enc);
		if !list.is_empty() {
			let version = ws.docs.get(&file).map(|d| d.version);
			out.insert(file, FileDiagnostics { version, list });
		}
	}
	out
}

/// The messages of all contexts of a file. A message that not all contexts
/// give gets the names of its contexts.
fn merge(state: FileState) -> Vec<Diagnostic> {
	let mut out = state.plain;
	let same = |a: &Diagnostic, b: &Diagnostic| a.range == b.range && a.message == b.message;
	let count = state.contexts.len();
	for (i, (_, _, list)) in state.contexts.iter().enumerate() {
		for (k, d) in list.iter().enumerate() {
			// Each message one time: for the first context that gives it.
			let holders: Vec<usize> = (0..count)
				.filter(|&j| state.contexts[j].2.iter().any(|e| same(d, e)))
				.collect();
			if holders[0] != i || list[..k].iter().any(|e| same(d, e)) {
				continue;
			}
			let mut d = d.clone();
			if holders.len() < count {
				let names: Vec<&str> = holders
					.iter()
					.map(|&j| state.contexts[j].1.as_str())
					.collect();
				d.message = format!("{} ({})", d.message, names.join("; "));
			}
			out.push(d);
		}
	}
	out
}

/// The messages of the file `path` in the analysis for `target`, and the
/// messages `probs` of `project::collect` for this file. The first result
/// is the version of the open document, if the messages are for its text.
fn messages(
	ws: &Workspace,
	target: &TargetProps,
	path: &Path,
	probs: &[diag::Diagnostic],
	enc: Encoding,
) -> (Option<i32>, Vec<Diagnostic>) {
	let mut list = Vec::new();
	let text = ws.text(path);
	let lines = text.as_deref().map(LineIndex::new);
	let mut version = None;
	if let Some(u) = ws.unit(target, path) {
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
	} else if let Some(doc) = ws.docs.get(path) {
		// No analysis: the messages are those of `project::collect` for the
		// current text.
		version = Some(doc.version);
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
	(version, list)
}

/// The messages for the project file `file`: its errors; if it has none, a
/// message for each root module that is not a file.
fn project_file_messages(ws: &Workspace, file: &Path, enc: Encoding) -> Vec<Diagnostic> {
	let Some(text) = ws.text(file) else {
		return Vec::new();
	};
	let lines = LineIndex::new(&text);
	let at = |pos: Pos, msg: &str| {
		let range = if pos.line == 0 {
			Range::default()
		} else {
			lines.lsp_range(&text, word_span(&text, &lines, pos), enc)
		};
		make(range, msg)
	};
	match ws.project(file) {
		Some(Err(errors)) => errors.iter().map(|d| at(d.pos, &d.msg)).collect(),
		Some(Ok(project)) => project
			.programs
			.iter()
			.filter(|p| !project::SourceHost::is_file(ws, &p.root))
			.map(|p| {
				let msg = format!(
					"the root module of the program {} is not a file: {}",
					p.name,
					p.root.display()
				);
				at(p.root_pos, &msg)
			})
			.collect(),
		None => Vec::new(),
	}
}

/// The range of the item at `pos` in the text of a project file: a string
/// with its quotes, a header, or the characters to the next separator.
fn word_span(text: &[u8], lines: &LineIndex, pos: Pos) -> Span {
	let start = lines.offset(pos).min(text.len());
	let rest = &text[start..];
	let line = &rest[..rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len())];
	let len = match line.first() {
		Some(&q @ (b'"' | b'\'')) => line[1..]
			.iter()
			.position(|&c| c == q)
			.map_or(line.len(), |i| i + 2),
		Some(b'[') => line
			.iter()
			.position(|&c| c == b']')
			.map_or(1, |i| i + 1 + usize::from(line.get(i + 1) == Some(&b']'))),
		_ => line
			.iter()
			.position(|&c| matches!(c, b' ' | b'\t' | b'\r' | b'=' | b',' | b']' | b'#'))
			.unwrap_or(line.len()),
	};
	Span::at(pos, len.max(1))
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
