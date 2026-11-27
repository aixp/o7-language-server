//! The modules of the workspace and their analyses.
//!
//! # Model
//!
//! A module is analysed in a [`Context`]: a target and a search list.
//! [`Workspace::contexts_of`] gives the contexts of a module:
//!
//! - With a project file `o7.toml` (the nearest one upward from the module,
//!   as the compiler finds it): one context for each program of the module
//!   (`o7_front::project_file`, "the programs of a module"). A module that
//!   programs for different targets import has more than one context. The
//!   first context is the *primary* context: the requests of an editor
//!   (completion, hover) use it. The diagnostics are those of all contexts.
//! - Without a project file: the module is a root module. The settings give
//!   the target; the search list is the directory of the module, then the
//!   search paths of the settings (the rule of the compiler for a root
//!   module).
//!
//! [`Workspace::check_root`] finds the modules that a module imports with
//! `project::collect` (the function of the compiler: same naming rule, same
//! search order, same messages) and analyses each one that is not up to
//! date, in the order of the imports.
//!
//! Results are stored for each target and each file path, not for each
//! module name: two contexts can see different files for one module name,
//! and the predeclared names (the procedures of `SYSTEM`, the type `REAL`)
//! depend on the target.
//!
//! # Memo table
//!
//! For each file the workspace keeps the last analysis ([`Unit`]) and the
//! last *interface snapshot* ([`Snapshot`]): the symbol file of the last
//! analysis without errors. The symbol file is made as the compiler makes
//! it: `SymFile::from_module`, `to_text`, `SymFile::parse`. No file is
//! written.
//!
//! An analysis is up to date if the text is the same and each module that
//! the loader gave to it ([`Dep`]) still resolves to the same file and the
//! same symbol-file key. The key changes only when the interface changes, so
//! an edit that does not change the interface of a module causes no new
//! analysis of its importers.
//!
//! # Modules with errors
//!
//! If the text of a module has errors, its importers use its last snapshot
//! without errors. If no such snapshot exists, the loader refuses the module
//! ("module A has errors"). A snapshot that was made with an older version of
//! one of its imports is *stale*; the front end then reports "module B was
//! compiled with a different version of A". The workspace replaces this
//! message with "module B has errors".
//!
//! # Workspace index
//!
//! All `*.Mod` files of the workspace folders and of the search paths are
//! analysed as roots: first in the background ([`Workspace::index_step`]),
//! then again before a query that needs the complete workspace
//! ([`Workspace::refresh_index`]).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use o7_front::diag::Diagnostic;
use o7_front::front::{self, Analysis, ImportLoader};
use o7_front::hir::TypeId;
use o7_front::project::{self, SOURCE_EXT, SourceHost};
use o7_front::project_file::{self, PROJECT_FILE, ProjectFile, Use};
use o7_front::symfile::SymFile;
use o7_front::target_info::TargetProps;

use crate::config::{Config, no_program_message, no_target_message, project_errors_message};
use crate::vfs::{Document, LineIndex, normalize};

/// Maximum number of files in the workspace index.
const MAX_INDEX_FILES: usize = 20_000;
/// Maximum directory depth of the search for files.
const MAX_DEPTH: usize = 24;

/// Where a [`Context`] comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
	/// A program of the project file.
	Program { name: String },
	/// The defaults of the project file: the module is in no program.
	Default,
	/// The settings of the client: there is no project file.
	Settings,
}

/// The target and the search list for the analysis of a module and of the
/// modules that it imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
	pub target: &'static TargetProps,
	/// The complete search list (the directory of the root module first).
	pub dirs: Vec<PathBuf>,
	pub source: Source,
}

impl Context {
	/// Two contexts with the same target and the same search list give the
	/// same analyses.
	pub fn same_analysis(&self, other: &Context) -> bool {
		self.target.name == other.target.name && self.dirs == other.dirs
	}

	/// The context in a message, for example `program rv, target rv32imac`.
	/// `root` is the root module of a context without a program.
	pub fn label(&self, root: &Path) -> String {
		match &self.source {
			Source::Program { name } => format!("program {name}, target {}", self.target.name),
			_ => format!(
				"root module {}",
				root.file_name().unwrap_or_default().to_string_lossy()
			),
		}
	}
}

