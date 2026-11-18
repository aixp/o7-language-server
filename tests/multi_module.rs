//! Phase 3: programs with more than one module.

mod common;

use common::*;
use lsp_types::{Position, Range};
use serde_json::json;

const A: &str = "MODULE A;\n  VAR x*: INTEGER;\n  PROCEDURE P*(n: INTEGER): INTEGER;\n  BEGIN RETURN n + 1\n  END P;\nEND A.\n";
const B: &str = "MODULE B;\n  IMPORT A;\n  VAR y: INTEGER;\nBEGIN\n  y := A.P(A.x)\nEND B.\n";

#[test]
fn interface_change_in_an_open_buffer_updates_the_importer() {
	let dir = temp_dir("interface");
	write_files(&dir, &[("A.Mod", A), ("B.Mod", B)]);
	let mut c = Client::start(Setup::default());
	let ub = c.open(&dir.join("B.Mod"));
	assert!(c.sync(&ub).is_empty());
	let ua = c.open(&dir.join("A.Mod"));
	assert!(c.sync(&ua).is_empty());
	// Change the type of x in the buffer of A (not saved).
	let p = find(A, "INTEGER", 0, 0);
	c.change(
		&ua,
		Some(Range::new(p, Position::new(p.line, p.character + 7))),
		"BOOLEAN",
	);
	assert!(c.sync(&ua).is_empty());
	let d = c.last_diags(&ub);
	assert_eq!(d.len(), 1, "{d:?}");
	assert_eq!(d[0].range.start.line, 4);
	// Back: the importer has no message again.
	let p = find(A, "INTEGER", 0, 0);
	c.change(
		&ua,
		Some(Range::new(p, Position::new(p.line, p.character + 7))),
		"INTEGER",
	);
	assert!(c.sync(&ua).is_empty());
	assert!(c.last_diags(&ub).is_empty());
	c.shutdown();
}

#[test]
fn a_body_change_causes_no_analysis_of_importers() {
	let dir = temp_dir("body");
	write_files(&dir, &[("A.Mod", A), ("B.Mod", B)]);
	let mut c = Client::start(Setup::default());
	let ub = c.open(&dir.join("B.Mod"));
	c.sync(&ub);
	let ua = c.open(&dir.join("A.Mod"));
	c.sync(&ua);
	let before = c.analyses();
	// `n + 1` -> `n + 2`: the interface does not change.
	let p = find(A, "1", 0, 0);
	c.change(
		&ua,
		Some(Range::new(p, Position::new(p.line, p.character + 1))),
		"2",
	);
	assert!(c.sync(&ua).is_empty());
	assert_eq!(c.analyses() - before, 1, "only A is analysed again");
	// An interface change: A and B.
	let before = c.analyses();
	let p = find(A, "  PROCEDURE", 0, 0);
	c.change(&ua, Some(Range::new(p, p)), "  z*: INTEGER;\n");
	assert!(c.sync(&ua).is_empty());
	assert_eq!(c.analyses() - before, 2, "A and its importer B");
	// A request with no change analyses nothing.
	let before = c.analyses();
	c.request::<lsp_types::request::HoverRequest>(lsp_types::HoverParams {
		text_document_position_params: pos_params(&ub, find(B, "A.x", 0, 2)),
		work_done_progress_params: Default::default(),
	});
	assert_eq!(c.analyses(), before);
	c.shutdown();
}

#[test]
fn missing_import() {
	let dir = temp_dir("missing");
	let text = "MODULE C;\n  IMPORT Nothing;\nEND C.\n";
	write_files(&dir, &[("C.Mod", text)]);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("C.Mod"));
	// One message: the message of the front end about the same import is
	// not shown a second time.
	assert_eq!(
		texts(&c.sync(&u)),
		["1:9: cannot find the source file Nothing.Mod of the imported module Nothing"]
	);
	// The file is created (and the client reports it): the message goes away.
	write_files(&dir, &[("Nothing.Mod", "MODULE Nothing; END Nothing.")]);
	c.notify::<lsp_types::notification::DidChangeWatchedFiles>(
		lsp_types::DidChangeWatchedFilesParams {
			changes: vec![lsp_types::FileEvent::new(
				uri(&dir.join("Nothing.Mod")),
				lsp_types::FileChangeType::CREATED,
			)],
		},
	);
	c.wait_diags(&u, |d| d.is_empty());
	c.shutdown();
}

