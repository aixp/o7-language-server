//! Phase 3: initialization, diagnostics of one module, incremental
//! synchronization, positions, speed.

mod common;

use std::time::{Duration, Instant};

use common::*;
use lsp_types::{Position, Range};
use serde_json::json;

#[test]
fn initialize_gives_the_versions_and_the_capabilities() {
	let c = Client::start(Setup::default());
	let info = c.init.server_info.clone().expect("serverInfo");
	assert_eq!(info.name, "o7-language-server");
	let v = info.version.unwrap();
	assert!(v.contains("o7-front 0.6.0"), "{v}");
	assert!(
		v.contains("fe5d144204d543fd92ce04177c216e2665910861"),
		"{v}"
	);
	let caps = &c.init.capabilities;
	assert!(caps.definition_provider.is_some());
	assert!(caps.semantic_tokens_provider.is_some());
	// The client did not offer UTF-8.
	assert_eq!(
		caps.position_encoding,
		Some(lsp_types::PositionEncodingKind::UTF16)
	);
	c.shutdown();
}

#[test]
fn utf8_positions_if_the_client_offers_them() {
	let c = Client::start(Setup {
		utf8: true,
		..Setup::default()
	});
	assert_eq!(
		c.init.capabilities.position_encoding,
		Some(lsp_types::PositionEncodingKind::UTF8)
	);
	c.shutdown();
}

#[test]
fn errors_of_the_procedures_of_system() {
	// The front end checks the procedures of SYSTEM without LLVM, so the
	// server shows the same errors as the compiler.
	let dir = temp_dir("system-procs");
	let text = "MODULE M;\n  IMPORT SYSTEM;\n  VAR i: INTEGER; x: REAL;\nBEGIN\n  \
		i := SYSTEM.SREG(18);\n  SYSTEM.LDSREG(20, i);\n  \
		SYSTEM.LDSREG(i, 1);\n  x := SYSTEM.SQRT(i);\n  \
		SYSTEM.WFI; i := SYSTEM.SREG(16); SYSTEM.LDSREG(16, 1); x := SYSTEM.SQRT(x)\nEND M.\n";
	write_files(&dir, &[("M.Mod", text)]);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("M.Mod"));
	let d = c.sync(&u);
	let t = texts(&d);
	assert_eq!(d.len(), 4, "{t:?}");
	let at = |line: u32, part: &str| {
		assert!(
			d.iter()
				.any(|d| d.range.start.line == line && d.message.contains(part)),
			"line {line}: {part:?} not in {t:?}"
		);
	};
	at(4, "special register 18 (BASEPRI_MAX) cannot be read");
	at(5, "special register 20 (CONTROL) cannot be written");
	at(6, "SYSTEM.LDSREG needs a constant register number");
	at(7, "REAL expression expected");
	c.shutdown();
}

#[test]
fn errors_of_cas() {
	// The front end checks the predeclared procedure CAS without LLVM.
	let dir = temp_dir("cas");
	let text = "MODULE M;\n  IMPORT SYSTEM;\n  VAR i: INTEGER; x: REAL; b: BOOLEAN;\nBEGIN\n  \
		i := CAS(i, 0);\n  x := CAS(x, 0.0, 1.0);\n  \
		CAS(i, 0, 1);\n  i := CAS(i, 0, TRUE);\n  \
		i := SYSTEM.CAS(i, 0, 1);\n  \
		i := CAS(i, i, i + 1); b := CAS(b, FALSE, TRUE)\nEND M.\n";
	write_files(&dir, &[("M.Mod", text)]);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("M.Mod"));
	let d = c.sync(&u);
	let t = texts(&d);
	assert_eq!(d.len(), 5, "{t:?}");
	let at = |line: u32, part: &str| {
		assert!(
			d.iter()
				.any(|d| d.range.start.line == line && d.message.contains(part)),
			"line {line}: {part:?} not in {t:?}"
		);
	};
	at(4, "this function needs 3 parameter(s), not 2");
	at(5, "the variable of CAS must have the type INTEGER");
	at(6, "a function cannot be called as a statement");
	at(
		7,
		"a value of type BOOLEAN cannot be assigned to a variable of type INTEGER",
	);
	at(8, "`CAS` is not exported by module SYSTEM");
	c.shutdown();
}