/// A project file as the workspace read it.
struct ProjectEntry {
	text: Rc<[u8]>,
	parsed: Result<Rc<ProjectFile>, Vec<Diagnostic>>,
	/// For each program the files of its modules, and the generation of the
	/// workspace for which they are correct.
	closures: Option<(u64, Rc<Vec<HashSet<PathBuf>>>)>,
}

/// One module that the loader gave to an analysis (or refused).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dep {
	pub name: String,
	/// The file of the module; `None` if the module is not in the program.
	pub path: Option<PathBuf>,
	/// The key of the symbol file; `None` if the loader refused the module.
	pub key: Option<u64>,
}

/// One analysis of one file.
#[derive(Debug)]
pub struct Unit {
	pub path: PathBuf,
	/// The target of the analysis.
	pub target: &'static TargetProps,
	/// The module name (from the header).
	pub name: String,
	pub src: Rc<[u8]>,
	pub lines: LineIndex,
	pub deps: Vec<Dep>,
	pub analysis: Analysis,
	/// The diagnostics of the analysis (with the replacement of the
	/// messages about stale snapshots).
	pub diags: Vec<Diagnostic>,
}

impl Unit {
	/// The file that the module `name` resolved to in this analysis.
	pub fn dep_path(&self, name: &str) -> Option<&Path> {
		self.deps
			.iter()
			.find(|d| d.name == name)
			.and_then(|d| d.path.as_deref())
	}

	pub fn has_errors(&self) -> bool {
		self.analysis.diags.has_errors()
	}
}

/// The interface of a module: the symbol file of an analysis without errors.
#[derive(Debug)]
pub struct Snapshot {
	pub unit: Rc<Unit>,
	pub symfile: Rc<SymFile>,
	/// For each type index of the symbol file: the type of the module.
	pub type_map: Vec<TypeId>,
	pub key: u64,
}

/// A file on the disk as the workspace read it.
#[derive(Debug)]
struct DiskFile {
	/// Modification time and size.
	stamp: (SystemTime, u64),
	text: Rc<[u8]>,
}

#[derive(Debug, Default)]
struct FileData {
	latest: Option<Rc<Unit>>,
	snapshot: Option<Rc<Snapshot>>,
}

/// The result of [`Workspace::check_root`].
#[derive(Clone, Debug, Default)]
pub struct RootInfo {
	/// The files of the program, each one after the files that it imports;
	/// the root is the last one.
	pub order: Vec<PathBuf>,
	/// The diagnostics of `project::collect`.
	pub problems: Vec<Diagnostic>,
}

#[derive(Default)]
pub struct Workspace {
	pub config: Config,
	/// The workspace folders.
	pub folders: Vec<PathBuf>,
	/// The open documents. Their text has priority over the disk.
	pub docs: HashMap<PathBuf, Document>,
	/// The contents of files on the disk.
	disk: RefCell<HashMap<PathBuf, DiskFile>>,
	/// The analyses, for each target name and each file.
	files: HashMap<&'static str, HashMap<PathBuf, FileData>>,
	/// The project files, for each path.
	projects: RefCell<HashMap<PathBuf, ProjectEntry>>,
	/// Number of calls of `analyze_module` (for tests and `o7/status`).
	pub analyses: usize,
	/// The files of the workspace index.
	index_files: BTreeSet<PathBuf>,
	queue: VecDeque<PathBuf>,
	/// Changed when a text or the configuration changes.
	generation: u64,
	index_generation: Option<u64>,
}

impl Workspace {
	pub fn new(config: Config, folders: Vec<PathBuf>) -> Workspace {
		Workspace {
			config,
			folders,
			..Workspace::default()
		}
	}

	// ----- texts -----

