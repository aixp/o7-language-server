//! Documents and the conversion of positions.
//!
//! Three kinds of positions occur in the server:
//!
//! - byte offsets into the text (the internal form);
//! - positions of the front end ([`Pos`]): line and column from 1, the
//!   column counts bytes, only `\n` ends a line, and a UTF-8 byte order mark
//!   at the start of the text is not counted;
//! - LSP positions: line and character from 0. `\n`, `\r\n` and a single
//!   `\r` end a line. The character counts UTF-16 code units or bytes (the
//!   encoding is negotiated in `initialize`).
//!
//! [`LineIndex`] converts between them. All conversions go through byte
//! offsets, so the two line rules can be different.

use std::path::{Component, Path, PathBuf};

use lsp_types::{Position, Range, TextDocumentContentChangeEvent, Url};
use o7_front::diag::{Pos, Span};

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// The unit of the LSP `character` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
	Utf8,
	Utf16,
}

/// The line starts of a text, for both line rules.
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
	/// Byte offsets of the LSP line starts.
	lsp: Vec<usize>,
	/// Byte offsets of the line starts of the front end (`\n` only).
	lex: Vec<usize>,
	/// Length of the byte order mark (0 or 3).
	bom: usize,
	len: usize,
}

impl LineIndex {
	pub fn new(text: &[u8]) -> LineIndex {
		let mut lsp = vec![0];
		let mut lex = vec![0];
		let mut i = 0;
		while i < text.len() {
			match text[i] {
				b'\n' => {
					lsp.push(i + 1);
					lex.push(i + 1);
				}
				b'\r' if text.get(i + 1) != Some(&b'\n') => lsp.push(i + 1),
				_ => {}
			}
			i += 1;
		}
		LineIndex {
			lsp,
			lex,
			bom: if text.starts_with(BOM) { 3 } else { 0 },
			len: text.len(),
		}
	}

	/// The byte offset of the front-end position `pos`. A position after
	/// the end of its line gives the end of the line; a position after the
	/// end of the text gives the end of the text.
	pub fn offset(&self, pos: Pos) -> usize {
		if pos.line == 0 {
			return 0;
		}
		let line = pos.line as usize - 1;
		let Some(&start) = self.lex.get(line) else {
			return self.len;
		};
		let start = if line == 0 { start + self.bom } else { start };
		// The end of the line: the position of its `\n` (or the end of the text).
		let end = self.lex.get(line + 1).map_or(self.len, |&n| n - 1);
		(start + pos.col.saturating_sub(1) as usize).min(end.max(start))
	}

	/// The front-end position of the byte offset `off`.
	pub fn pos(&self, off: usize) -> Pos {
		let off = off.min(self.len);
		let line = self.lex.partition_point(|&s| s <= off) - 1;
		let mut start = self.lex[line];
		if line == 0 {
			start = (start + self.bom).min(off.max(start));
		}
		Pos::new(line as u32 + 1, (off - start) as u32 + 1)
	}

	/// The LSP position of the byte offset `off` in `text`.
	pub fn lsp_position(&self, text: &[u8], off: usize, enc: Encoding) -> Position {
		let off = floor_char(text, off.min(self.len));
		let line = self.lsp.partition_point(|&s| s <= off) - 1;
		let mut start = self.lsp[line];
		// The byte order mark is not a character of the document (editors
		// remove it from the text that they show and send).
		if line == 0 && off >= self.bom {
			start += self.bom;
		}
		let chunk = &text[start.min(off)..off];
		let character = match enc {
			Encoding::Utf8 => chunk.len(),
			Encoding::Utf16 => utf16_len(chunk),
		};
		Position::new(line as u32, character as u32)
	}

	/// The byte offset of the LSP position `p` in `text`. A character after
	/// the end of the line gives the end of the line (without the line end).
	pub fn lsp_offset(&self, text: &[u8], p: Position, enc: Encoding) -> usize {
		let line = p.line as usize;
		let Some(&start) = self.lsp.get(line) else {
			return self.len;
		};
		let start = if line == 0 { start + self.bom } else { start };
		let mut end = self.lsp.get(line + 1).copied().unwrap_or(self.len);
		// Without the line end.
		if end > start && text.get(end - 1) == Some(&b'\n') {
			end -= 1;
		}
		if end > start && text.get(end - 1) == Some(&b'\r') {
			end -= 1;
		}
		let want = p.character as usize;
		let mut i = start;
		let mut units = 0;
		while i < end && units < want {
			let (len, u) = char_at(text, i);
			if enc == Encoding::Utf16 && units + u > want {
				// The position is inside a surrogate pair: take the
				// character.
				break;
			}
			units += match enc {
				Encoding::Utf8 => len,
				Encoding::Utf16 => u,
			};
			i += len;
		}
		i.min(end)
	}