#[test]
fn errors_of_one_module() {
	let dir = temp_dir("one-module");
	let text = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := TRUE;\n  y := 1\nEND M.\n";
	write_files(&dir, &[("M.Mod", text)]);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("M.Mod"));
	let d = c.sync(&u);
	assert_eq!(d.len(), 2, "{:?}", texts(&d));
	// The range is the symbol at the position of the message.
	let y = d
		.iter()
		.find(|d| d.range.start.line == 4)
		.expect("message about y");
	assert_eq!(
		y.range,
		Range::new(Position::new(4, 2), Position::new(4, 3))
	);
	assert_eq!(y.severity, Some(lsp_types::DiagnosticSeverity::ERROR));
	assert_eq!(y.source.as_deref(), Some("o7"));
	// Repair the text: the messages go away.
	c.change(
		&u,
		None,
		"MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := 1\nEND M.\n",
	);
	assert!(c.sync(&u).is_empty());
	// Closing the document clears its diagnostics.
	c.change(&u, None, "MODULE M; BEGIN z END M.");
	assert_eq!(c.sync(&u).len(), 1);
	c.close(&u);
	c.wait_diags(&u, |d| d.is_empty());
	c.shutdown();
}

/// Builds `text` by small edits: insertions of short pieces at the end,
/// some characters typed in the middle, wrong text that is removed again.
fn edits_for(text: &str) -> Vec<(Range, String)> {
	let mut out = Vec::new();
	let mut cur = String::new();
	let pos_of = |s: &str, at: usize| {
		let line = s[..at].matches('\n').count() as u32;
		let col = s[..at].rfind('\n').map_or(at, |i| at - i - 1);
		// UTF-16 units of the line part (the test texts are ASCII up to here).
		let col16 = s[at - col..at].encode_utf16().count() as u32;
		Position::new(line, col16)
	};
	let mut i = 0;
	let bytes = text.as_bytes();
	let mut k = 0;
	while i < text.len() {
		// Pieces of 1 to 40 bytes, at a character boundary.
		let mut n = (k * 7 % 40) + 1;
		while i + n < text.len() && !text.is_char_boundary(i + n) {
			n += 1;
		}
		let n = n.min(text.len() - i);
		let piece = &text[i..i + n];
		let at = cur.len();
		let p = pos_of(&cur, at);
		if k % 5 == 3 && bytes[i] != b'\n' {
			// Type a wrong word, then remove it.
			out.push((Range::new(p, p), "WRONG(*".to_string()));
			let end = Position::new(p.line, p.character + 7);
			out.push((Range::new(p, end), String::new()));
		}
		out.push((Range::new(p, p), piece.to_string()));
		cur.push_str(piece);
		i += n;
		k += 1;
	}
	assert_eq!(cur, text);
	out
}