	/// The current text of a file: the open document, else the file on the
	/// disk. A file in the cache is read again if its modification time or
	/// its size changed (also without a notification of the client).
	pub fn text(&self, path: &Path) -> Option<Rc<[u8]>> {
		if let Some(d) = self.docs.get(path) {
			return Some(Rc::from(d.text.as_bytes()));
		}
		let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
		let stamp = (
			meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
			meta.len(),
		);
		if let Some(f) = self.disk.borrow().get(path)
			&& f.stamp == stamp
		{
			return Some(f.text.clone());
		}
		let text: Rc<[u8]> = std::fs::read(path).ok()?.into();
		let file = DiskFile {
			stamp,
			text: text.clone(),
		};
		self.disk.borrow_mut().insert(path.to_path_buf(), file);
		Some(text)
	}

	/// Something changed: queries must not use old results.
	pub fn touch(&mut self) {
		self.generation += 1;
	}

	pub fn open(&mut self, doc: Document) {
		self.docs.insert(doc.path.clone(), doc);
		self.touch();
	}

	pub fn close(&mut self, path: &Path) {
		self.docs.remove(path);
		self.disk.borrow_mut().remove(path);
		self.touch();
	}

	/// The file `path` changed on the disk (or was created or deleted).
	pub fn file_changed(&mut self, path: &Path, exists: bool) {
		self.disk.borrow_mut().remove(path);
		if self.in_index_area(path) && is_source(path) {
			if exists {
				if self.index_files.insert(path.to_path_buf()) {
					self.queue.push_back(path.to_path_buf());
				}
			} else {
				self.index_files.remove(path);
			}
		}
		if !exists && !self.docs.contains_key(path) {
			for files in self.files.values_mut() {
				files.remove(path);
			}
		}
		self.touch();
	}

	/// The configuration changed.
	pub fn set_config(&mut self, config: Config) {
		if config != self.config {
			self.config = config;
			self.disk.borrow_mut().clear();
			self.files.clear();
			self.projects.borrow_mut().clear();
			self.touch();
			self.start_index();
		}
	}

	// ----- results -----

	/// The last analysis of the file for the target.
	pub fn unit(&self, target: &TargetProps, path: &Path) -> Option<&Rc<Unit>> {
		self.files.get(target.name)?.get(path)?.latest.as_ref()
	}

	/// The last interface snapshot of the file for the target.
	pub fn snapshot(&self, target: &TargetProps, path: &Path) -> Option<&Rc<Snapshot>> {
		self.files.get(target.name)?.get(path)?.snapshot.as_ref()
	}

	/// The last analyses of all files, for all targets. A file that is
	/// analysed for two targets is there two times.
	pub fn units(&self) -> impl Iterator<Item = &Rc<Unit>> {
		self.files
			.values()
			.flat_map(|files| files.values())
			.filter_map(|f| f.latest.as_ref())
	}

	// ----- analysis -----

	/// The search list of a root module without a project file: its
	/// directory, then the search paths of the settings.
	pub fn search_dirs(&self, root: &Path) -> Vec<PathBuf> {
		let mut dirs = vec![root.parent().map(Path::to_path_buf).unwrap_or_default()];
		for d in self
			.config
			.search_dirs(self.folders.first().map(PathBuf::as_path))
		{
			if !dirs.contains(&d) {
				dirs.push(d);
			}
		}
		dirs
	}

	/// Brings the analyses of the module `root` and of the modules that it
	/// imports up to date, in the context `ctx`.
	pub fn check_root(&mut self, ctx: &Context, root: &Path) -> RootInfo {
		let target = ctx.target;
		let (units, problems) = project::collect(&*self, root, &ctx.dirs);
		// The modules before the current one (as the symbol files of the
		// compiler driver): a module can use only these.
		let mut available: HashMap<String, PathBuf> = HashMap::new();
		let mut order = Vec::with_capacity(units.len());
		for u in units {
			let path = normalize(&u.path);
			let fresh = self.unit(target, &path).is_some_and(|l| {
				*l.src == *u.src
					&& l.deps
						.iter()
						.all(|d| self.lookup(target, &d.name, &available).0 == *d)
			});
			if !fresh {
				self.analyse(&path, &u.name, Rc::from(u.src), target, &available);
			}
			available.insert(u.name, path.clone());
			order.push(path);
		}
		RootInfo { order, problems }
	}

