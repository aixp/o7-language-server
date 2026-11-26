//! The project file `o7.toml`: programs for different targets in one
//! workspace. Each module gets the target of its programs; a module that
//! programs for different targets import is analysed for each target.

mod common;

use std::path::Path;

use common::*;
use lsp_types::request::{Completion, References, Rename};
use lsp_types::{
	CompletionParams, CompletionResponse, Position, ReferenceContext, ReferenceParams,
	RenameParams, Url,
};
use serde_json::json;

const PROJECT: &str = "\
search = [\"lib\"]

[[program]]
name = \"arm\"
root = \"arm/Main.Mod\"
board = \"nucleo-f446re\"

[[program]]
name = \"rv\"
root = \"rv/Main.Mod\"
board = \"qemu-virt-rv32imac\"
";

const ARM: &str = "MODULE Main;\n  IMPORT SYSTEM, Shared;\n  VAR a: INTEGER;\nBEGIN\n  a := Shared.Twice(1);\n  SYSTEM.WFE;\n  SYSTEM.\nEND Main.\n";
const RV: &str = "MODULE Main;\n  IMPORT SYSTEM, Shared;\n  VAR a: INTEGER;\nBEGIN\n  a := Shared.Twice(1);\n  SYSTEM.WFE;\n  SYSTEM.\nEND Main.\n";
const SHARED: &str = "MODULE Shared;\n  PROCEDURE Twice*(n: INTEGER): INTEGER;\n  BEGIN RETURN 2 * n\n  END Twice;\nEND Shared.\n";
const FREE: &str = "MODULE Free;\n  IMPORT SYSTEM;\nBEGIN\n  SYSTEM.\nEND Free.\n";

/// The message of the front end for the incomplete `SYSTEM.` of the test
/// modules.
const NO_NAME: &str = "identifier expected";

/// A new workspace with the project file `project` and the test modules.
fn workspace(name: &str, project: &str) -> std::path::PathBuf {
	let dir = temp_dir(name);
	write_files(
		&dir,
		&[
			("o7.toml", project),
			("arm/Main.Mod", ARM),
			("rv/Main.Mod", RV),
			("lib/Shared.Mod", SHARED),
			("lib/Free.Mod", FREE),
		],
	);
	dir
}

