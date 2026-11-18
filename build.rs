//! Puts the version and the commit of the front end (`o7-front`) from
//! `Cargo.lock` into the environment of the compilation, for `--version`
//! and `serverInfo`.

use std::fs;

fn main() {
	println!("cargo:rerun-if-changed=Cargo.lock");
	let lock = fs::read_to_string("Cargo.lock").unwrap_or_default();
	let mut version = "unknown".to_string();
	let mut commit = "unknown".to_string();
	for block in lock.split("[[package]]") {
		if !block.lines().any(|l| l.trim() == "name = \"o7-front\"") {
			continue;
		}
		for l in block.lines() {
			let l = l.trim();
			if let Some(v) = l.strip_prefix("version = ") {
				version = v.trim_matches('"').to_string();
			}
			if let Some(s) = l.strip_prefix("source = ") {
				// "git+URL?rev=...#<commit>"
				if let Some((_, c)) = s.trim_matches('"').rsplit_once('#') {
					commit = c.to_string();
				}
			}
		}
	}
	println!("cargo:rustc-env=O7_FRONT_VERSION={version}");
	println!("cargo:rustc-env=O7_FRONT_COMMIT={commit}");
}