	/// What the loader gives for the module `name`: the dependency record,
	/// and the snapshot if the module can be imported.
	fn lookup(
		&self,
		target: &TargetProps,
		name: &str,
		available: &HashMap<String, PathBuf>,
	) -> (Dep, Option<Rc<Snapshot>>) {
		let path = available.get(name).cloned();
		let snap = path
			.as_ref()
			.and_then(|p| self.snapshot(target, p).cloned());
		let dep = Dep {
			name: name.to_string(),
			path,
			key: snap.as_ref().map(|s| s.key),
		};
		(dep, snap)
	}

	/// A snapshot is stale if one of the modules that it imports now has
	/// another key.
	fn is_stale(&self, snap: &Snapshot, available: &HashMap<String, PathBuf>) -> bool {
		snap.symfile.imports.iter().any(|(m, key)| {
			available
				.get(m)
				.and_then(|p| self.snapshot(snap.unit.target, p))
				.is_some_and(|s| s.key != *key)
		})
	}

	fn analyse(
		&mut self,
		path: &Path,
		name: &str,
		src: Rc<[u8]>,
		target: &'static TargetProps,
		available: &HashMap<String, PathBuf>,
	) {
		let file = path.display().to_string();
		let mut loader = Loader {
			ws: self,
			target,
			available,
			used: Vec::new(),
			stale: Vec::new(),
		};
		let analysis = front::analyze_module(&src, &file, target, &mut loader);
		let Loader { used, stale, .. } = loader;
		let diags = replace_stale_messages(analysis.diags.list(), &stale);
		let unit = Rc::new(Unit {
			path: path.to_path_buf(),
			target,
			name: name.to_string(),
			lines: LineIndex::new(&src),
			src,
			deps: used,
			analysis,
			diags,
		});
		self.analyses += 1;
		let files = self.files.entry(target.name).or_default();
		let data = files.entry(path.to_path_buf()).or_default();
		data.latest = Some(unit.clone());
		if !unit.has_errors() {
			// As the compiler driver: importers see what is in the text of the
			// symbol file.
			let (sf, type_map) = SymFile::from_module_mapped(&unit.analysis.module);
			match SymFile::parse(&sf.to_text()) {
				Ok(sf) => {
					data.snapshot = Some(Rc::new(Snapshot {
						key: sf.key,
						symfile: Rc::new(sf),
						type_map,
						unit,
					}));
				}
				Err(e) => {
					eprintln!(
						"o7-language-server: module {name}: internal error in the symbol file: {e}"
					);
					data.snapshot = None;
				}
			}
		}
	}

	// ----- workspace index -----

	fn in_index_area(&self, path: &Path) -> bool {
		self.folders.iter().any(|f| path.starts_with(f))
			|| self
				.config
				.search_dirs(self.folders.first().map(PathBuf::as_path))
				.iter()
				.any(|d| path.parent() == Some(d.as_path()))
	}

	/// Finds the files of the workspace index and puts them in the queue of
	/// the background analysis.
	pub fn start_index(&mut self) {
		let mut files = BTreeSet::new();
		for f in &self.folders {
			find_sources(f, 0, &mut files);
		}
		for d in self
			.config
			.search_dirs(self.folders.first().map(PathBuf::as_path))
		{
			if let Ok(rd) = std::fs::read_dir(&d) {
				for e in rd.flatten() {
					let p = normalize(&e.path());
					if is_source(&p) && p.is_file() && files.len() < MAX_INDEX_FILES {
						files.insert(p);
					}
				}
			}
		}
		self.queue = files.iter().cloned().collect();
		self.index_files = files;
		self.index_generation = None;
	}

	pub fn index_files(&self) -> &BTreeSet<PathBuf> {
		&self.index_files
	}

	/// Work for the background: analyses the next file of the queue.
	/// Returns `false` if the queue is empty.
	pub fn index_step(&mut self) -> bool {
		let Some(p) = self.queue.pop_front() else {
			return false;
		};
		if self.index_files.contains(&p) {
			self.check_all(&p);
		}
		true
	}

