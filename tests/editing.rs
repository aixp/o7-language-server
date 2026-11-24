//! Phase 6: completion, rename, semantic tokens.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use common::*;
use lsp_types::request::{Completion, PrepareRenameRequest, Rename, SemanticTokensFullRequest};
use lsp_types::{
	CompletionParams, CompletionResponse, Position, PrepareRenameResponse, RenameParams,
	SemanticTokensParams, SemanticTokensResult, TextDocumentIdentifier, Url,
};
use o7_front::diag::Diagnostics;
use o7_front::lexer::{Lexer, Tok};
use serde_json::json;

fn fixture() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nav")
}

fn text(name: &str) -> String {
	std::fs::read_to_string(fixture().join(name)).unwrap()
}

fn start(folder: &Path) -> Client {
	Client::start(Setup {
		folders: vec![folder.to_path_buf()],
		..Setup::default()
	})
}

fn complete(c: &mut Client, u: &Url, p: Position) -> Vec<String> {
	let r = c.request::<Completion>(CompletionParams {
		text_document_position: pos_params(u, p),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
		context: None,
	});
	match r {
		Some(CompletionResponse::Array(items)) => items.into_iter().map(|i| i.label).collect(),
		Some(CompletionResponse::List(l)) => l.items.into_iter().map(|i| i.label).collect(),
		None => Vec::new(),
	}
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
	v.sort();
	v
}

/// Opens `name` of the fixture with `insert` at the start of the line that
/// contains `before`, and returns the position after the inserted text.
fn open_with(c: &mut Client, name: &str, before: &str, insert: &str) -> (Url, Position) {
	let t = text(name);
	let at = t.find(before).unwrap();
	let at = t[..at].rfind('\n').map_or(0, |i| i + 1);
	let new = format!("{}{insert}\n{}", &t[..at], &t[at..]);
	let u = c.open_text(&fixture().join(name), &new);
	let end = at + insert.len();
	let line = new[..end].matches('\n').count() as u32;
	let col = end - new[..end].rfind('\n').map_or(0, |i| i + 1);
	(u, Position::new(line, col as u32))
}

#[test]
fn completion_after_a_module_name() {
	let mut c = start(&fixture());
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  L.");
	assert_eq!(
		sorted(complete(&mut c, &u, p)),
		["Add", "Base", "Max", "Node", "NodeDesc", "count"]
	);
	c.shutdown();
}

#[test]
fn completion_of_fields() {
	let mut c = start(&fixture());
	// A record of another module: inherited fields too, no private field.
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  r.");
	assert_eq!(complete(&mut c, &u, p), ["next", "value", "id"]);
	c.close(&u);
	// A pointer gives the fields of its record.
	let (u, p) = open_with(&mut c, "Main.Mod", "  END Show", "    ;n.");
	assert_eq!(complete(&mut c, &u, p), ["next", "value", "id"]);
	c.close(&u);
	// In the module of the record: the private field too.
	let (u, p) = open_with(&mut c, "Lib.Mod", "    RETURN count", "    n.");
	assert_eq!(complete(&mut c, &u, p), ["next", "value", "id", "hidden"]);
	c.shutdown();
}

#[test]
fn completion_in_a_statement() {
	let mut c = start(&fixture());
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  k := ");
	let items = complete(&mut c, &u, p);
	for want in [
		"head", "r", "k", "Show", "L", "Out", "INTEGER", "ABS", "CAS", "WHILE",
	] {
		assert!(
			items.iter().any(|i| i == want),
			"{want} missing in {items:?}"
		);
	}
	// Local names of a procedure are not visible in the module body.
	assert!(!items.iter().any(|i| i == "i"));
	c.shutdown();
}