	pub fn lsp_range(&self, text: &[u8], span: Span, enc: Encoding) -> Range {
		let start = self.offset(span.start);
		let end = self.offset(span.end).max(start);
		Range::new(
			self.lsp_position(text, start, enc),
			self.lsp_position(text, end, enc),
		)
	}

	/// The front-end position of an LSP position.
	pub fn front_pos(&self, text: &[u8], p: Position, enc: Encoding) -> Pos {
		self.pos(self.lsp_offset(text, p, enc))
	}
}

/// The length of the UTF-8 sequence at `i` and its number of UTF-16 code
/// units. A byte that does not start a valid sequence counts as one
/// character of one unit (the text of a document is valid UTF-8; the text
/// of a file on the disk need not be).
fn char_at(text: &[u8], i: usize) -> (usize, usize) {
	let b = text[i];
	let len = match b {
		0x00..=0x7F => 1,
		0xC0..=0xDF => 2,
		0xE0..=0xEF => 3,
		0xF0..=0xF7 => 4,
		_ => 1,
	};
	if i + len > text.len() || text[i + 1..i + len].iter().any(|&c| c & 0xC0 != 0x80) {
		return (1, 1);
	}
	(len, if len == 4 { 2 } else { 1 })
}

fn utf16_len(chunk: &[u8]) -> usize {
	let mut i = 0;
	let mut n = 0;
	while i < chunk.len() {
		let (len, u) = char_at(chunk, i);
		i += len;
		n += u;
	}
	n
}

/// The largest offset `<= off` that is not inside a UTF-8 sequence.
fn floor_char(text: &[u8], mut off: usize) -> usize {
	while off > 0 && off < text.len() && text[off] & 0xC0 == 0x80 {
		off -= 1;
	}
	off
}

/// An open document.
#[derive(Clone, Debug)]
pub struct Document {
	pub uri: Url,
	pub path: PathBuf,
	pub version: i32,
	pub text: String,
	pub lines: LineIndex,
}

impl Document {
	pub fn new(uri: Url, path: PathBuf, version: i32, text: String) -> Document {
		let lines = LineIndex::new(text.as_bytes());
		Document {
			uri,
			path,
			version,
			text,
			lines,
		}
	}

	/// Applies one change of `textDocument/didChange`.
	pub fn apply(&mut self, change: &TextDocumentContentChangeEvent, enc: Encoding) {
		match change.range {
			None => self.text = change.text.clone(),
			Some(r) => {
				let bytes = self.text.as_bytes();
				let start = self.lines.lsp_offset(bytes, r.start, enc);
				let end = self.lines.lsp_offset(bytes, r.end, enc).max(start);
				// Offsets are on character boundaries (`lsp_offset` walks
				// characters), but a BOM is only skipped on line 0.
				let start = floor_str(&self.text, start);
				let end = floor_str(&self.text, end);
				self.text.replace_range(start..end, &change.text);
			}
		}
		self.lines = LineIndex::new(self.text.as_bytes());
	}
}

fn floor_str(s: &str, mut i: usize) -> usize {
	i = i.min(s.len());
	while !s.is_char_boundary(i) {
		i -= 1;
	}
	i
}

/// The file path of a `file:` URI, without `.` and `..` components.
pub fn uri_to_path(uri: &Url) -> Option<PathBuf> {
	if uri.scheme() != "file" {
		return None;
	}
	uri.to_file_path().ok().map(|p| normalize(&p))
}

pub fn path_to_uri(path: &Path) -> Option<Url> {
	Url::from_file_path(path).ok()
}