	pub fn index_pending(&self) -> bool {
		!self.queue.is_empty()
	}

	/// Brings the analyses of all files of the workspace index and of all
	/// open documents up to date.
	pub fn refresh_index(&mut self) {
		if self.index_generation == Some(self.generation) && self.queue.is_empty() {
			return;
		}
		self.queue.clear();
		let roots: Vec<PathBuf> = self
			.index_files
			.iter()
			.filter(|p| !self.docs.contains_key(*p))
			.cloned()
			.collect();
		let mut used = HashSet::new();
		for r in roots {
			used.extend(self.check_all(&r));
		}
		// The open documents last: their own search list decides.
		let open: Vec<PathBuf> = self.docs.keys().cloned().collect();
		for r in open.iter().filter(|p| !is_project_file(p)) {
			used.extend(self.check_all(r));
		}
		// An analysis that no context uses now (the target of a program
		// changed, a module is not imported any more) is for an old text.
		for (target, files) in &mut self.files {
			files.retain(|path, _| used.contains(&(*target, path.clone())));
		}
		self.index_generation = Some(self.generation);
	}

	/// Brings the analyses of the module `path` up to date in all its
	/// contexts. The primary context is the last one: if two contexts with
	/// one target see different files for a module name, the primary
	/// context decides. The result is the list of the analyses that the
	/// contexts use: target name and file.
	pub fn check_all(&mut self, path: &Path) -> Vec<(&'static str, PathBuf)> {
		let mut used = Vec::new();
		if let Ok(list) = self.contexts_of(path) {
			for ctx in list.iter().rev() {
				let info = self.check_root(ctx, path);
				used.extend(info.order.into_iter().map(|p| (ctx.target.name, p)));
			}
		}
		used
	}

	// ----- project files and contexts -----

	/// The project file of the module in the file `path`: the nearest file
	/// `o7.toml` upward from its directory.
	pub fn project_file_of(&self, path: &Path) -> Option<PathBuf> {
		project_file::find(self, path.parent()?)
	}

	/// Reads the project file `path` (again if its text changed) and gives
	/// the result of `f` for it.
	fn with_project<T>(&self, path: &Path, f: impl FnOnce(&mut ProjectEntry) -> T) -> Option<T> {
		let text = self.text(path)?;
		let mut projects = self.projects.borrow_mut();
		let fresh = projects.get(path).is_some_and(|e| *e.text == *text);
		if !fresh {
			let parsed = project_file::parse(&text, path).map(Rc::new);
			let entry = ProjectEntry {
				text,
				parsed,
				closures: None,
			};
			projects.insert(path.to_path_buf(), entry);
		}
		projects.get_mut(path).map(f)
	}

	/// The contents of the project file `path`; the error is the list of
	/// the errors in the file. `None` if the file cannot be read.
	pub fn project(&self, path: &Path) -> Option<Result<Rc<ProjectFile>, Vec<Diagnostic>>> {
		self.with_project(path, |e| e.parsed.clone())
	}

	/// For each program of the project the files of its modules.
	fn closures(&self, path: &Path, project: &ProjectFile) -> Rc<Vec<HashSet<PathBuf>>> {
		let cached = self.with_project(path, |e| e.closures.clone()).flatten();
		if let Some((generation, c)) = cached
			&& generation == self.generation
		{
			return c;
		}
		let list: Vec<HashSet<PathBuf>> = project
			.programs
			.iter()
			.map(|p| project_file::closure(self, p).0.into_iter().collect())
			.collect();
		let list = Rc::new(list);
		let generation = self.generation;
		self.with_project(path, |e| e.closures = Some((generation, list.clone())));
		list
	}