/// The client with the settings target `thumbv7em-m4f` (`TEST_TARGET`):
/// the project file must have priority.
fn start(dir: &Path) -> Client {
	Client::start(Setup {
		folders: vec![dir.to_path_buf()],
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

fn has(items: &[String], name: &str) -> bool {
	items.iter().any(|i| i == name)
}

/// Tells the server that the file changed on the disk.
fn changed(c: &mut Client, path: &Path) {
	c.notify::<lsp_types::notification::DidChangeWatchedFiles>(
		lsp_types::DidChangeWatchedFilesParams {
			changes: vec![lsp_types::FileEvent::new(
				uri(path),
				lsp_types::FileChangeType::CHANGED,
			)],
		},
	);
}

/// The messages without the message for the incomplete `SYSTEM.`.
fn errors(d: &[lsp_types::Diagnostic]) -> Vec<String> {
	texts(d)
		.into_iter()
		.filter(|t| !t.contains(NO_NAME))
		.collect()
}

#[test]
fn each_program_has_its_target() {
	let dir = workspace("project-targets", PROJECT);
	let mut c = start(&dir);
	let arm = c.open(&dir.join("arm/Main.Mod"));
	let rv = c.open(&dir.join("rv/Main.Mod"));
	// SYSTEM.WFE is a procedure of thumbv7em-m4f only.
	assert_eq!(errors(&c.sync(&arm)), [] as [&str; 0]);
	assert_eq!(
		errors(&c.sync(&rv)),
		["5:2: SYSTEM.WFE is not available on this target"]
	);
	let at = find(ARM, "SYSTEM.\n", 0, 7);
	let items = complete(&mut c, &arm, at);
	assert!(has(&items, "WFE"), "{items:?}");
	assert!(!has(&items, "FENCE"), "{items:?}");
	let items = complete(&mut c, &rv, at);
	assert!(has(&items, "FENCE"), "{items:?}");
	assert!(!has(&items, "WFE"), "{items:?}");
	// The predeclared names: rv32imac has no type REAL.
	let at = find(RV, "a := ", 0, 5);
	assert!(has(&complete(&mut c, &arm, at), "REAL"));
	assert!(!has(&complete(&mut c, &rv, at), "REAL"));

	// Two analyses for each program: the shared module is analysed for
	// each target. An edit in one program does not change the other one.
	assert_eq!(c.analyses(), 4);
	c.change(&rv, None, &RV.replace("SYSTEM.WFE", "SYSTEM.WFI"));
	assert_eq!(errors(&c.sync(&rv)), [] as [&str; 0]);
	assert_eq!(c.analyses(), 5);
	assert_eq!(errors(&c.last_diags(&arm)), [] as [&str; 0]);

	let s = c.status();
	assert_eq!(s["settings"]["target"], "thumbv7em-m4f");
	assert_eq!(s["projectFiles"][0]["errors"], 0);
	assert_eq!(
		s["projectFiles"][0]["programs"][1],
		json!({
			"name": "rv",
			"root": dir.join("rv/Main.Mod").display().to_string(),
			"board": "qemu-virt-rv32imac",
			"target": "rv32imac",
		})
	);
	assert_eq!(
		s["documents"][1]["contexts"],
		json!([{
			"source": "program", "program": "rv",
			"board": "qemu-virt-rv32imac", "target": "rv32imac",
		}])
	);
	c.shutdown();
}

#[test]
fn shared_module_has_the_errors_of_all_its_programs() {
	let dir = workspace("project-shared", PROJECT);
	// The type REAL is an error of the program `rv` only; the name `y` is
	// an error of the two programs.
	let shared = SHARED.replace(
		"  BEGIN RETURN 2 * n\n",
		"    VAR r: REAL;\n  BEGIN y := 1; RETURN 2 * n\n",
	);
	write_files(&dir, &[("lib/Shared.Mod", &shared)]);
	let mut c = start(&dir);
	let u = c.open(&dir.join("lib/Shared.Mod"));
	let expected = [
		"2:11: the type REAL is not available on this target (program rv, board qemu-virt-rv32imac)",
		"3:8: `y` is not declared",
	];
	assert_eq!(texts(&c.sync(&u)), expected);
	// The requests use the primary context: the first program.
	let items = complete(&mut c, &u, find(&shared, "RETURN", 0, 0));
	assert!(has(&items, "REAL"), "{items:?}");
	let s = c.status();
	let contexts = s["documents"][0]["contexts"].as_array().expect("contexts");
	let names: Vec<&str> = contexts
		.iter()
		.map(|x| x["program"].as_str().unwrap())
		.collect();
	assert_eq!(names, ["arm", "rv"]);
	c.close(&u);

	// The same messages when only a root module is open.
	let rv = c.open(&dir.join("rv/Main.Mod"));
	c.sync(&rv);
	assert_eq!(
		texts(&c.last_diags(&u)),
		[
			"2:11: the type REAL is not available on this target",
			"3:8: `y` is not declared",
		]
	);
	// With the two root modules open, the shared module has two contexts.
	let arm = c.open(&dir.join("arm/Main.Mod"));
	c.sync(&arm);
	assert_eq!(texts(&c.last_diags(&u)), expected);
	c.shutdown();
}

#[test]
fn references_and_rename_in_a_shared_module() {
	let dir = workspace("project-refs", PROJECT);
	let mut c = start(&dir);
	let u = c.open(&dir.join("lib/Shared.Mod"));
	c.sync(&u);
	let at = find(SHARED, "Twice*", 0, 0);
	// The references in the programs of the two targets, each one time.
	let list = c
		.request::<References>(ReferenceParams {
			text_document_position: pos_params(&u, at),
			work_done_progress_params: Default::default(),
			partial_result_params: Default::default(),
			context: ReferenceContext {
				include_declaration: false,
			},
		})
		.expect("references");
	let mut places: Vec<String> = list
		.iter()
		.map(|l| {
			let path = l.uri.to_file_path().unwrap();
			let rel = path.strip_prefix(&dir).unwrap().display().to_string();
			format!("{rel}:{}", l.range.start.line)
		})
		.collect();
	places.sort();
	assert_eq!(
		places,
		["arm/Main.Mod:4", "lib/Shared.Mod:3", "rv/Main.Mod:4"]
	);
	// A rename needs a check for each target: not available.
	let e = c
		.try_request::<Rename>(RenameParams {
			text_document_position: pos_params(&u, at),
			new_name: "Double".to_string(),
			work_done_progress_params: Default::default(),
		})
		.expect_err("refused");
	assert!(
		e.message
			.contains("is in programs for different targets (thumbv7em-m4f, rv32imac)"),
		"{}",
		e.message
	);
	// A rename in a module of one program is possible.
	let arm = c.open(&dir.join("arm/Main.Mod"));
	c.sync(&arm);
	let edit = c
		.request::<Rename>(RenameParams {
			text_document_position: pos_params(&arm, find(ARM, "a := ", 0, 0)),
			new_name: "b".to_string(),
			work_done_progress_params: Default::default(),
		})
		.expect("edit");
	assert_eq!(edit.changes.expect("changes")[&arm].len(), 2);
	c.shutdown();
}

#[test]
fn module_in_no_program() {
	let dir = workspace("project-default", PROJECT);
	let file = dir.join("o7.toml");
	let mut c = start(&dir);
	let u = c.open(&dir.join("lib/Free.Mod"));
	// The setting `target` of the client does not apply: the project file
	// decides.
	let no_program = format!(
		"0:0: no target: the module is in no program of {}, and that file has no default board (the key `board` at its top)",
		file.display()
	);
	assert_eq!(texts(&c.sync(&u)), [no_program.as_str()]);
	let s = c.status();
	assert_eq!(s["documents"][0]["contexts"], json!([]));
	assert_eq!(s["documents"][0]["problem"], no_program[5..]);
	let e = c
		.try_request::<Completion>(CompletionParams {
			text_document_position: pos_params(&u, find(FREE, "SYSTEM.\n", 0, 7)),
			work_done_progress_params: Default::default(),
			partial_result_params: Default::default(),
			context: None,
		})
		.expect_err("no target");
	assert!(e.message.starts_with("no target: "), "{}", e.message);

	// A default board: the module is a root with the defaults of the file.
	write_files(
		&dir,
		&[(
			"o7.toml",
			&format!("board = \"qemu-virt-rv32imafc\"\n{PROJECT}"),
		)],
	);
	changed(&mut c, &file);
	c.wait_diags(&u, |d| errors(d).is_empty());
	let items = complete(&mut c, &u, find(FREE, "SYSTEM.\n", 0, 7));
	assert!(has(&items, "FENCE"), "{items:?}");
	assert!(has(&items, "SQRT"), "{items:?}");
	assert_eq!(
		c.status()["documents"][0]["contexts"],
		json!([{
			"source": "default", "program": null,
			"board": "qemu-virt-rv32imafc", "target": "rv32imafc",
		}])
	);
	c.shutdown();
}

#[test]
fn errors_in_the_project_file() {
	let dir = workspace("project-errors", &PROJECT.replace("nucleo-f446re", "z80"));
	let file = dir.join("o7.toml");
	let project = uri(&file);
	let mut c = start(&dir);
	let u = c.open(&dir.join("rv/Main.Mod"));
	// The module is not analysed, and the project file has the message.
	assert_eq!(
		texts(&c.sync(&u)),
		[format!(
			"0:0: no target: the project file {} has errors",
			file.display()
		)]
	);
	assert_eq!(c.analyses(), 0);
	let d = c.last_diags(&project);
	assert_eq!(
		texts(&d),
		[
			"5:8: unknown board `z80`; the boards are: nucleo-f446re, nucleo-f411re, qemu-stm32f405, qemu-virt-rv32imac, qemu-virt-rv32imafc, esp32h2"
		]
	);
	// The range is the string with its quotes.
	assert_eq!(d[0].range.end, Position::new(5, 13));
	assert_eq!(c.status()["projectFiles"][0]["errors"], 1);

	// The file is correct again, but a root module is not a file.
	write_files(
		&dir,
		&[("o7.toml", &PROJECT.replace("arm/Main.Mod", "arm/None.Mod"))],
	);
	changed(&mut c, &file);
	c.wait_diags(&u, |d| {
		errors(d) == ["5:2: SYSTEM.WFE is not available on this target"]
	});
	let d = c.wait_diags(&project, |d| !d.is_empty() && !d[0].message.contains("z80"));
	assert_eq!(
		texts(&d),
		[format!(
			"4:7: the root module of the program arm is not a file: {}",
			dir.join("arm/None.Mod").display()
		)]
	);

	// All errors corrected: the messages of the project file go away.
	write_files(&dir, &[("o7.toml", PROJECT)]);
	changed(&mut c, &file);
	c.wait_diags(&project, |d| d.is_empty());
	c.shutdown();
}

#[test]
fn settings_without_a_project_file() {
	// No project file: the settings give the target, as before.
	let dir = temp_dir("project-none");
	write_files(&dir, &[("rv/Main.Mod", RV), ("rv/Shared.Mod", SHARED)]);
	let mut c = start(&dir);
	let u = c.open(&dir.join("rv/Main.Mod"));
	assert_eq!(errors(&c.sync(&u)), [] as [&str; 0]);
	assert_eq!(
		c.status()["documents"][0],
		json!({
			"path": dir.join("rv/Main.Mod").display().to_string(),
			"projectFile": null,
			"contexts": [{
				"source": "settings", "program": null, "board": null,
				"target": "thumbv7em-m4f",
			}],
			"problem": null,
		})
	);
	// A project file is created: it replaces the settings.
	write_files(&dir, &[("o7.toml", "board = \"esp32h2\"\n")]);
	changed(&mut c, &dir.join("o7.toml"));
	c.wait_diags(&u, |d| {
		errors(d) == ["5:2: SYSTEM.WFE is not available on this target"]
	});
	c.shutdown();
}
