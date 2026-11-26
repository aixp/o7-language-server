//! The message loop.
//!
//! The crate `lsp-server` reads and writes the messages on its own threads.
//! One thread (this loop) owns all analysis data: the front end uses `Rc`,
//! and its results are not shared. The thread has a large stack, as the
//! compiler driver: the parser and the release of a deep tree are
//! recursive.
//!
//! A change of a document starts a timer. When no change comes for
//! [`Options::debounce`], the server analyses the programs of the open
//! documents and publishes the diagnostics. A request does not wait for the
//! timer: it analyses the current text at once. When no message is waiting,
//! the server analyses the files of the workspace index, one file at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use lsp_server::{
	Connection, ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError,
};
use lsp_types::notification::{
	DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
	DidChangeWorkspaceFolders, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
	Exit, Notification as _, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
	Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, PrepareRenameRequest,
	References, RegisterCapability, Rename, Request as _, SemanticTokensFullRequest, Shutdown,
	WorkspaceConfiguration, WorkspaceSymbolRequest,
};
use lsp_types::{
	CompletionOptions, CompletionParams, CompletionResponse, ConfigurationItem,
	ConfigurationParams, DidChangeConfigurationParams, DidChangeTextDocumentParams,
	DidChangeWatchedFilesParams, DidChangeWatchedFilesRegistrationOptions,
	DidChangeWorkspaceFoldersParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
	DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, FileChangeType,
	FileSystemWatcher, GlobPattern, GotoDefinitionParams, GotoDefinitionResponse, HoverParams,
	HoverProviderCapability, InitializeParams, InitializeResult, MessageType, OneOf,
	PositionEncodingKind, PrepareRenameResponse, PublishDiagnosticsParams, ReferenceParams,
	Registration, RegistrationParams, RenameOptions, RenameParams, SemanticTokensFullOptions,
	SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
	SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, ShowMessageParams,
	TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind,
	TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Url, WorkDoneProgressOptions,
	WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities, WorkspaceSymbolParams,
	WorkspaceSymbolResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::{Config, SECTIONS};
use crate::features::diagnostics::{self, FileDiagnostics};
use crate::features::{
	completion, definition, hover, references, rename, semantic_tokens, symbols,
};
use crate::vfs::{Document, Encoding, path_to_uri, uri_to_path};
use crate::workspace::{Context, Source, Unit, Workspace, is_project_file};

pub type BoxError = Box<dyn Error + Send + Sync>;

/// The stack of the analysis thread (as the compiler driver).
const STACK_SIZE: usize = 512 << 20;

/// A request of the server for the state of the analyses (for tests, for
/// the diagnosis of problems, and for a client that shows the target of a
/// document). The result is an object with:
///
/// - the counters `analyses`, `indexFiles` and `indexPending`;
/// - `settings`: the settings `target` and `searchPaths`;
/// - `projectFiles`: the project files of the open documents, each with its
///   `path`, the number of its `errors` and its `programs` (`name`, `root`,
///   `board`, `target`);
/// - `documents`: the open documents, each with its `path`, its
///   `projectFile`, its `contexts` (`source`: `program`, `default` or
///   `settings`; `program`, `board`, `target`; the first one is the primary
///   context) and the `problem` if it has no context.
pub const STATUS_REQUEST: &str = "o7/status";

#[derive(Clone, Debug)]
pub struct Options {
	/// The pause after the last change before the diagnostics are published.
	pub debounce: Duration,
}

impl Default for Options {
	fn default() -> Options {
		Options {
			debounce: Duration::from_millis(100),
		}
	}
}

/// Runs the server on the standard input and output.
pub fn run_stdio(opts: Options) -> Result<(), BoxError> {
	let (conn, io) = Connection::stdio();
	let result = run_on_thread(conn, opts);
	io.join()?;
	result
}

/// Runs the server on a new thread with a large stack and waits for its end.
pub fn run_on_thread(conn: Connection, opts: Options) -> Result<(), BoxError> {
	let t = std::thread::Builder::new()
		.name("o7-analysis".to_string())
		.stack_size(STACK_SIZE)
		.spawn(move || run(&conn, opts).map_err(|e| e.to_string()))?;
	match t.join() {
		Ok(r) => r.map_err(Into::into),
		Err(_) => Err("the analysis thread panicked".into()),
	}
}

/// Runs the server on the connection: initialization, message loop, end.
pub fn run(conn: &Connection, opts: Options) -> Result<(), BoxError> {
	let (id, params) = conn.initialize_start()?;
	let init: InitializeParams = serde_json::from_value(params)?;
	let mut server = Server::new(conn, &init, opts);
	conn.initialize_finish(id, serde_json::to_value(server.initialize_result())?)?;
	server.initialized();
	server.main_loop()
}

struct Server<'a> {
	conn: &'a Connection,
	ws: Workspace,
	enc: Encoding,
	opts: Options,
	/// When the diagnostics must be published.
	deadline: Option<Instant>,
	published: BTreeMap<PathBuf, FileDiagnostics>,
	next_id: i32,
	config_request: Option<RequestId>,
	dynamic_watch: bool,
	configuration_pull: bool,
	/// Messages about the settings, shown after the initialization.
	pending_messages: Vec<String>,
	/// The request `shutdown` came; only `exit` is expected now.
	shut_down: bool,
}