/// Removes `.` and `..` components from a path, without access to the file
/// system (symbolic links are kept as the user sees them).
pub fn normalize(path: &Path) -> PathBuf {
	let mut out = PathBuf::new();
	for c in path.components() {
		match c {
			Component::CurDir => {}
			Component::ParentDir => {
				if !out.pop() {
					out.push("..");
				}
			}
			c => out.push(c.as_os_str()),
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn change(sl: u32, sc: u32, el: u32, ec: u32, text: &str) -> TextDocumentContentChangeEvent {
		TextDocumentContentChangeEvent {
			range: Some(Range::new(Position::new(sl, sc), Position::new(el, ec))),
			range_length: None,
			text: text.to_string(),
		}
	}

	#[test]
	fn front_positions() {
		let t = b"ab\ncd\r\nef";
		let li = LineIndex::new(t);
		assert_eq!(li.offset(Pos::new(1, 1)), 0);
		assert_eq!(li.offset(Pos::new(2, 2)), 4);
		// The `\r` is a byte of line 2 for the front end.
		assert_eq!(li.offset(Pos::new(2, 3)), 5);
		assert_eq!(li.offset(Pos::new(3, 1)), 7);
		assert_eq!(li.offset(Pos::new(9, 1)), 9);
		// After the end of the line: the end of the line.
		assert_eq!(li.offset(Pos::new(1, 50)), 2);
		for (off, &c) in t.iter().enumerate() {
			if c != b'\n' {
				assert_eq!(li.offset(li.pos(off)), off, "offset {off}");
			}
		}
	}

	#[test]
	fn lsp_positions_with_crlf_and_single_cr() {
		let t = b"ab\r\ncd\ref\ngh";
		let li = LineIndex::new(t);
		let enc = Encoding::Utf16;
		assert_eq!(li.lsp_position(t, 4, enc), Position::new(1, 0));
		assert_eq!(li.lsp_position(t, 7, enc), Position::new(2, 0));
		assert_eq!(li.lsp_position(t, 11, enc), Position::new(3, 1));
		assert_eq!(li.lsp_offset(t, Position::new(2, 1), enc), 8);
		// After the end of the line: before the line end.
		assert_eq!(li.lsp_offset(t, Position::new(0, 9), enc), 2);
		assert_eq!(li.lsp_offset(t, Position::new(9, 0), enc), t.len());
		// For the front end, `cd\ref` is one line.
		assert_eq!(li.pos(8), Pos::new(2, 5));
	}

	#[test]
	fn utf8_and_utf16() {
		// "(* ä 😀 *) x" : ä has 2 bytes and 1 unit; 😀 has 4 bytes and 2 units.
		let s = "(* \u{e4} \u{1F600} *) x";
		let t = s.as_bytes();
		let li = LineIndex::new(t);
		let x = s.find('x').unwrap();
		assert_eq!(li.pos(x), Pos::new(1, x as u32 + 1));
		assert_eq!(
			li.lsp_position(t, x, Encoding::Utf8),
			Position::new(0, x as u32)
		);
		assert_eq!(li.lsp_position(t, x, Encoding::Utf16), Position::new(0, 11));
		assert_eq!(li.lsp_offset(t, Position::new(0, 11), Encoding::Utf16), x);
		// A position inside the surrogate pair gives the start of the character.
		assert_eq!(li.lsp_offset(t, Position::new(0, 6), Encoding::Utf16), 6);
	}

	#[test]
	fn byte_order_mark() {
		let t = b"\xEF\xBB\xBFMODULE M;\nEND M.";
		let li = LineIndex::new(t);
		// The front end does not count the mark.
		assert_eq!(li.offset(Pos::new(1, 1)), 3);
		assert_eq!(li.pos(3), Pos::new(1, 1));
		assert_eq!(li.lsp_position(t, 3, Encoding::Utf16), Position::new(0, 0));
		assert_eq!(li.lsp_offset(t, Position::new(0, 7), Encoding::Utf16), 10);
		assert_eq!(li.offset(Pos::new(2, 1)), 13);
	}

	#[test]
	fn incremental_edits() {
		let uri = Url::parse("file:///x/M.Mod").unwrap();
		let mut d = Document::new(
			uri,
			PathBuf::from("/x/M.Mod"),
			1,
			"MODULE M;\nEND M.\n".into(),
		);
		d.apply(&change(1, 0, 1, 0, "VAR x: INTEGER;\n"), Encoding::Utf16);
		assert_eq!(d.text, "MODULE M;\nVAR x: INTEGER;\nEND M.\n");
		d.apply(&change(1, 4, 1, 5, "\u{1F600}y"), Encoding::Utf16);
		assert_eq!(d.text, "MODULE M;\nVAR \u{1F600}y: INTEGER;\nEND M.\n");
		// 😀 has two UTF-16 units.
		d.apply(&change(1, 4, 1, 6, ""), Encoding::Utf16);
		assert_eq!(d.text, "MODULE M;\nVAR y: INTEGER;\nEND M.\n");
		d.apply(&change(0, 0, 3, 0, "X"), Encoding::Utf8);
		assert_eq!(d.text, "X");
	}

	#[test]
	fn normalized_paths() {
		assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
		assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
	}
}
