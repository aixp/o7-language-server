//! Robustness: requests on texts that are not complete give no internal
//! error, and the binary works on the standard input and output.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::*;
use lsp_types::request::{
	Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, PrepareRenameRequest,
	SemanticTokensFullRequest,
};
use lsp_types::{
	CompletionParams, DocumentSymbolParams, GotoDefinitionParams, HoverParams, Position,
	SemanticTokensParams, TextDocumentIdentifier, Url,
};
use serde_json::{Value, json};

fn requests(c: &mut Client, u: &Url, text: &str) {
	let doc = TextDocumentIdentifier::new(u.clone());
	c.request::<DocumentSymbolRequest>(DocumentSymbolParams {
		text_document: doc.clone(),
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	c.request::<SemanticTokensFullRequest>(SemanticTokensParams {
		text_document: doc,
		work_done_progress_params: Default::default(),
		partial_result_params: Default::default(),
	});
	let lines: Vec<&str> = text.split('\n').collect();
	let last = Position::new(
		lines.len() as u32 - 1,
		lines.last().map_or(0, |l| l.encode_utf16().count()) as u32,
	);
	let mut positions = vec![last, Position::new(0, 0), Position::new(9999, 9999)];
	for (i, l) in lines.iter().enumerate().step_by(7) {
		positions.push(Position::new(i as u32, (l.len() / 2) as u32));
	}
	for p in positions {
		let tp = pos_params(u, p);
		c.request::<Completion>(CompletionParams {
			text_document_position: tp.clone(),
			work_done_progress_params: Default::default(),
			partial_result_params: Default::default(),
			context: None,
		});
		c.request::<HoverRequest>(HoverParams {
			text_document_position_params: tp.clone(),
			work_done_progress_params: Default::default(),
		});
		c.request::<GotoDefinition>(GotoDefinitionParams {
			text_document_position_params: tp.clone(),
			work_done_progress_params: Default::default(),
			partial_result_params: Default::default(),
		});
		// A refusal is an answer; an internal error is not.
		if let Err(e) = c.try_request::<PrepareRenameRequest>(tp) {
			assert_ne!(
				e.code,
				lsp_server::ErrorCode::InternalError as i32,
				"{}",
				e.message
			);
		}
	}
}

#[test]
fn cut_texts() {
	let mut files: Vec<PathBuf> = ["Lib.Mod", "Main.Mod"]
		.iter()
		.map(|f| {
			PathBuf::from(env!("CARGO_MANIFEST_DIR"))
				.join("tests/fixtures/nav")
				.join(f)
		})
		.collect();
	if let Some(repo) = compiler_repo() {
		files.push(repo.join("lib/Micro/MicroSTM32F4Ports.Mod"));
		files.push(repo.join("tests/lib/Out.Mod"));
	}
	let mut c = Client::start(Setup {
		folders: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nav")],
		debounce: std::time::Duration::ZERO,
		..Setup::default()
	});
	for f in files {
		let full = std::fs::read_to_string(&f).unwrap();
		let u = c.open_text(&f, "");
		let n = 40;
		for i in 0..=n {
			let mut cut = full.len() * i / n;
			while !full.is_char_boundary(cut) {
				cut -= 1;
			}
			// The start of the text, and the text with a hole.
			for t in [
				full[..cut].to_string(),
				format!("{}{}", &full[..cut / 2], &full[cut..]),
			] {
				c.change(&u, None, &t);
				c.sync(&u);
				requests(&mut c, &u, &t);
			}
		}
		c.close(&u);
	}
	c.shutdown();
}

fn write_message(w: &mut impl Write, v: &Value) {
	let body = v.to_string();
	write!(w, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
	w.flush().unwrap();
}

fn read_message(r: &mut impl BufRead) -> Value {
	let mut len = 0;
	loop {
		let mut line = String::new();
		r.read_line(&mut line).unwrap();
		let line = line.trim_end();
		if line.is_empty() {
			break;
		}
		if let Some(v) = line.strip_prefix("Content-Length: ") {
			len = v.parse().unwrap();
		}
	}
	let mut body = vec![0; len];
	r.read_exact(&mut body).unwrap();
	serde_json::from_slice(&body).unwrap()
}

#[test]
fn binary_on_standard_input_and_output() {
	let exe = env!("CARGO_BIN_EXE_o7-language-server");
	let out = Command::new(exe).arg("--version").output().unwrap();
	let v = String::from_utf8(out.stdout).unwrap();
	assert!(
		v.starts_with("o7-language-server 0.1.0 (o7-front 0.2.1, compiler commit "),
		"{v}"
	);

	let dir = temp_dir("stdio");
	write_files(&dir, &[("M.Mod", "MODULE M; BEGIN x END M.")]);
	let mut p = Command::new(exe)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.spawn()
		.unwrap();
	let mut w = p.stdin.take().unwrap();
	let mut r = BufReader::new(p.stdout.take().unwrap());
	write_message(
		&mut w,
		&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
	);
	let init = read_message(&mut r);
	assert_eq!(init["id"], 1);
	assert_eq!(init["result"]["serverInfo"]["name"], "o7-language-server");
	write_message(
		&mut w,
		&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
	);
	let uri = uri(&dir.join("M.Mod")).to_string();
	write_message(
		&mut w,
		&json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {
			"uri": uri, "languageId": "oberon", "version": 1, "text": "MODULE M; BEGIN x END M."}}}),
	);
	let d = loop {
		let m = read_message(&mut r);
		if m["method"] == "textDocument/publishDiagnostics" {
			break m;
		}
	};
	assert_eq!(
		d["params"]["diagnostics"][0]["message"],
		"`x` is not declared"
	);
	write_message(
		&mut w,
		&json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
	);
	let s = read_message(&mut r);
	assert_eq!(s["id"], 2);
	write_message(&mut w, &json!({"jsonrpc": "2.0", "method": "exit"}));
	drop(w);
	assert!(p.wait().unwrap().success());
}
