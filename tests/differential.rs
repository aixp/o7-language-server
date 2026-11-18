//! Differential test (plan, section 5.6): for each module of the compiler
//! repository, the diagnostics of the server are equal to the diagnostics
//! of `front::parse_module` (file, line, column, message).
//!
//! The reference analyses each program as the compiler driver does
//! (`project::collect`, then `parse_module` for each module in the order of
//! the imports, the symbol file of each module without errors made in
//! memory), with the rules of the server for an editor:
//!
//! - a module whose import has errors is analysed too; the loader refuses
//!   the import with "module X has errors" (the driver does not compile
//!   such a module);
//! - the message "cannot import module X" of the front end at an import
//!   that `project::collect` reported is not shown a second time;
//! - a message of `project::collect` about a wrong module name is at the
//!   module name of the file.
//!
//! Set `O7C_REPO` to use a compiler repository other than
//! `../oberon-compiler`.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::*;
use o7_front::diag::Pos;
use o7_front::front::{self, ImportLoader};
use o7_front::project::{self, FsHost};
use o7_front::symfile::SymFile;
use o7_front::target_info::THUMBV7EM_M4F;
use o7_language_server::vfs::{Encoding, LineIndex, normalize};
use serde_json::json;

type Messages = BTreeMap<PathBuf, Vec<(Pos, String)>>;

const ROOT_DIRS: [&str; 8] = [
	"tests/exec",
	"tests/reject",
	"tests/codegen",
	"tests/index",
	"tests/lib",
	"lib/Micro",
	"lib/Micro/hsebyp",
	"examples",
];

/// The root modules of the test programs in `dir`: each file `dir/X.Mod` and
/// each file `dir/X/X.Mod` (as the tests of the compiler).
fn roots(dir: &Path) -> Vec<PathBuf> {
	let mut list = Vec::new();
	for e in std::fs::read_dir(dir).unwrap() {
		let p = e.unwrap().path();
		if p.is_dir() {
			let r = p.join(format!("{}.Mod", p.file_name().unwrap().to_string_lossy()));
			if r.is_file() {
				list.push(r);
			}
		} else if p.extension().is_some_and(|x| x == "Mod") {
			list.push(p);
		}
	}
	list.sort();
	list
}

struct Loader<'a> {
	symfiles: &'a HashMap<String, Rc<SymFile>>,
	analysed: &'a HashSet<String>,
}

impl ImportLoader for Loader<'_> {
	fn load(&mut self, module: &str) -> Result<Rc<SymFile>, String> {
		if let Some(s) = self.symfiles.get(module) {
			return Ok(s.clone());
		}
		Err(if self.analysed.contains(module) {
			format!("module {module} has errors")
		} else {
			format!("no symbol file for module {module}")
		})
	}
}

fn reference(root: &Path, dirs: &[PathBuf]) -> Messages {
	let (units, problems) = project::collect(&FsHost, root, dirs);
	let mut out: Messages = BTreeMap::new();
	let mut at_problem: HashMap<PathBuf, HashSet<Pos>> = HashMap::new();
	for d in &problems {
		let path = normalize(Path::new(&d.file));
		let mut pos = d.pos;
		if pos.line == 0 {
			pos = if d.msg.contains("the file contains module") {
				project::scan_header(&std::fs::read(&path).unwrap()).name_pos
			} else {
				Pos::new(1, 1)
			};
		}
		at_problem.entry(path.clone()).or_default().insert(d.pos);
		out.entry(path).or_default().push((pos, d.msg.clone()));
	}
	let mut symfiles = HashMap::new();
	let mut analysed = HashSet::new();
	for u in units {
		let path = normalize(&u.path);
		let file = path.display().to_string();
		let mut loader = Loader {
			symfiles: &symfiles,
			analysed: &analysed,
		};
		let (module, diags) = front::parse_module(&u.src, &file, &THUMBV7EM_M4F, &mut loader);
		let list = out.entry(path.clone()).or_default();
		for d in diags.list() {
			if d.msg.starts_with("cannot import module ")
				&& at_problem.get(&path).is_some_and(|s| s.contains(&d.pos))
			{
				continue;
			}
			list.push((d.pos, d.msg.clone()));
		}
		if !diags.has_errors() {
			let sf = SymFile::parse(&SymFile::from_module(&module).to_text()).unwrap();
			symfiles.insert(u.name.clone(), Rc::new(sf));
		}
		analysed.insert(u.name);
	}
	for v in out.values_mut() {
		v.sort();
		v.dedup();
	}
	out
}

#[test]
fn diagnostics_equal_those_of_the_front_end() {
	let Some(repo) = compiler_repo() else {
		eprintln!("skipped: the compiler repository is not available");
		return;
	};
	let search: Vec<PathBuf> = compiler_search_paths(&repo)
		.iter()
		.map(PathBuf::from)
		.collect();
	let mut c = Client::start(Setup {
		options: json!({ "searchPaths": compiler_search_paths(&repo) }),
		debounce: std::time::Duration::ZERO,
		..Setup::default()
	});
	let mut count = 0;
	let mut files = HashSet::new();
	let mut messages = 0;
	let mut failures = Vec::new();
	for dir in ROOT_DIRS {
		for root in roots(&repo.join(dir)) {
			let mut dirs = vec![root.parent().unwrap().to_path_buf()];
			dirs.extend(search.iter().cloned());
			let expected = reference(&root, &dirs);
			let u = c.open(&root);
			c.sync(&u);
			for (path, want) in &expected {
				let text = std::fs::read(path).unwrap_or_default();
				let li = LineIndex::new(&text);
				let mut got: Vec<(Pos, String)> = c
					.last_diags(&uri(path))
					.iter()
					.map(|d| {
						let off = li.lsp_offset(&text, d.range.start, Encoding::Utf16);
						(li.pos(off), d.message.clone())
					})
					.collect();
				got.sort();
				if got != *want {
					failures.push(format!(
						"root {}, file {}:\n  server:    {got:?}\n  reference: {want:?}",
						root.display(),
						path.display()
					));
				}
				files.insert(path.clone());
				messages += want.len();
			}
			c.close(&u);
			count += 1;
		}
	}
	c.shutdown();
	eprintln!("{count} roots, {} files, {messages} messages", files.len());
	assert!(count > 100, "the test programs were not found");
	assert!(messages > 50, "the reject tests give messages");
	assert!(
		failures.is_empty(),
		"{} differences:\n{}",
		failures.len(),
		failures.join("\n")
	);
}