#[test]
fn small_edits_give_the_same_diagnostics_as_the_final_text() {
	let dir = temp_dir("incremental");
	let main = "MODULE Main;\n  IMPORT Lib;\n  (* Kommentar: \u{e4}\u{f6}\u{fc} \u{1F600} *) VAR v: Lib.T;\n  \
		PROCEDURE P(VAR t: Lib.T): INTEGER;\n  BEGIN RETURN t.a + Lib.N\n  END P;\nBEGIN\n  v.a := P(v); v.b := 1; \
		undeclared := 2\nEND Main.\n";
	let lib = "MODULE Lib;\n  CONST N* = 3;\n  TYPE T* = RECORD a*: INTEGER END;\nEND Lib.\n";
	write_files(&dir, &[("Lib.Mod", lib), ("Main.Mod", "")]);
	let path = dir.join("Main.Mod");

	// Fresh server, final text at once.
	let mut fresh = Client::start(Setup::default());
	let u = fresh.open_text(&path, main);
	let expected = texts(&fresh.sync(&u));
	fresh.shutdown();
	assert_eq!(expected.len(), 2, "{expected:?}");

	// Small edits; each edit is one message, and some messages have
	// several changes.
	let mut c = Client::start(Setup::default());
	let u = c.open_text(&path, "");
	let edits = edits_for(main);
	for chunk in edits.chunks(3) {
		c.changes(
			&u,
			chunk.iter().map(|(r, t)| (Some(*r), t.clone())).collect(),
		);
	}
	assert_eq!(texts(&c.sync(&u)), expected);
	c.shutdown();
}

#[test]
fn positions_with_crlf_and_utf8_comments() {
	let dir = temp_dir("crlf");
	// Line 2: a comment with non-ASCII characters before the error.
	let text = "MODULE M;\r\n(* \u{e4}\u{1F600} *) VAR x: UNKNOWN;\r\nEND M.\r\n";
	write_files(&dir, &[("M.Mod", text)]);
	let col_bytes = text.lines().nth(1).unwrap().find("UNKNOWN").unwrap() as u32;
	// "(* ä😀 *) VAR x: " : ä is 1 unit, 😀 is 2 units.
	let col_utf16 = "(* \u{e4}\u{1F600} *) VAR x: ".encode_utf16().count() as u32;
	for (utf8, col) in [(false, col_utf16), (true, col_bytes)] {
		let mut c = Client::start(Setup {
			utf8,
			..Setup::default()
		});
		let u = c.open(&dir.join("M.Mod"));
		let d = c.sync(&u);
		assert_eq!(d.len(), 1, "{:?}", texts(&d));
		assert_eq!(
			d[0].range,
			Range::new(Position::new(1, col), Position::new(1, col + 7)),
			"utf8 = {utf8}"
		);
		c.shutdown();
	}
}

#[test]
fn byte_order_mark() {
	let dir = temp_dir("bom");
	let text = "\u{feff}MODULE M; BEGIN x END M.";
	write_files(&dir, &[("M.Mod", text)]);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("M.Mod"));
	let d = c.sync(&u);
	assert_eq!(texts(&d), ["0:16: `x` is not declared"]);
	c.shutdown();
}

#[test]
fn diagnostics_of_a_change_in_a_large_module_are_fast() {
	let Some(repo) = compiler_repo() else {
		eprintln!("skipped: the compiler repository is not available");
		return;
	};
	let path = repo.join("lib/Micro/MicroSTM32F4.Mod");
	let text = std::fs::read_to_string(&path).unwrap();
	let debounce = Duration::from_millis(50);
	let mut c = Client::start(Setup {
		options: json!({ "searchPaths": compiler_search_paths(&repo) }),
		debounce,
		..Setup::default()
	});
	let u = c.open(&path);
	assert!(c.sync(&u).is_empty());
	// The position of the last `END`: a body change.
	let lines: Vec<&str> = text.lines().collect();
	let line = lines.iter().rposition(|l| l.starts_with("END")).unwrap() as u32;
	let mut best = Duration::MAX;
	for i in 0..10 {
		let p = Position::new(line, 0);
		let ins = if i % 2 == 0 { " " } else { "" };
		let start = Instant::now();
		if ins.is_empty() {
			c.change(&u, Some(Range::new(p, Position::new(line, 1))), "");
		} else {
			c.change(&u, Some(Range::new(p, p)), ins);
		}
		c.sync(&u);
		let t = start.elapsed().saturating_sub(debounce);
		best = best.min(t);
	}
	eprintln!("diagnostics after the pause: {best:?}");
	assert!(best < Duration::from_millis(10), "{best:?}");
	c.shutdown();
}
