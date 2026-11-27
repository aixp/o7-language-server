//! Phase 5: definition, hover, document symbols, references, workspace
//! symbols. The fixture `tests/fixtures/nav` compiles with `o7c`.

mod common;

use std::path::PathBuf;

use common::*;
use lsp_types::request::{
	DocumentSymbolRequest, GotoDefinition, HoverRequest, References, WorkspaceSymbolRequest,
};
use lsp_types::{
	DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
	GotoDefinitionResponse, HoverContents, HoverParams, Location, Position, ReferenceContext,
	ReferenceParams, TextDocumentIdentifier, Url, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use serde_json::json;

pub fn fixture() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nav")
}

fn text(name: &str) -> String {
	std::fs::read_to_string(fixture().join(name)).unwrap()
}

fn start() -> Client {
	Client::start(Setup {
		folders: vec![fixture()],
		..Setup::default()
	})
}

fn definition(c: &mut Client, u: &Url, p: Position) -> Option<Location> {
	let r = c.request::<GotoDefinition>(GotoDefinitionParams {
		text_document_position_params: pos_params(u, p),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	match r {
		None => None,
		Some(GotoDefinitionResponse::Scalar(l)) => Some(l),
		Some(other) => panic!("unexpected response {other:?}"),
	}
}

fn at(file: &str, p: Position) -> (String, Position) {
	(file.to_string(), p)
}

fn short(l: Option<Location>) -> Option<(String, Position)> {
	l.map(|l| {
		let name = l.uri.path().rsplit('/').next().unwrap().to_string();
		(name, l.range.start)
	})
}

#[test]
fn definition_in_one_module_and_across_modules() {
	let main = text("Main.Mod");
	let lib = text("Lib.Mod");
	let mut c = start();
	let u = c.open(&fixture().join("Main.Mod"));
	assert!(c.sync(&u).is_empty(), "{:?}", c.last_diags(&u));
	let cases = [
		// Local variable.
		(
			find(&main, "head", 1, 0),
			Some(at("Main.Mod", find(&main, "head", 0, 0))),
		),
		// Imported procedure.
		(
			find(&main, "Add", 0, 1),
			Some(at("Lib.Mod", find(&lib, "Add*", 0, 0))),
		),
		// Import alias: the module.
		(
			find(&main, "L.Add", 0, 0),
			Some(at("Lib.Mod", Position::new(0, 7))),
		),
		// The module name after `:=`.
		(
			find(&main, "Lib", 0, 1),
			Some(at("Lib.Mod", Position::new(0, 7))),
		),
		// Imported type.
		(
			find(&main, "NodeDesc", 0, 0),
			Some(at("Lib.Mod", find(&lib, "NodeDesc*", 0, 0))),
		),
		// Field of an imported record.
		(
			find(&main, "n.value", 0, 2),
			Some(at("Lib.Mod", find(&lib, "value*", 0, 0))),
		),
		// Field of the base record of an imported record.
		(
			find(&main, "r.id", 0, 2),
			Some(at("Lib.Mod", find(&lib, "id*", 0, 0))),
		),
		// Procedure of this module, and its name after END.
		(
			find(&main, "Show(head)", 0, 0),
			Some(at("Main.Mod", find(&main, "Show", 0, 0))),
		),
		(
			find(&main, "END Show", 0, 4),
			Some(at("Main.Mod", find(&main, "Show", 0, 0))),
		),
		// Predeclared name: no location.
		(find(&main, "INTEGER", 0, 0), None),
	];
	for (p, want) in cases {
		assert_eq!(short(definition(&mut c, &u, p)), want, "at {p:?}");
	}
	// In Lib: the forward reference of a pointer type.
	let ul = c.open(&fixture().join("Lib.Mod"));
	let p = find(&lib, "NodeDesc;", 0, 0);
	assert_eq!(
		short(definition(&mut c, &ul, p)),
		Some(at("Lib.Mod", find(&lib, "NodeDesc*", 0, 0)))
	);
	c.shutdown();
}

fn hover_text(c: &mut Client, u: &Url, p: Position) -> Option<String> {
	let h = c.request::<HoverRequest>(HoverParams {
		text_document_position_params: pos_params(u, p),
		work_done_progress_params: Default::default(),
	})?;
	match h.contents {
		HoverContents::Markup(m) => Some(m.value),
		other => panic!("unexpected hover {other:?}"),
	}
}

#[test]
fn hover() {
	let main = text("Main.Mod");
	let mut c = start();
	let u = c.open(&fixture().join("Main.Mod"));
	c.sync(&u);
	// An imported procedure: the parameter names come from the index of Lib.
	let h = hover_text(&mut c, &u, find(&main, "Add", 0, 1)).unwrap();
	assert!(
		h.contains("PROCEDURE Add*(VAR list: Node; value: INTEGER): INTEGER"),
		"{h}"
	);
	assert!(h.contains("Module `Lib`"), "{h}");
	// A constant shows its value.
	let h = hover_text(&mut c, &u, find(&main, "Max", 0, 0)).unwrap();
	assert!(h.contains("CONST Max* = 10"), "{h}");
	// A local variable.
	let h = hover_text(&mut c, &u, find(&main, "i :=", 0, 0)).unwrap();
	assert!(h.contains("VAR i: INTEGER"), "{h}");
	// Not an identifier: the type of the expression.
	let h = hover_text(&mut c, &u, find(&main, "# NIL", 0, 0)).unwrap();
	assert!(h.contains("BOOLEAN"), "{h}");
	// A keyword outside of an expression: nothing.
	assert_eq!(hover_text(&mut c, &u, Position::new(0, 2)), None);
	c.shutdown();
}

fn names(list: &[DocumentSymbol]) -> Vec<String> {
	list.iter()
		.map(|s| match &s.children {
			Some(c) if !c.is_empty() => format!("{}{:?}", s.name, names(c)),
			_ => s.name.clone(),
		})
		.collect()
}

#[test]
fn document_symbols() {
	let mut c = start();
	let u = c.open(&fixture().join("Lib.Mod"));
	c.sync(&u);
	let r = c.request::<DocumentSymbolRequest>(DocumentSymbolParams {
		text_document: TextDocumentIdentifier::new(u.clone()),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	let Some(DocumentSymbolResponse::Nested(list)) = r else {
		panic!("{r:?}")
	};
	assert_eq!(
		names(&list),
		[
			r#"Lib["Max*", "Base*[\"id*\", \"hidden\"]", "Node*", "NodeDesc*[\"next*\", \"value*\"]", "count*", "Add*[\"n\"]"]"#
		]
	);
	// The selection range is inside the range.
	fn check(list: &[DocumentSymbol]) {
		for s in list {
			assert!(
				s.range.start <= s.selection_range.start && s.selection_range.end <= s.range.end,
				"{s:?}"
			);
			check(s.children.as_deref().unwrap_or(&[]));
		}
	}
	check(&list);
	let add = &list[0].children.as_ref().unwrap()[5];
	assert_eq!(
		add.detail.as_deref(),
		Some("(VAR list: Node; value: INTEGER): INTEGER")
	);
	c.shutdown();
}

fn references(c: &mut Client, u: &Url, p: Position, decl: bool) -> Vec<(String, Position)> {
	let r = c.request::<References>(ReferenceParams {
		text_document_position: pos_params(u, p),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
		context: ReferenceContext {
			include_declaration: decl,
		},
	});
	let mut v: Vec<(String, Position)> = r
		.unwrap_or_default()
		.into_iter()
		.map(|l| short(Some(l)).unwrap())
		.collect();
	v.sort();
	v
}

#[test]
fn references_across_modules() {
	let lib = text("Lib.Mod");
	let main = text("Main.Mod");
	let other = text("Other.Mod");
	let mut c = start();
	// Only Lib is open: Main and Other come from the workspace index.
	let u = c.open(&fixture().join("Lib.Mod"));
	c.sync(&u);
	let mut want = vec![
		at("Lib.Mod", find(&lib, "Add*", 0, 0)),
		at("Lib.Mod", find(&lib, "END Add", 0, 4)),
		at("Main.Mod", find(&main, "Add", 0, 0)),
		at("Other.Mod", find(&other, "Add", 0, 0)),
	];
	want.sort();
	assert_eq!(references(&mut c, &u, find(&lib, "Add*", 0, 0), true), want);
	// Without the declaration.
	let without: Vec<_> = want
		.iter()
		.filter(|w| w.1 != find(&lib, "Add*", 0, 0))
		.cloned()
		.collect();
	assert_eq!(
		references(&mut c, &u, find(&lib, "Add*", 0, 0), false),
		without
	);
	// An exported field: through the type map of the symbol file.
	let mut want = vec![
		at("Lib.Mod", find(&lib, "value*", 0, 0)),
		at("Lib.Mod", find(&lib, "n.value", 0, 2)),
		at("Main.Mod", find(&main, "n.value", 0, 2)),
	];
	want.sort();
	assert_eq!(
		references(&mut c, &u, find(&lib, "value*", 0, 0), true),
		want
	);
	// A field of the base record, used through the extension in Main.
	let got = references(&mut c, &u, find(&lib, "id*", 0, 0), true);
	assert_eq!(got.len(), 2, "{got:?}");
	// A local variable: only in its procedure.
	let got = references(&mut c, &u, find(&lib, "n: Node", 0, 0), true);
	assert_eq!(got.len(), 6, "{got:?}");
	assert!(got.iter().all(|g| g.0 == "Lib.Mod"));
	// From a use in another open module.
	let um = c.open(&fixture().join("Main.Mod"));
	c.sync(&um);
	let got = references(&mut c, &um, find(&main, "Add", 0, 1), true);
	assert_eq!(got.len(), 4, "{got:?}");
	c.shutdown();
}

#[test]
fn workspace_symbols() {
	let mut c = start();
	let r = c.request::<WorkspaceSymbolRequest>(WorkspaceSymbolParams {
		query: "nodeD".to_string(),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	let Some(WorkspaceSymbolResponse::Flat(list)) = r else {
		panic!("{r:?}")
	};
	let found: Vec<(String, Option<String>)> = list
		.iter()
		.map(|s| (s.name.clone(), s.container_name.clone()))
		.collect();
	assert_eq!(found, [("NodeDesc".to_string(), Some("Lib".to_string()))]);
	let r = c.request::<WorkspaceSymbolRequest>(WorkspaceSymbolParams {
		query: String::new(),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	let Some(WorkspaceSymbolResponse::Flat(list)) = r else {
		panic!("{r:?}")
	};
	// All module-level symbols of the four modules, and the modules.
	assert!(list.iter().any(|s| s.name == "Show"));
	assert!(list.iter().any(|s| s.name == "Other"));
	assert!(!list.iter().any(|s| s.name == "n"), "no local symbols");
	c.shutdown();
}

#[test]
fn references_of_out_include_files_that_are_not_open() {
	let Some(repo) = compiler_repo() else {
		eprintln!("skipped: the compiler repository is not available");
		return;
	};
	// The workspace folders are `tests` and `lib`, not the root of the
	// repository: the index then does not depend on the files that are not
	// in the repository (for example the output of a build). A relative
	// search path starts at the first workspace folder, thus the search
	// paths are absolute.
	let search: Vec<String> = ["tests/lib/stm32f4", "lib", "lib/Micro", "lib/Micro/hsebyp"]
		.iter()
		.map(|d| repo.join(d).display().to_string())
		.collect();
	let mut c = Client::start(Setup {
		folders: vec![repo.join("tests"), repo.join("lib")],
		options: json!({ "searchPaths": search }),
		..Setup::default()
	});
	let out = repo.join("tests/lib/stm32f4/Out.Mod");
	let text = std::fs::read_to_string(&out).unwrap();
	let u = c.open(&out);
	c.sync(&u);
	let r = c.request::<References>(ReferenceParams {
		text_document_position: pos_params(&u, find(&text, "String*", 0, 0)),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
		context: ReferenceContext {
			include_declaration: true,
		},
	});
	let list = r.unwrap_or_default();
	let exec: Vec<&Location> = list
		.iter()
		.filter(|l| l.uri.path().contains("/tests/exec/"))
		.collect();
	let files: std::collections::HashSet<&str> = exec.iter().map(|l| l.uri.path()).collect();
	eprintln!(
		"{} references, {} in {} files of tests/exec",
		list.len(),
		exec.len(),
		files.len()
	);
	assert!(files.len() > 50, "{} files", files.len());
	c.shutdown();
}
