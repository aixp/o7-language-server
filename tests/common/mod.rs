//! A client for the protocol tests. The server runs in the same process on
//! a connection in memory (`lsp_server::Connection::memory`).
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response, ResponseError};
use lsp_types::notification::{
	DidChangeConfiguration, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
	Initialized, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Initialize, Shutdown};
use lsp_types::{
	ClientCapabilities, Diagnostic, DidChangeConfigurationParams, DidChangeTextDocumentParams,
	DidCloseTextDocumentParams, DidOpenTextDocumentParams, GeneralClientCapabilities,
	InitializeParams, InitializeResult, Position, PositionEncodingKind, PublishDiagnosticsParams,
	Range, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
	TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use o7_language_server::server::{self, Options, STATUS_REQUEST};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(60);

/// The root of the compiler repository: `$O7C_REPO`, else the sibling
/// directory `oberon-compiler`. `None` if it is not there.
pub fn compiler_repo() -> Option<PathBuf> {
	let p = match std::env::var_os("O7C_REPO") {
		Some(p) => PathBuf::from(p),
		None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../oberon-compiler"),
	};
	let p = p.canonicalize().ok()?;
	p.join("lib/Micro/MicroSTM32F4.Mod").is_file().then_some(p)
}

/// The search paths of the tests of the compiler.
pub fn compiler_search_paths(repo: &Path) -> Vec<String> {
	["tests/lib", "lib/Micro", "lib/Micro/hsebyp"]
		.iter()
		.map(|d| repo.join(d).display().to_string())
		.collect()
}

/// A new empty directory for one test.
pub fn temp_dir(name: &str) -> PathBuf {
	static N: AtomicU32 = AtomicU32::new(0);
	let n = N.fetch_add(1, Ordering::Relaxed);
	let d =
		Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{n}", std::process::id()));
	let _ = std::fs::remove_dir_all(&d);
	std::fs::create_dir_all(&d).expect("temporary directory");
	d
}

/// Writes the files `(relative path, text)` below `dir`.
pub fn write_files(dir: &Path, files: &[(&str, &str)]) {
	for (p, text) in files {
		let p = dir.join(p);
		std::fs::create_dir_all(p.parent().unwrap()).unwrap();
		std::fs::write(p, text).unwrap();
	}
}

pub fn uri(p: &Path) -> Url {
	Url::from_file_path(p).expect("absolute path")
}

/// The position of the `n`-th (from 0) occurrence of `needle` in `text`,
/// plus `offset` bytes (UTF-16 = bytes for ASCII lines).
pub fn find(text: &str, needle: &str, n: usize, offset: u32) -> Position {
	let mut from = 0;
	let mut at = None;
	for _ in 0..=n {
		let i = text[from..]
			.find(needle)
			.unwrap_or_else(|| panic!("{needle:?} not found"));
		at = Some(from + i);
		from += i + 1;
	}
	let at = at.unwrap();
	let line = text[..at].matches('\n').count() as u32;
	let col = at - text[..at].rfind('\n').map_or(0, |i| i + 1);
	Position::new(line, col as u32 + offset)
}

pub fn pos_params(uri: &Url, p: Position) -> TextDocumentPositionParams {
	TextDocumentPositionParams::new(TextDocumentIdentifier::new(uri.clone()), p)
}

pub struct Setup {
	pub folders: Vec<PathBuf>,
	pub options: Value,
	pub utf8: bool,
	pub debounce: Duration,
	/// The answer to `workspace/configuration` (if `Some`, the client says
	/// that it supports the request).
	pub pull_settings: Option<Value>,
}

impl Default for Setup {
	fn default() -> Setup {
		Setup {
			folders: Vec::new(),
			options: Value::Null,
			utf8: false,
			debounce: Duration::from_millis(20),
			pull_settings: None,
		}
	}
}

pub struct Client {
	conn: Connection,
	thread: Option<JoinHandle<()>>,
	next_id: i32,
	queue: VecDeque<Notification>,
	/// The last published diagnostics of each file.
	pub diags: HashMap<Url, PublishDiagnosticsParams>,
	pub init: InitializeResult,
	pull_settings: Option<Value>,
	versions: HashMap<Url, i32>,
}

impl Client {
	pub fn start(setup: Setup) -> Client {
		let (server_conn, conn) = Connection::memory();
		let opts = Options {
			debounce: setup.debounce,
		};
		let thread = std::thread::spawn(move || {
			server::run_on_thread(server_conn, opts).expect("server");
		});
		let mut c = Client {
			conn,
			thread: Some(thread),
			next_id: 0,
			queue: VecDeque::new(),
			diags: HashMap::new(),
			init: InitializeResult::default(),
			pull_settings: setup.pull_settings.clone(),
			versions: HashMap::new(),
		};
		let mut caps: ClientCapabilities = serde_json::from_value(json!({
			"workspace": {
				"didChangeWatchedFiles": { "dynamicRegistration": true },
				"configuration": setup.pull_settings.is_some(),
			}
		}))
		.unwrap();
		if setup.utf8 {
			caps.general = Some(GeneralClientCapabilities {
				position_encodings: Some(vec![
					PositionEncodingKind::UTF8,
					PositionEncodingKind::UTF16,
				]),
				..GeneralClientCapabilities::default()
			});
		}
		#[allow(deprecated)]
		let params = InitializeParams {
			capabilities: caps,
			initialization_options: Some(setup.options.clone()),
			workspace_folders: Some(
				setup
					.folders
					.iter()
					.map(|f| WorkspaceFolder {
						uri: uri(f),
						name: f.display().to_string(),
					})
					.collect(),
			),
			..InitializeParams::default()
		};
		c.init = c.request::<Initialize>(params);
		c.notify::<Initialized>(lsp_types::InitializedParams {});
		c
	}

	fn send(&self, m: Message) {
		self.conn.sender.send(m).expect("send");
	}

	/// Receives one message. Requests of the server are answered here.
	fn receive(&mut self) -> Message {
		let m = self
			.conn
			.receiver
			.recv_timeout(TIMEOUT)
			.expect("no message from the server");
		if let Message::Request(r) = &m {
			let result = match r.method.as_str() {
				"workspace/configuration" => {
					let n = r.params["items"].as_array().map_or(1, Vec::len);
					let mut v = vec![Value::Null; n];
					if let Some(s) = &self.pull_settings {
						v[0] = s.clone();
					}
					Value::Array(v)
				}
				_ => Value::Null,
			};
			self.send(Response::new_ok(r.id.clone(), result).into());
		}
		if let Message::Notification(n) = &m
			&& n.method == PublishDiagnostics::METHOD
		{
			let p: PublishDiagnosticsParams = serde_json::from_value(n.params.clone()).unwrap();
			self.diags.insert(p.uri.clone(), p);
		}
		m
	}

	pub fn raw_request(&mut self, method: &str, params: Value) -> Result<Value, ResponseError> {
		self.next_id += 1;
		let id = RequestId::from(self.next_id);
		self.send(Request::new(id.clone(), method.to_string(), params).into());
		loop {
			match self.receive() {
				Message::Response(r) if r.id == id => {
					return match r.error {
						Some(e) => Err(e),
						None => Ok(r.result.unwrap_or(Value::Null)),
					};
				}
				Message::Notification(n) => self.queue.push_back(n),
				_ => {}
			}
		}
	}

	pub fn try_request<R: lsp_types::request::Request>(
		&mut self,
		params: R::Params,
	) -> Result<R::Result, ResponseError> {
		let v = self.raw_request(R::METHOD, serde_json::to_value(params).unwrap())?;
		Ok(serde_json::from_value(v).expect("result"))
	}

	pub fn request<R: lsp_types::request::Request>(&mut self, params: R::Params) -> R::Result {
		match self.try_request::<R>(params) {
			Ok(r) => r,
			Err(e) => panic!("{} failed: {}", R::METHOD, e.message),
		}
	}

	pub fn notify<N: lsp_types::notification::Notification>(&mut self, params: N::Params) {
		self.send(Notification::new(N::METHOD.to_string(), params).into());
	}

	/// The counters of `o7/status`. The answer also shows that the server
	/// processed all earlier messages.
	pub fn status(&mut self) -> Value {
		self.raw_request(STATUS_REQUEST, Value::Null)
			.expect("status")
	}

	pub fn analyses(&mut self) -> u64 {
		self.status()["analyses"].as_u64().unwrap()
	}

	/// Waits for the diagnostics of `uri` for the version `version` of the
	/// document, then for the end of the publication.
	pub fn sync(&mut self, uri: &Url) -> Vec<Diagnostic> {
		let version = self.versions.get(uri).copied();
		loop {
			if let Some(p) = self.diags.get(uri)
				&& p.version == version
			{
				break;
			}
			if let Message::Notification(n) = self.receive() {
				self.queue.push_back(n);
			}
		}
		// All notifications of one publication come before the answer to a
		// later request.
		self.status();
		self.diags[uri].diagnostics.clone()
	}

	/// Waits until a publication for `uri` satisfies `f`.
	pub fn wait_diags(&mut self, uri: &Url, f: impl Fn(&[Diagnostic]) -> bool) -> Vec<Diagnostic> {
		loop {
			if let Some(p) = self.diags.get(uri)
				&& f(&p.diagnostics)
			{
				return p.diagnostics.clone();
			}
			if let Message::Notification(n) = self.receive() {
				self.queue.push_back(n);
			}
		}
	}

	/// The last published diagnostics of `uri` (empty if none).
	pub fn last_diags(&self, uri: &Url) -> Vec<Diagnostic> {
		self.diags
			.get(uri)
			.map(|p| p.diagnostics.clone())
			.unwrap_or_default()
	}

	pub fn open_text(&mut self, path: &Path, text: &str) -> Url {
		let u = uri(path);
		self.versions.insert(u.clone(), 1);
		self.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
			text_document: TextDocumentItem::new(
				u.clone(),
				"oberon".to_string(),
				1,
				text.to_string(),
			),
		});
		u
	}

	pub fn open(&mut self, path: &Path) -> Url {
		let text =
			std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
		self.open_text(path, &text)
	}

	pub fn close(&mut self, u: &Url) {
		self.versions.remove(u);
		self.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
			text_document: TextDocumentIdentifier::new(u.clone()),
		});
	}

	/// Sends one change; `range` `None` replaces the complete text.
	pub fn change(&mut self, u: &Url, range: Option<Range>, text: &str) {
		self.changes(u, vec![(range, text.to_string())]);
	}

	pub fn changes(&mut self, u: &Url, list: Vec<(Option<Range>, String)>) {
		let v = self.versions.entry(u.clone()).or_insert(1);
		*v += 1;
		let version = *v;
		self.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
			text_document: VersionedTextDocumentIdentifier::new(u.clone(), version),
			content_changes: list
				.into_iter()
				.map(|(range, text)| TextDocumentContentChangeEvent {
					range,
					range_length: None,
					text,
				})
				.collect(),
		});
	}

	pub fn configure(&mut self, settings: Value) {
		self.notify::<DidChangeConfiguration>(DidChangeConfigurationParams { settings });
	}

	pub fn shutdown(mut self) {
		self.request::<Shutdown>(());
		self.send(Notification::new("exit".to_string(), Value::Null).into());
		if let Some(t) = self.thread.take() {
			t.join().expect("server thread");
		}
	}
}

/// The diagnostics as `line:col: message` (LSP positions from 0), sorted.
pub fn texts(d: &[Diagnostic]) -> Vec<String> {
	let mut v: Vec<String> = d
		.iter()
		.map(|d| {
			format!(
				"{}:{}: {}",
				d.range.start.line, d.range.start.character, d.message
			)
		})
		.collect();
	v.sort();
	v
}
