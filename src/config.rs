//! The settings of the server.
//!
//! The client gives them in the initialization options and in
//! `workspace/didChangeConfiguration` (or the server asks for them with
//! `workspace/configuration`):
//!
//! ```json
//! { "searchPaths": ["lib/Micro", "lib/Micro/hsebyp"], "target": "thumbv7em-m4f" }
//! ```
//!
//! The object can also be the value of the key `o7` (the id of the Zed
//! extension) or `o7-language-server` (the name of the server).

use std::path::{Path, PathBuf};

use o7_front::target_info::{THUMBV7EM_M4F, TargetProps, target_props};
use serde_json::Value;

use crate::vfs::normalize;

/// The keys under which the settings can be.
pub const SECTIONS: [&str; 2] = ["o7-language-server", "o7"];

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
	/// The directories that are searched for imported modules after the
	/// directory of the root module (the `-I` options of `o7c`). Relative
	/// paths start at the workspace root.
	pub search_paths: Vec<String>,
	pub target: &'static TargetProps,
}

impl Default for Config {
	fn default() -> Config {
		Config {
			search_paths: Vec::new(),
			target: &THUMBV7EM_M4F,
		}
	}
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
					Some(t) => self.target = t,
					None => {
						let known: Vec<&str> = o7_front::target_info::TARGETS
							.iter()
							.map(|t| t.name)
							.collect();
						problems.push(format!(
							"o7-language-server: unknown target {name}; known targets: {}",
							known.join(", ")
						));
					}
				},
				None if v.is_null() => self.target = &THUMBV7EM_M4F,
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
		assert_eq!(c.target.name, "thumbv7em-m4f");
		assert!(c.update(&Value::Null).is_empty());
	}
}