#[test]
fn completion_in_a_nested_procedure() {
	let dir = temp_dir("nested");
	let t = "MODULE N;\n  VAR g: INTEGER;\n  PROCEDURE Outer(p: INTEGER);\n    VAR o: INTEGER;\n    \
		PROCEDURE Inner(q: INTEGER);\n      VAR i: INTEGER;\n    BEGIN i := \n    END Inner;\n  BEGIN o := p\n  \
		END Outer;\nEND N.\n";
	write_files(&dir, &[("N.Mod", t)]);
	let mut c = start(&dir);
	let u = c.open(&dir.join("N.Mod"));
	let items = complete(&mut c, &u, find(t, "i := ", 0, 5));
	for want in ["g", "i", "q", "Inner", "Outer"] {
		assert!(
			items.iter().any(|i| i == want),
			"{want} missing in {items:?}"
		);
	}
	// Variables of the enclosing procedure are not accessible.
	for no in ["o", "p"] {
		assert!(!items.iter().any(|i| i == no), "{no} in {items:?}");
	}
	c.shutdown();
}

#[test]
fn completion_after_a_syntax_error() {
	let mut c = start(&fixture());
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  k := 1 + ;\n  r.");
	assert_eq!(complete(&mut c, &u, p), ["next", "value", "id"]);
	c.close(&u);
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  k := ) ;\n  he");
	assert!(complete(&mut c, &u, p).iter().any(|i| i == "head"));
	c.close(&u);
	// After an error in the same statement.
	let (u, p) = open_with(&mut c, "Main.Mod", "  Show(head)", "  k := 1 + L.");
	assert!(complete(&mut c, &u, p).iter().any(|i| i == "Add"));
	c.shutdown();
}

#[test]
fn completion_of_system_and_none_in_comments() {
	let dir = temp_dir("system");
	let t = "MODULE S;\n  IMPORT SYSTEM;\n  VAR a: INTEGER;\nBEGIN\n  a := SYSTEM.\n  (* a := SYSTEM. *)\n  a := \"SY\"\nEND S.\n";
	write_files(&dir, &[("S.Mod", t)]);
	let mut c = start(&dir);
	let u = c.open(&dir.join("S.Mod"));
	let items = complete(&mut c, &u, find(t, "SYSTEM.", 0, 7));
	for want in [
		"ADR", "GET", "PUT", "BIT", "VAL", "SREG", "LDSREG", "SQRT", "WFI", "WFE", "DMB", "DSB",
		"ISB",
	] {
		assert!(
			items.iter().any(|i| i == want),
			"{want} missing in {items:?}"
		);
	}
	// `CAS` is a predeclared procedure, not a procedure of SYSTEM.
	assert!(!items.iter().any(|i| i == "CAS"), "{items:?}");
	assert!(complete(&mut c, &u, find(t, "SYSTEM.", 1, 7)).is_empty());
	assert!(complete(&mut c, &u, find(t, "SY\"", 0, 2)).is_empty());
	c.shutdown();
}

fn rename(
	c: &mut Client,
	u: &Url,
	p: Position,
	new: &str,
) -> Result<HashMap<String, Vec<(Position, String)>>, String> {
	let r = c.try_request::<Rename>(RenameParams {
		text_document_position: pos_params(u, p),
		new_name: new.to_string(),
		work_done_progress_params: Default::default(),
	});
	let edit = r.map_err(|e| e.message)?.expect("an edit");
	let mut out = HashMap::new();
	for (uri, edits) in edit.changes.unwrap_or_default() {
		let name = uri.path().rsplit('/').next().unwrap().to_string();
		let mut v: Vec<(Position, String)> = edits
			.into_iter()
			.map(|e| (e.range.start, e.new_text))
			.collect();
		v.sort();
		out.insert(name, v);
	}
	Ok(out)
}

fn count(m: &HashMap<String, Vec<(Position, String)>>) -> HashMap<String, usize> {
	m.iter().map(|(k, v)| (k.clone(), v.len())).collect()
}

