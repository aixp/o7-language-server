//! The command `o7-language-server`.

use std::process::ExitCode;

use o7_language_server::server::{self, Options};

const USAGE: &str = "\
Usage: o7-language-server [--stdio]
       o7-language-server --version

Language server for Oberon-07. It reads and writes the Language Server
Protocol on the standard input and output.

A project file o7.toml (the nearest one upward from a module; see the
README of o7c) gives the programs of a project, each with the description of
its MCU (the target, the memory regions) and its search directories. The
server uses it for the target of each module.

Settings for modules without a project file (initialization options or
workspace configuration):
  searchPaths   directories for imported modules (the -I options of o7c);
                relative paths start at the workspace root
  target        the target of the compiler (required, there is no default):
                thumbv7em-m4f, rv32imac or rv32imafc";

fn main() -> ExitCode {
	for arg in std::env::args().skip(1) {
		match arg.as_str() {
			"--version" | "-V" => {
				println!("o7-language-server {}", o7_language_server::version_text());
				return ExitCode::SUCCESS;
			}
			"--help" | "-h" => {
				println!("{USAGE}");
				return ExitCode::SUCCESS;
			}
			"--stdio" => {}
			_ => {
				eprintln!("o7-language-server: unknown argument {arg}\n\n{USAGE}");
				return ExitCode::from(2);
			}
		}
	}
	match server::run_stdio(Options::default()) {
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("o7-language-server: {e}");
			ExitCode::FAILURE
		}
	}
}