	/// The contexts of the module in the file `path` (see the module
	/// documentation); the first one is the primary context. The error is
	/// the message for a module that cannot be analysed: there is no
	/// target for it.
	pub fn contexts_of(&self, path: &Path) -> Result<Vec<Context>, String> {
		let Some(file) = self.project_file_of(path) else {
			let Some(target) = self.config.target else {
				return Err(no_target_message());
			};
			return Ok(vec![Context {
				target,
				dirs: self.search_dirs(path),
				source: Source::Settings,
			}]);
		};
		let project = match self.project(&file) {
			Some(Ok(p)) => p,
			_ => return Err(project_errors_message(&file)),
		};
		let closures = self.closures(&file, &project);
		let imports = |p: &project_file::Program| {
			let i = project.programs.iter().position(|q| std::ptr::eq(p, q));
			i.is_some_and(|i| closures[i].contains(path))
		};
		match project_file::programs_with(&project, path, imports) {
			Use::Roots(programs) | Use::Importers(programs) => {
				let mut list: Vec<Context> = Vec::new();
				for p in programs {
					let ctx = Context {
						target: p.context.mcu.target,
						dirs: project_file::search_list(&p.root, &p.context),
						source: Source::Program {
							name: p.name.clone(),
						},
					};
					if !list.iter().any(|c| c.same_analysis(&ctx)) {
						list.push(ctx);
					}
				}
				Ok(list)
			}
			Use::Default(c) => Ok(vec![Context {
				target: c.mcu.target,
				dirs: project_file::search_list(path, c),
				source: Source::Default,
			}]),
			Use::None => Err(no_program_message(&file)),
		}
	}
}

impl SourceHost for Workspace {
	fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
		match self.text(&normalize(path)) {
			Some(t) => Ok(t.to_vec()),
			// The message of the operating system.
			None => std::fs::read(path).map_err(|e| e.to_string()),
		}
	}

	fn is_file(&self, path: &Path) -> bool {
		let path = normalize(path);
		self.docs.contains_key(&path) || self.text(&path).is_some()
	}
}

/// Gives an analysis the snapshots of the modules before it, and records
/// what it gave.
struct Loader<'a> {
	ws: &'a Workspace,
	target: &'static TargetProps,
	available: &'a HashMap<String, PathBuf>,
	used: Vec<Dep>,
	/// The modules whose snapshot was stale.
	stale: Vec<String>,
}

impl ImportLoader for Loader<'_> {
	fn load(&mut self, module: &str) -> Result<Rc<SymFile>, String> {
		let (dep, snap) = self.ws.lookup(self.target, module, self.available);
		let known = dep.path.is_some();
		if !self.used.contains(&dep) {
			self.used.push(dep);
		}
		match snap {
			Some(s) => {
				if self.ws.is_stale(&s, self.available)
					&& self
						.ws
						.unit(self.target, &s.unit.path)
						.is_some_and(|u| u.has_errors())
				{
					self.stale.push(module.to_string());
				}
				Ok(s.symfile.clone())
			}
			None if known => Err(format!("module {module} has errors")),
			// The message of the test helper of the front end. The import has
			// a message of `project::collect` at the same place.
			None => Err(format!("no symbol file for module {module}")),
		}
	}
}

/// Replaces the messages of the front end about a stale symbol file with
/// "cannot import module X: module B has errors" (B: a module whose
/// snapshot is stale).
fn replace_stale_messages(list: &[Diagnostic], stale: &[String]) -> Vec<Diagnostic> {
	list.iter()
		.map(|d| {
			let mut d = d.clone();
			if stale.is_empty()
				|| !(d.msg.contains("compiled with a different version of")
					|| d.msg.contains("compiled with different versions of"))
			{
				return d;
			}
			let Some(rest) = d.msg.strip_prefix("cannot import module ") else {
				return d;
			};
			let Some((imported, detail)) = rest.split_once(':') else {
				return d;
			};
			let culprit = detail
				.split(|c: char| !c.is_ascii_alphanumeric())
				.find(|w| stale.iter().any(|s| s == w));
			if let Some(b) = culprit {
				d.msg = format!("cannot import module {imported}: module {b} has errors");
			}
			d
		})
		.collect()
}

/// `p` is a project file (by its name).
pub fn is_project_file(p: &Path) -> bool {
	p.file_name().is_some_and(|n| n == PROJECT_FILE)
}

fn is_source(p: &Path) -> bool {
	p.extension().is_some_and(|e| e == SOURCE_EXT)
}