fn counts(list: &[(&str, usize)]) -> HashMap<String, usize> {
	list.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn rename_local_variable_and_procedure() {
	let main = text("Main.Mod");
	let mut c = start(&fixture());
	let u = c.open(&fixture().join("Main.Mod"));
	c.sync(&u);
	let e = rename(&mut c, &u, find(&main, "i :=", 0, 0), "j").unwrap();
	assert_eq!(count(&e), counts(&[("Main.Mod", 3)]));
	assert!(e["Main.Mod"].iter().all(|x| x.1 == "j"));
	// A procedure: also the name after END.
	let e = rename(&mut c, &u, find(&main, "Show", 0, 0), "Display").unwrap();
	assert_eq!(count(&e), counts(&[("Main.Mod", 3)]));
	assert!(e["Main.Mod"].contains(&(find(&main, "END Show", 0, 4), "Display".to_string())));
	c.shutdown();
}

#[test]
fn rename_exported_procedure_and_field_with_importers() {
	let lib = text("Lib.Mod");
	let mut c = start(&fixture());
	let u = c.open(&fixture().join("Lib.Mod"));
	c.sync(&u);
	let e = rename(&mut c, &u, find(&lib, "Add*", 0, 0), "Insert").unwrap();
	assert_eq!(
		count(&e),
		counts(&[("Lib.Mod", 2), ("Main.Mod", 1), ("Other.Mod", 1)])
	);
	let e = rename(&mut c, &u, find(&lib, "value*", 0, 0), "val").unwrap();
	assert_eq!(count(&e), counts(&[("Lib.Mod", 2), ("Main.Mod", 1)]));
	// From a use in an importer: the same edit.
	let main = text("Main.Mod");
	let um = c.open(&fixture().join("Main.Mod"));
	c.sync(&um);
	let e2 = rename(&mut c, &um, find(&main, "n.value", 0, 2), "val").unwrap();
	assert_eq!(e2, e);
	c.shutdown();
}

#[test]
fn rename_that_changes_the_meaning_is_refused() {
	let dir = temp_dir("rename-check");
	let t = "MODULE H;\n  VAR a, b: INTEGER;\n  PROCEDURE P;\n    VAR x: INTEGER;\n  BEGIN x := a\n  END P;\n\
		BEGIN a := 1; b := 2\nEND H.\n";
	write_files(&dir, &[("H.Mod", t)]);
	let mut c = start(&dir);
	let u = c.open(&dir.join("H.Mod"));
	assert!(c.sync(&u).is_empty());
	// The local x would hide the global a that P uses.
	let e = rename(&mut c, &u, find(t, "x: INTEGER", 0, 0), "a").unwrap_err();
	assert!(e.contains("meaning"), "{e}");
	// The global a would be hidden by the local x in P.
	let e = rename(&mut c, &u, find(t, "a, b", 0, 0), "x").unwrap_err();
	assert!(e.contains("meaning"), "{e}");
	// Two declarations of a.
	let e = rename(&mut c, &u, find(t, "b: INTEGER", 0, 0), "a").unwrap_err();
	assert!(e.contains("error"), "{e}");
	// Reserved word, not an identifier.
	let e = rename(&mut c, &u, find(t, "b: INTEGER", 0, 0), "END").unwrap_err();
	assert!(e.contains("reserved word"), "{e}");
	let e = rename(&mut c, &u, find(t, "b: INTEGER", 0, 0), "b_1").unwrap_err();
	assert!(e.contains("not an identifier"), "{e}");
	// A correct rename still works.
	assert_eq!(
		count(&rename(&mut c, &u, find(t, "b: INTEGER", 0, 0), "c").unwrap()),
		counts(&[("H.Mod", 2)])
	);
	c.shutdown();
}

#[test]
fn prepare_rename() {
	let main = text("Main.Mod");
	let mut c = start(&fixture());
	let u = c.open(&fixture().join("Main.Mod"));
	c.sync(&u);
	let prep = |c: &mut Client, p: Position| {
		c.try_request::<PrepareRenameRequest>(pos_params(&u, p))
			.map_err(|e| e.message)
			.map(|r| match r {
				Some(PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. }) => {
					placeholder
				}
				other => panic!("{other:?}"),
			})
	};
	assert_eq!(
		prep(&mut c, find(&main, "head", 0, 0)),
		Ok("head".to_string())
	);
	// An alias can be renamed.
	assert_eq!(prep(&mut c, find(&main, "L :=", 0, 0)), Ok("L".to_string()));
	// Not: predeclared names, module names, an import without an alias.
	assert!(
		prep(&mut c, find(&main, "INTEGER", 0, 0))
			.unwrap_err()
			.contains("predeclared")
	);
	assert!(
		prep(&mut c, find(&main, "Main", 0, 0))
			.unwrap_err()
			.contains("module")
	);
	assert!(
		prep(&mut c, find(&main, "Out", 0, 0))
			.unwrap_err()
			.contains("alias")
	);
	c.shutdown();
}