#[test]
fn circular_import() {
	let dir = temp_dir("circular");
	write_files(
		&dir,
		&[
			("X.Mod", "MODULE X;\n  IMPORT Y;\nEND X.\n"),
			("Y.Mod", "MODULE Y;\n  IMPORT X;\nEND Y.\n"),
		],
	);
	let mut c = Client::start(Setup::default());
	let ux = c.open(&dir.join("X.Mod"));
	let dx = c.sync(&ux);
	assert_eq!(
		texts(&dx),
		["1:9: cannot import module Y: module Y has errors"]
	);
	let dy = c.last_diags(&uri(&dir.join("Y.Mod")));
	assert_eq!(texts(&dy), ["1:9: circular import: X -> Y -> X"]);
	c.shutdown();
}

#[test]
fn wrong_module_name() {
	let dir = temp_dir("wrong-name");
	write_files(
		&dir,
		&[
			("C.Mod", "MODULE C;\n  IMPORT W;\nEND C.\n"),
			("W.Mod", "MODULE Other;\nEND Other.\n"),
		],
	);
	let mut c = Client::start(Setup::default());
	let u = c.open(&dir.join("C.Mod"));
	c.sync(&u);
	let w = dir.join("W.Mod");
	let dw = c.last_diags(&uri(&w));
	assert_eq!(dw.len(), 1, "{dw:?}");
	// At the module name.
	assert_eq!(
		dw[0].range,
		Range::new(Position::new(0, 7), Position::new(0, 12))
	);
	assert_eq!(
		dw[0].message,
		format!(
			"{}: the file contains module Other, but the file name requires module W",
			w.display()
		)
	);
	assert_eq!(
		texts(&c.last_diags(&u)),
		["1:9: cannot import module W: no symbol file for module W"]
	);
	c.shutdown();
}

#[test]
fn module_with_errors_and_stale_snapshot() {
	let dir = temp_dir("stale");
	let a = "MODULE A;\n  VAR x*: INTEGER;\nEND A.\n";
	let b = "MODULE B;\n  IMPORT A;\n  VAR y*: INTEGER;\nBEGIN y := A.x\nEND B.\n";
	let cc = "MODULE C;\n  IMPORT A, B;\n  VAR v: INTEGER;\nBEGIN v := A.x + B.y\nEND C.\n";
	write_files(&dir, &[("A.Mod", a), ("B.Mod", b), ("C.Mod", cc)]);
	let mut c = Client::start(Setup::default());
	let uc = c.open(&dir.join("C.Mod"));
	assert!(c.sync(&uc).is_empty());
	// B gets an error: C uses the last snapshot of B without errors.
	let ub = c.open(&dir.join("B.Mod"));
	c.sync(&ub);
	let p = find(b, "END B", 0, 0);
	c.change(&ub, Some(Range::new(p, p)), "  error\n");
	assert!(!c.sync(&ub).is_empty());
	assert!(c.last_diags(&uc).is_empty(), "{:?}", c.last_diags(&uc));
	// The interface of A changes: the snapshot of B is stale now.
	let ua = c.open(&dir.join("A.Mod"));
	c.sync(&ua);
	let p = find(a, "END A", 0, 0);
	c.change(&ua, Some(Range::new(p, p)), "  z*: INTEGER;\n");
	assert!(c.sync(&ua).is_empty());
	assert_eq!(
		texts(&c.last_diags(&uc)),
		[
			"1:12: cannot import module B: module B has errors",
			// The failed import declares nothing.
			"3:17: `B` is not declared"
		]
	);
	// B is repaired: no message.
	c.change(&ub, None, b);
	assert!(c.sync(&ub).is_empty());
	assert!(c.last_diags(&uc).is_empty());
	c.shutdown();
}