fn find_sources(dir: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
	if depth > MAX_DEPTH || out.len() >= MAX_INDEX_FILES {
		return;
	}
	let Ok(rd) = std::fs::read_dir(dir) else {
		return;
	};
	let mut entries: Vec<_> = rd.flatten().collect();
	entries.sort_by_key(|e| e.file_name());
	for e in entries {
		let name = e.file_name();
		let name = name.to_string_lossy();
		if name.starts_with('.') || name == "target" || name == "node_modules" {
			continue;
		}
		let Ok(ft) = e.file_type() else { continue };
		let p = normalize(&e.path());
		if ft.is_dir() {
			find_sources(&p, depth + 1, out);
		} else if is_source(&p) && out.len() < MAX_INDEX_FILES {
			out.insert(p);
		}
	}
}

// ----- analysis of changed texts (check of a rename) -----

/// A module of [`Workspace::analyse_changed`].
pub struct ScratchUnit {
	pub path: PathBuf,
	pub name: String,
	pub src: Vec<u8>,
	pub analysis: Analysis,
	/// The type map of the symbol file; empty if the module has errors.
	pub type_map: Vec<TypeId>,
	symfile: Option<Rc<SymFile>>,
}

/// The texts of the workspace with some texts replaced.
struct Overlay<'a> {
	ws: &'a Workspace,
	texts: &'a HashMap<PathBuf, Vec<u8>>,
}

impl SourceHost for Overlay<'_> {
	fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
		match self.texts.get(&normalize(path)) {
			Some(t) => Ok(t.clone()),
			None => self.ws.read(path),
		}
	}

	fn is_file(&self, path: &Path) -> bool {
		self.texts.contains_key(&normalize(path)) || self.ws.is_file(path)
	}
}

struct MapLoader<'a>(&'a HashMap<String, Rc<SymFile>>);

impl ImportLoader for MapLoader<'_> {
	fn load(&mut self, module: &str) -> Result<Rc<SymFile>, String> {
		self.0
			.get(module)
			.cloned()
			.ok_or_else(|| format!("no symbol file for module {module}"))
	}
}

impl Workspace {
	/// Analyses the module `root` and the modules that it imports in the
	/// context `ctx`, as the compiler would compile them after the
	/// replacement of the texts `texts`. The memo table is not used and not
	/// changed. `cache` keeps the results for more calls with the same texts
	/// and the same target. Returns the modules in the order of the imports.
	pub fn analyse_changed(
		&self,
		ctx: &Context,
		root: &Path,
		texts: &HashMap<PathBuf, Vec<u8>>,
		cache: &mut HashMap<PathBuf, Rc<ScratchUnit>>,
	) -> Vec<Rc<ScratchUnit>> {
		let target = ctx.target;
		let host = Overlay { ws: self, texts };
		let (units, _) = project::collect(&host, root, &ctx.dirs);
		let mut symfiles: HashMap<String, Rc<SymFile>> = HashMap::new();
		let mut out = Vec::new();
		for u in units {
			let path = normalize(&u.path);
			let su = match cache.get(&path) {
				Some(su) if su.src == u.src => su.clone(),
				_ => {
					let file = path.display().to_string();
					let analysis =
						front::analyze_module(&u.src, &file, target, &mut MapLoader(&symfiles));
					let (mut type_map, mut symfile) = (Vec::new(), None);
					if !analysis.diags.has_errors() {
						let (sf, map) = SymFile::from_module_mapped(&analysis.module);
						if let Ok(sf) = SymFile::parse(&sf.to_text()) {
							symfile = Some(Rc::new(sf));
							type_map = map;
						}
					}
					let su = Rc::new(ScratchUnit {
						path: path.clone(),
						name: u.name.clone(),
						src: u.src,
						analysis,
						type_map,
						symfile,
					});
					cache.insert(path, su.clone());
					su
				}
			};
			if let Some(sf) = &su.symfile {
				symfiles.insert(su.name.clone(), sf.clone());
			}
			out.push(su);
		}
		out
	}
}