/// The absolute positions, types and modifiers of the semantic tokens.
fn tokens(c: &mut Client, u: &Url) -> Vec<(u32, u32, u32, u32, u32)> {
	let r = c.request::<SemanticTokensFullRequest>(SemanticTokensParams {
		text_document: TextDocumentIdentifier::new(u.clone()),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	let Some(SemanticTokensResult::Tokens(t)) = r else {
		panic!("{r:?}")
	};
	let (mut line, mut col) = (0, 0);
	t.data
		.iter()
		.map(|d| {
			if d.delta_line > 0 {
				line += d.delta_line;
				col = d.delta_start;
			} else {
				col += d.delta_start;
			}
			(line, col, d.length, d.token_type, d.token_modifiers_bitset)
		})
		.collect()
}

#[test]
fn semantic_token_kinds() {
	let main = text("Main.Mod");
	let mut c = start(&fixture());
	let u = c.open(&fixture().join("Main.Mod"));
	c.sync(&u);
	let toks = tokens(&mut c, &u);
	let at = |p: Position| {
		toks.iter()
			.find(|t| t.0 == p.line && t.1 == p.character)
			.map(|t| (t.3, t.4))
			.unwrap_or_else(|| panic!("no token at {p:?}"))
	};
	// Types: namespace 0, type 1, variable 2, parameter 3, property 4,
	// function 5. Modifiers: declaration 1, readonly 2, defaultLibrary 4.
	assert_eq!(at(find(&main, "L :=", 0, 0)), (0, 1));
	assert_eq!(at(find(&main, "Node;", 0, 0)), (1, 0));
	assert_eq!(at(find(&main, "head", 0, 0)), (2, 1));
	assert_eq!(at(find(&main, "Max", 0, 0)), (2, 2));
	assert_eq!(at(find(&main, "INTEGER", 0, 0)), (1, 4));
	assert_eq!(at(find(&main, "Add", 0, 0)), (5, 0));
	assert_eq!(at(find(&main, "n.value", 0, 2)), (4, 0));
	assert_eq!(at(find(&main, "n: L", 0, 0)), (3, 1));
	assert_eq!(at(find(&main, "INC", 0, 0)), (5, 4));
	c.shutdown();
}

#[test]
fn one_semantic_token_for_each_identifier() {
	let Some(repo) = compiler_repo() else {
		eprintln!("skipped: the compiler repository is not available");
		return;
	};
	let mut c = Client::start(Setup {
		options: json!({ "searchPaths": compiler_search_paths(&repo) }),
		debounce: std::time::Duration::ZERO,
		..Setup::default()
	});
	let mut files = 0;
	for dir in [
		"tests/exec",
		"tests/codegen",
		"tests/index",
		"tests/lib",
		"lib/Micro",
		"lib/Micro/hsebyp",
		"examples",
	] {
		let mut list: Vec<PathBuf> = std::fs::read_dir(repo.join(dir))
			.unwrap()
			.map(|e| e.unwrap().path())
			.filter(|p| p.extension().is_some_and(|x| x == "Mod"))
			.collect();
		list.sort();
		for p in list {
			let u = c.open(&p);
			if !c.sync(&u).is_empty() {
				c.close(&u);
				continue;
			}
			let src = std::fs::read(&p).unwrap();
			let mut idents = Vec::new();
			let mut d = Diagnostics::new("");
			let mut lx = Lexer::new(&src);
			loop {
				let (t, pos) = lx.next(&mut d);
				match t {
					Tok::Eof => break,
					Tok::Ident(_) => idents.push((pos.line - 1, pos.col - 1)),
					_ => {}
				}
			}
			// The files are ASCII: the column in bytes is the UTF-16 column.
			let toks: Vec<(u32, u32)> = tokens(&mut c, &u).iter().map(|t| (t.0, t.1)).collect();
			assert_eq!(toks, idents, "{}", p.display());
			files += 1;
			c.close(&u);
		}
	}
	eprintln!("{files} files");
	assert!(files > 100);
	c.shutdown();
}