#[test]
fn module_with_errors_and_no_snapshot() {
	let dir = temp_dir("no-snapshot");
	write_files(
		&dir,
		&[
			("A.Mod", "MODULE A;\n  VAR x*: UNKNOWN;\nEND A.\n"),
			("B.Mod", "MODULE B;\n  IMPORT A;\nEND B.\n"),
		],
	);
	let mut c = Client::start(Setup::default());
	let ub = c.open(&dir.join("B.Mod"));
	assert_eq!(
		texts(&c.sync(&ub)),
		["1:9: cannot import module A: module A has errors"]
	);
	// The diagnostics of the imported file are published too.
	assert_eq!(c.last_diags(&uri(&dir.join("A.Mod"))).len(), 1);
	// When B is closed, no open document imports A: its diagnostics are cleared.
	c.close(&ub);
	c.wait_diags(&uri(&dir.join("A.Mod")), |d| d.is_empty());
	c.shutdown();
}

#[test]
fn changed_search_paths() {
	let dir = temp_dir("search");
	write_files(
		&dir,
		&[
			("src/C.Mod", "MODULE C;\n  IMPORT L;\nBEGIN L.P\nEND C.\n"),
			("lib/L.Mod", "MODULE L;\n  PROCEDURE P*; END P;\nEND L.\n"),
			("other/L.Mod", "MODULE L;\nEND L.\n"),
		],
	);
	let mut c = Client::start(Setup {
		folders: vec![dir.clone()],
		..Setup::default()
	});
	let u = c.open(&dir.join("src/C.Mod"));
	let d = texts(&c.sync(&u));
	assert_eq!(
		d[0], "1:9: cannot find the source file L.Mod of the imported module L",
		"{d:?}"
	);
	// A relative path starts at the workspace folder.
	c.configure(json!({ "searchPaths": ["lib"] }));
	c.wait_diags(&u, |d| d.is_empty());
	// The first directory of the list has priority: other/L.Mod has no P.
	c.configure(json!({ "o7-language-server": { "searchPaths": ["other", "lib"] } }));
	let d = c.wait_diags(&u, |d| !d.is_empty());
	assert_eq!(texts(&d), ["2:8: `P` is not exported by module L"]);
	c.shutdown();
}

#[test]
fn settings_from_workspace_configuration() {
	let dir = temp_dir("pull");
	write_files(
		&dir,
		&[
			("src/C.Mod", "MODULE C;\n  IMPORT L;\nEND C.\n"),
			("lib/L.Mod", "MODULE L;\nEND L.\n"),
		],
	);
	let mut c = Client::start(Setup {
		folders: vec![dir.clone()],
		pull_settings: Some(json!({ "searchPaths": ["lib"] })),
		..Setup::default()
	});
	let u = c.open(&dir.join("src/C.Mod"));
	c.wait_diags(&u, |d| d.is_empty());
	c.shutdown();
}

#[test]
fn disk_change_without_notification() {
	let dir = temp_dir("disk");
	write_files(&dir, &[("A.Mod", A), ("B.Mod", B)]);
	let mut c = Client::start(Setup::default());
	let ub = c.open(&dir.join("B.Mod"));
	assert!(c.sync(&ub).is_empty());
	// Another program changes A (no message of the client).
	write_files(
		&dir,
		&[("A.Mod", &A.replace("VAR x*: INTEGER;", "VAR xx*: INTEGER;"))],
	);
	// The next change of B sees the new A.
	let end = find(B, "END B.", 0, 6);
	c.change(&ub, Some(Range::new(end, end)), " ");
	assert_eq!(
		texts(&c.sync(&ub)),
		["4:13: `x` is not exported by module A"]
	);
	c.shutdown();
}
