//! The settings of the server.
//!
//! The client gives them in the initialization options and in
//! `workspace/didChangeConfiguration` (or the server asks for them with
//! `workspace/configuration`):
//!
//! ```json
//! { "searchPaths": ["lib", "lib/Micro", "lib/Micro/hsebyp"], "target": "thumbv7em-m4f" }
//! ```
//!
//! The object can also be the value of the key `o7` (the id of the Zed
//! extension) or `o7-language-server` (the name of the server).
//!
//! The target has no default value (as the target of `o7c build`, which has
//! no default). Without a target the server does not analyse the modules;
//! each open document has the error [`NO_TARGET`].
//!
//! The settings are for a module without a project file. A project file
//! `o7.toml` (the nearest one upward from the module) gives the description
//! of the MCU, and thus the target, and the search directories of each of
//! its programs; the settings then do not apply to the module
//! (`crate::workspace`).

use std::path::{Path, PathBuf};

use o7_front::project_file::PROJECT_FILE;
use o7_front::target_info::{TARGETS, TargetProps, target_props};
use serde_json::Value;

use crate::vfs::normalize;

/// The keys under which the settings can be.
pub const SECTIONS: [&str; 2] = ["o7-language-server", "o7"];

/// The start of the message for a missing target.
pub const NO_TARGET: &str = "no target: set the setting `target`";

/// The message for a missing target, with the names of the known targets.
pub fn no_target_message() -> String {
	format!(
		"{NO_TARGET} to one of: {}; or make a project file {PROJECT_FILE} with the programs and their MCUs",
		target_names().join(", ")
	)
}

/// The message for a module that is in no program of the project file
/// `file`, when that file has no default MCU.
pub fn no_program_message(file: &Path) -> String {
	format!(
		"no target: the module is in no program of {}, and that file has no default MCU (the keys `target`, `flash` and `ram` at its top)",
		file.display()
	)
}

/// The message for a module whose project file `file` has errors.
pub fn project_errors_message(file: &Path) -> String {
	format!("no target: the project file {} has errors", file.display())
}

/// The names of the targets of the front end.
pub fn target_names() -> Vec<&'static str> {
	TARGETS.iter().map(|t| t.name).collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
	/// The directories that are searched for imported modules after the
	/// directory of the root module (the `-I` options of `o7c`). Relative
	/// paths start at the workspace root.
	pub search_paths: Vec<String>,
	/// The target of the compiler. `None` until the client gives it: there
	/// is no default.
	pub target: Option<&'static TargetProps>,
}

impl Config {
	/// Reads the settings from `value`. Keys that are absent keep their old
	/// value. Returns messages about wrong values (the server shows them).
	pub fn update(&mut self, value: &Value) -> Vec<String> {
		let mut problems = Vec::new();
		let Some(obj) = settings_object(value) else {
			return problems;
		};
		if let Some(v) = obj.get("searchPaths") {
			match v {
				Value::Array(items) => {
					let mut list = Vec::new();
					for item in items {
						match item.as_str() {
							Some(s) => list.push(s.to_string()),
							None => problems.push(format!(
								"o7-language-server: searchPaths: {item} is not a string"
							)),
						}
					}
					self.search_paths = list;
				}
				Value::Null => self.search_paths.clear(),
				_ => problems.push(
					"o7-language-server: searchPaths must be a list of directories".to_string(),
				),
			}
		}
		if let Some(v) = obj.get("target") {
			match v.as_str() {
				Some(name) => match target_props(name) {
					Some(t) => self.target = Some(t),
					None => problems.push(format!(
						"o7-language-server: unknown target {name}; known targets: {}",
						target_names().join(", ")
					)),
				},
				None if v.is_null() => self.target = None,
				None => problems.push("o7-language-server: target must be a string".to_string()),
			}
		}
		problems
	}

	/// The search directories as absolute paths. A relative path starts at
	/// `root` (the first workspace folder).
	pub fn search_dirs(&self, root: Option<&Path>) -> Vec<PathBuf> {
		self.search_paths
			.iter()
			.filter(|s| !s.is_empty())
			.filter_map(|s| {
				let p = Path::new(s);
				if p.is_absolute() {
					Some(normalize(p))
				} else {
					root.map(|r| normalize(&r.join(p)))
				}
			})
			.collect()
	}
}

/// The object with the settings: `value` itself if it has one of the keys,
/// else the value of one of the keys of [`SECTIONS`].
fn settings_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
	let obj = value.as_object()?;
	if obj.contains_key("searchPaths") || obj.contains_key("target") {
		return Some(obj);
	}
	SECTIONS
		.iter()
		.find_map(|k| obj.get(*k).and_then(Value::as_object))
		.or(Some(obj))
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	#[test]
	fn read_settings() {
		let mut c = Config::default();
		assert_eq!(c.target, None, "no default target");
		assert!(
			c.update(&json!({"searchPaths": ["lib", "/abs/x/../y"]}))
				.is_empty()
		);
		assert_eq!(c.search_paths, ["lib", "/abs/x/../y"]);
		assert_eq!(
			c.search_dirs(Some(Path::new("/w"))),
			[PathBuf::from("/w/lib"), PathBuf::from("/abs/y")]
		);
		// Nested under the name of the server; absent keys do not change.
		assert!(
			c.update(&json!({"o7-language-server": {"target": "thumbv7em-m4f"}}))
				.is_empty()
		);
		assert_eq!(c.search_paths, ["lib", "/abs/x/../y"]);
		let p = c.update(&json!({"target": "z80", "searchPaths": 3}));
		assert_eq!(p.len(), 2, "{p:?}");
		assert_eq!(c.target.map(|t| t.name), Some("thumbv7em-m4f"));
		assert!(c.update(&Value::Null).is_empty());
		// null removes the target.
		assert!(c.update(&json!({"target": null})).is_empty());
		assert_eq!(c.target, None);
	}
}