fn folders(init: &InitializeParams) -> Vec<PathBuf> {
	if let Some(f) = &init.workspace_folders
		&& !f.is_empty()
	{
		return f.iter().filter_map(|f| uri_to_path(&f.uri)).collect();
	}
	#[allow(deprecated)]
	if let Some(u) = &init.root_uri {
		return uri_to_path(u).into_iter().collect();
	}
	#[allow(deprecated)]
	init.root_path
		.as_ref()
		.map(|p| vec![crate::vfs::normalize(std::path::Path::new(p))])
		.unwrap_or_default()
}

impl<'a> Server<'a> {
	fn new(conn: &'a Connection, init: &InitializeParams, opts: Options) -> Server<'a> {
		let caps = &init.capabilities;
		let utf8 = caps
			.general
			.as_ref()
			.and_then(|g| g.position_encodings.as_ref())
			.is_some_and(|e| e.contains(&PositionEncodingKind::UTF8));
		let ws_caps = caps.workspace.as_ref();
		let dynamic_watch = ws_caps
			.and_then(|w| w.did_change_watched_files.as_ref())
			.and_then(|d| d.dynamic_registration)
			.unwrap_or(false);
		let configuration_pull = ws_caps.and_then(|w| w.configuration).unwrap_or(false);
		let mut config = Config::default();
		let mut pending_messages = Vec::new();
		if let Some(opts) = &init.initialization_options {
			pending_messages = config.update(opts);
		}
		Server {
			conn,
			ws: Workspace::new(config, folders(init)),
			enc: if utf8 {
				Encoding::Utf8
			} else {
				Encoding::Utf16
			},
			opts,
			deadline: None,
			published: BTreeMap::new(),
			next_id: 0,
			config_request: None,
			dynamic_watch,
			configuration_pull,
			pending_messages,
			shut_down: false,
		}
	}

	fn initialize_result(&self) -> InitializeResult {
		let capabilities = ServerCapabilities {
			position_encoding: Some(match self.enc {
				Encoding::Utf8 => PositionEncodingKind::UTF8,
				Encoding::Utf16 => PositionEncodingKind::UTF16,
			}),
			text_document_sync: Some(TextDocumentSyncCapability::Options(
				TextDocumentSyncOptions {
					open_close: Some(true),
					change: Some(TextDocumentSyncKind::INCREMENTAL),
					save: Some(TextDocumentSyncSaveOptions::Supported(true)),
					..TextDocumentSyncOptions::default()
				},
			)),
			definition_provider: Some(OneOf::Left(true)),
			hover_provider: Some(HoverProviderCapability::Simple(true)),
			document_symbol_provider: Some(OneOf::Left(true)),
			references_provider: Some(OneOf::Left(true)),
			workspace_symbol_provider: Some(OneOf::Left(true)),
			completion_provider: Some(CompletionOptions {
				trigger_characters: Some(vec![".".to_string()]),
				..CompletionOptions::default()
			}),
			rename_provider: Some(OneOf::Right(RenameOptions {
				prepare_provider: Some(true),
				work_done_progress_options: WorkDoneProgressOptions::default(),
			})),
			semantic_tokens_provider: Some(
				SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
					legend: semantic_tokens::legend(),
					full: Some(SemanticTokensFullOptions::Bool(true)),
					range: None,
					work_done_progress_options: WorkDoneProgressOptions::default(),
				}),
			),
			workspace: Some(WorkspaceServerCapabilities {
				workspace_folders: Some(WorkspaceFoldersServerCapabilities {
					supported: Some(true),
					change_notifications: Some(OneOf::Left(true)),
				}),
				file_operations: None,
			}),
			..ServerCapabilities::default()
		};
		InitializeResult {
			capabilities,
			server_info: Some(ServerInfo {
				name: "o7-language-server".to_string(),
				version: Some(crate::version_text()),
			}),
		}
	}

	/// The work after the notification `initialized`.
	fn initialized(&mut self) {
		if self.dynamic_watch {
			let params = RegistrationParams {
				registrations: vec![Registration {
					id: "o7-watch-sources".to_string(),
					method: DidChangeWatchedFiles::METHOD.to_string(),
					register_options: serde_json::to_value(
						DidChangeWatchedFilesRegistrationOptions {
							watchers: ["**/*.Mod", "**/o7.toml"]
								.map(|glob| FileSystemWatcher {
									glob_pattern: GlobPattern::String(glob.to_string()),
									kind: None,
								})
								.to_vec(),
						},
					)
					.ok(),
				}],
			};
			self.request(RegisterCapability::METHOD, params);
		}
		if self.configuration_pull {
			self.request_configuration();
		}
		for m in std::mem::take(&mut self.pending_messages) {
			self.show_message(MessageType::WARNING, m);
		}
		self.ws.start_index();
	}

	fn main_loop(&mut self) -> Result<(), BoxError> {
		loop {
			let timeout = match self.deadline {
				_ if self.shut_down => Duration::from_secs(3600),
				Some(d) => d.saturating_duration_since(Instant::now()),
				None if self.ws.index_pending() => Duration::ZERO,
				None => Duration::from_secs(3600),
			};
			match self.conn.receiver.recv_timeout(timeout) {
				Ok(Message::Request(req)) => {
					if req.method == Shutdown::METHOD {
						self.shut_down = true;
						self.send(Response::new_ok(req.id, Value::Null).into());
					} else if self.shut_down {
						let msg = "the server was shut down".to_string();
						self.send(
							Response::new_err(req.id, ErrorCode::InvalidRequest as i32, msg).into(),
						);
					} else {
						self.on_request(req);
					}
				}
				Ok(Message::Notification(n)) => {
					if n.method == Exit::METHOD {
						return Ok(());
					}
					self.on_notification(n);
				}
				Ok(Message::Response(r)) => self.on_response(r),
				Err(RecvTimeoutError::Timeout) => {
					if self.deadline.is_some_and(|d| d <= Instant::now()) {
						self.guard("publishing the diagnostics", |s| s.publish());
					} else {
						self.guard("indexing", |s| {
							s.ws.index_step();
						});
					}
				}
				Err(RecvTimeoutError::Disconnected) => return Ok(()),
			}
		}
	}

	/// Runs `f`; a panic is reported to the client and does not end the
	/// server.
	fn guard(&mut self, what: &str, f: impl FnOnce(&mut Self)) {
		if let Err(p) = catch_unwind(AssertUnwindSafe(|| f(self))) {
			let msg = format!(
				"o7-language-server: internal error while {what}: {}",
				panic_text(&p)
			);
			eprintln!("{msg}");
			self.deadline = None;
			self.show_message(MessageType::ERROR, msg);
		}
	}

	// ----- sending -----

	fn send(&self, msg: Message) {
		// The client is gone if this fails; the loop ends at the next receive.
		let _ = self.conn.sender.send(msg);
	}

	fn notify<P: Serialize>(&self, method: &str, params: P) {
		self.send(Notification::new(method.to_string(), params).into());
	}

	fn request<P: Serialize>(&mut self, method: &str, params: P) -> RequestId {
		self.next_id += 1;
		let id = RequestId::from(format!("o7-{}", self.next_id));
		self.send(Request::new(id.clone(), method.to_string(), params).into());
		id
	}

	fn show_message(&self, typ: MessageType, message: String) {
		self.notify(ShowMessage::METHOD, ShowMessageParams { typ, message });
	}

	fn request_configuration(&mut self) {
		let params = ConfigurationParams {
			items: SECTIONS
				.iter()
				.map(|s| ConfigurationItem {
					scope_uri: None,
					section: Some(s.to_string()),
				})
				.collect(),
		};
		self.config_request = Some(self.request(WorkspaceConfiguration::METHOD, params));
	}

	/// Starts or restarts the timer of the diagnostics.
	fn schedule(&mut self, delay: Duration) {
		let t = Instant::now() + delay;
		self.deadline = Some(match self.deadline {
			Some(d) if delay.is_zero() => d.min(t),
			_ => t,
		});
	}

	/// Analyses the programs of the open documents and publishes the
	/// diagnostics that changed.
	fn publish(&mut self) {
		self.deadline = None;
		let new = diagnostics::collect(&mut self.ws, self.enc);
		for (path, d) in &new {
			if self.published.get(path) == Some(d) {
				continue;
			}
			if let Some(uri) = path_to_uri(path) {
				self.notify(
					PublishDiagnostics::METHOD,
					PublishDiagnosticsParams::new(uri, d.list.clone(), d.version),
				);
			}
		}
		for path in self.published.keys() {
			if !new.contains_key(path)
				&& let Some(uri) = path_to_uri(path)
			{
				self.notify(
					PublishDiagnostics::METHOD,
					PublishDiagnosticsParams::new(uri, Vec::new(), None),
				);
			}
		}
		self.published = new;
	}

	// ----- notifications -----

	fn on_notification(&mut self, n: Notification) {
		let method = n.method.clone();
		let result = catch_unwind(AssertUnwindSafe(|| self.notification(n)));
		match result {
			Ok(Ok(())) => {}
			Ok(Err(e)) => eprintln!("o7-language-server: {method}: {e}"),
			Err(p) => {
				let msg = format!(
					"o7-language-server: internal error in {method}: {}",
					panic_text(&p)
				);
				eprintln!("{msg}");
				self.show_message(MessageType::ERROR, msg);
			}
		}
	}

	fn notification(&mut self, n: Notification) -> Result<(), String> {
		match n.method.as_str() {
			DidOpenTextDocument::METHOD => {
				let p: DidOpenTextDocumentParams = params(n.params)?;
				let d = p.text_document;
				let path = file_path(&d.uri)?;
				self.ws.open(Document::new(d.uri, path, d.version, d.text));
				self.schedule(Duration::ZERO);
			}
			DidChangeTextDocument::METHOD => {
				let p: DidChangeTextDocumentParams = params(n.params)?;
				let path = file_path(&p.text_document.uri)?;
				let enc = self.enc;
				let doc = self
					.ws
					.docs
					.get_mut(&path)
					.ok_or_else(|| format!("{} is not open", p.text_document.uri))?;
				for c in &p.content_changes {
					doc.apply(c, enc);
				}
				doc.version = p.text_document.version;
				self.ws.touch();
				self.schedule(self.opts.debounce);
			}
			DidCloseTextDocument::METHOD => {
				let p: DidCloseTextDocumentParams = params(n.params)?;
				self.ws.close(&file_path(&p.text_document.uri)?);
				self.schedule(Duration::ZERO);
			}
			DidSaveTextDocument::METHOD => {
				let p: DidSaveTextDocumentParams = params(n.params)?;
				self.ws
					.file_changed(&file_path(&p.text_document.uri)?, true);
				self.schedule(self.opts.debounce);
			}
			DidChangeWatchedFiles::METHOD => {
				let p: DidChangeWatchedFilesParams = params(n.params)?;
				for c in p.changes {
					if let Some(path) = uri_to_path(&c.uri) {
						self.ws
							.file_changed(&path, c.typ != FileChangeType::DELETED);
					}
				}
				self.schedule(self.opts.debounce);
			}
			DidChangeConfiguration::METHOD => {
				let p: DidChangeConfigurationParams = params(n.params)?;
				let empty = match &p.settings {
					Value::Null => true,
					Value::Object(o) => o.is_empty(),
					_ => false,
				};
				if empty && self.configuration_pull {
					self.request_configuration();
				} else {
					self.apply_settings(&p.settings);
				}
			}
			DidChangeWorkspaceFolders::METHOD => {
				let p: DidChangeWorkspaceFoldersParams = params(n.params)?;
				let removed: Vec<PathBuf> = p
					.event
					.removed
					.iter()
					.filter_map(|f| uri_to_path(&f.uri))
					.collect();
				self.ws.folders.retain(|f| !removed.contains(f));
				for f in p.event.added.iter().filter_map(|f| uri_to_path(&f.uri)) {
					if !self.ws.folders.contains(&f) {
						self.ws.folders.push(f);
					}
				}
				self.ws.touch();
				self.ws.start_index();
				self.schedule(Duration::ZERO);
			}
			_ => {}
		}
		Ok(())
	}

	fn apply_settings(&mut self, value: &Value) {
		let mut c = self.ws.config.clone();
		for m in c.update(value) {
			self.show_message(MessageType::WARNING, m);
		}
		if c != self.ws.config {
			self.ws.set_config(c);
			self.schedule(Duration::ZERO);
		}
	}

	fn on_response(&mut self, r: Response) {
		if Some(&r.id) != self.config_request.as_ref() {
			return;
		}
		self.config_request = None;
		if let Some(Value::Array(items)) = r.result {
			for item in items.iter().filter(|v| !v.is_null()) {
				self.apply_settings(item);
			}
		}
	}

	// ----- requests -----

	fn on_request(&mut self, req: Request) {
		let id = req.id.clone();
		let method = req.method.clone();
		let result = catch_unwind(AssertUnwindSafe(|| self.dispatch(req)));
		let resp = match result {
			Ok(Ok(value)) => Response::new_ok(id, value),
			Ok(Err(error)) => Response {
				id,
				result: None,
				error: Some(error),
			},
			Err(p) => {
				let msg = format!("internal error in {method}: {}", panic_text(&p));
				eprintln!("o7-language-server: {msg}");
				Response::new_err(id, ErrorCode::InternalError as i32, msg)
			}
		};
		self.send(resp.into());
	}

	/// The current analysis of the document `uri` in its primary context.
	/// Without a target the request fails with the message "no target".
	fn unit(&mut self, uri: &Url) -> Result<Rc<Unit>, ResponseError> {
		let path = file_path(uri).map_err(invalid_params)?;
		let contexts = self.ws.contexts_of(&path).map_err(failed)?;
		let cannot = || failed(format!("{} cannot be analysed", path.display()));
		let ctx = contexts.first().ok_or_else(cannot)?;
		self.ws.check_root(ctx, &path);
		self.ws.unit(ctx.target, &path).cloned().ok_or_else(cannot)
	}

	/// The result of the request `o7/status`.
	fn status(&self) -> Value {
		let text = |p: &std::path::Path| p.display().to_string();
		let context = |c: &Context| {
			let (source, program, board) = match &c.source {
				Source::Program { name, board } => ("program", Some(name.as_str()), Some(*board)),
				Source::Default { board } => ("default", None, Some(*board)),
				Source::Settings => ("settings", None, None),
			};
			json!({
				"source": source,
				"program": program,
				"board": board,
				"target": c.target.name,
			})
		};
		let mut project_files = BTreeSet::new();
		let mut documents = Vec::new();
		let paths: BTreeSet<&PathBuf> = self.ws.docs.keys().collect();
		for path in paths.into_iter().filter(|p| !is_project_file(p)) {
			let file = self.ws.project_file_of(path);
			let (contexts, problem) = match self.ws.contexts_of(path) {
				Ok(list) => (list.iter().map(context).collect(), None),
				Err(msg) => (Vec::new(), Some(msg)),
			};
			documents.push(json!({
				"path": text(path),
				"projectFile": file.as_deref().map(text),
				"contexts": contexts,
				"problem": problem,
			}));
			project_files.extend(file);
		}
		let project_files: Vec<Value> = project_files
			.iter()
			.map(|file| {
				let (errors, programs) = match self.ws.project(file) {
					Some(Ok(project)) => (
						0,
						project
							.programs
							.iter()
							.map(|p| {
								json!({
									"name": p.name,
									"root": text(&p.root),
									"board": p.context.board,
									"target": p.context.target.name,
								})
							})
							.collect(),
					),
					Some(Err(list)) => (list.len(), Vec::new()),
					None => (1, Vec::new()),
				};
				json!({ "path": text(file), "errors": errors, "programs": programs })
			})
			.collect();
		json!({
			"analyses": self.ws.analyses,
			"indexFiles": self.ws.index_files().len(),
			"indexPending": self.ws.index_pending(),
			"settings": {
				"target": self.ws.config.target.map(|t| t.name),
				"searchPaths": self.ws.config.search_paths,
			},
			"projectFiles": project_files,
			"documents": documents,
		})
	}

	/// The current analysis of the document `uri`, after the analysis of
	/// the complete workspace.
	fn unit_in_index(&mut self, uri: &Url) -> Result<Rc<Unit>, ResponseError> {
		self.ws.refresh_index();
		self.unit(uri)
	}

	fn dispatch(&mut self, req: Request) -> Result<Value, ResponseError> {
		let enc = self.enc;
		match req.method.as_str() {
			GotoDefinition::METHOD => {
				let p: GotoDefinitionParams = params(req.params).map_err(invalid_params)?;
				let TextDocumentPositionParams {
					text_document,
					position,
				} = p.text_document_position_params;
				let unit = self.unit(&text_document.uri)?;
				let r = definition::definition(&self.ws, &unit, position, enc)
					.map(GotoDefinitionResponse::Scalar);
				result(r)
			}
			HoverRequest::METHOD => {
				let p: HoverParams = params(req.params).map_err(invalid_params)?;
				let TextDocumentPositionParams {
					text_document,
					position,
				} = p.text_document_position_params;
				let unit = self.unit(&text_document.uri)?;
				result(hover::hover(&self.ws, &unit, position, enc))
			}
			DocumentSymbolRequest::METHOD => {
				let p: DocumentSymbolParams = params(req.params).map_err(invalid_params)?;
				let unit = self.unit(&p.text_document.uri)?;
				result(Some(DocumentSymbolResponse::Nested(
					symbols::document_symbols(&unit, enc),
				)))
			}
			References::METHOD => {
				let p: ReferenceParams = params(req.params).map_err(invalid_params)?;
				let TextDocumentPositionParams {
					text_document,
					position,
				} = p.text_document_position;
				let unit = self.unit_in_index(&text_document.uri)?;
				let list = references::references(
					&self.ws,
					&unit,
					position,
					p.context.include_declaration,
					enc,
				);
				result(Some(list))
			}
			WorkspaceSymbolRequest::METHOD => {
				let p: WorkspaceSymbolParams = params(req.params).map_err(invalid_params)?;
				self.ws.refresh_index();
				let list = symbols::workspace_symbols(&self.ws, &p.query, enc);
				result(Some(WorkspaceSymbolResponse::Flat(list)))
			}
			Completion::METHOD => {
				let p: CompletionParams = params(req.params).map_err(invalid_params)?;
				let TextDocumentPositionParams {
					text_document,
					position,
				} = p.text_document_position;
				let unit = self.unit(&text_document.uri)?;
				let items = completion::completion(&self.ws, &unit, position, enc);
				result(Some(CompletionResponse::Array(items)))
			}
			PrepareRenameRequest::METHOD => {
				let p: TextDocumentPositionParams = params(req.params).map_err(invalid_params)?;
				let unit = self.unit(&p.text_document.uri)?;
				let (range, placeholder) =
					rename::prepare(&self.ws, &unit, p.position, enc).map_err(failed)?;
				result(Some(PrepareRenameResponse::RangeWithPlaceholder {
					range,
					placeholder,
				}))
			}
			Rename::METHOD => {
				let p: RenameParams = params(req.params).map_err(invalid_params)?;
				let TextDocumentPositionParams {
					text_document,
					position,
				} = p.text_document_position;
				let unit = self.unit_in_index(&text_document.uri)?;
				let edit =
					rename::rename(&self.ws, &unit, position, &p.new_name, enc).map_err(failed)?;
				result(Some(edit))
			}
			SemanticTokensFullRequest::METHOD => {
				let p: SemanticTokensParams = params(req.params).map_err(invalid_params)?;
				let unit = self.unit(&p.text_document.uri)?;
				result(Some(SemanticTokensResult::Tokens(semantic_tokens::full(
					&unit, enc,
				))))
			}
			STATUS_REQUEST => Ok(self.status()),
			_ => Err(ResponseError {
				code: ErrorCode::MethodNotFound as i32,
				message: format!("unknown request {}", req.method),
				data: None,
			}),
		}
	}
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, String> {
	serde_json::from_value(v).map_err(|e| format!("invalid parameters: {e}"))
}

fn result<T: Serialize>(v: T) -> Result<Value, ResponseError> {
	serde_json::to_value(v).map_err(|e| failed(e.to_string()))
}

fn file_path(uri: &Url) -> Result<PathBuf, String> {
	uri_to_path(uri).ok_or_else(|| format!("{uri} is not a file"))
}

fn invalid_params(message: String) -> ResponseError {
	ResponseError {
		code: ErrorCode::InvalidParams as i32,
		message,
		data: None,
	}
}

fn failed(message: impl Into<String>) -> ResponseError {
	ResponseError {
		code: ErrorCode::RequestFailed as i32,
		message: message.into(),
		data: None,
	}
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
	p.downcast_ref::<&str>()
		.map(|s| s.to_string())
		.or_else(|| p.downcast_ref::<String>().cloned())
		.unwrap_or_else(|| "panic".to_string())
}
