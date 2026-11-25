# o7-language-server

A language server for Oberon-07. It uses the front end of the compiler
[`o7c`](https://github.com/aixp/o7c) (crate `o7-front`). The diagnostics are
the messages of the compiler. All information about names comes from the
semantic index of the front end. The server does not need LLVM.

## Features

| Feature | Notes |
| ------- | ----- |
| Diagnostics | For the open documents and the files that they import. The range is the symbol at the position of the message. All messages are errors (the compiler has no warnings). |
| Go to Definition | Also for imported objects, fields of imported records and import names (they go to the module). |
| Hover | The declaration (`PROCEDURE Add*(VAR list: Node; value: INTEGER): INTEGER`, `CONST Max* = 10`). For an imported procedure the parameter names come from the exporting module. On an expression: its type. |
| Document Symbols | Module, constants, types with their fields, variables, procedures (nested). |
| Find References | In the file and, for an exported object or field, in all files of the workspace that import the module (also files that are not open). Includes the name after `END`. |
| Workspace Symbols | The module-level symbols of all files of the workspace. The query is a sub-sequence of the name. |
| Completion | After `M.`: the exported objects of `M` (the procedures of `SYSTEM` for `SYSTEM`). After `r.`: the fields, also the inherited fields; a pointer gives the fields of its record. Else: the visible names, the predeclared names, the reserved words. Variables of an enclosing procedure are not offered (Oberon-07 does not permit their use). |
| Rename | All references, also in other modules. The server analyses the changed files before it returns the edit. It refuses a rename that causes an error or that changes what a name denotes (a new name that hides another name). It refuses predeclared names, module names and an import without an alias. |
| Semantic Tokens | One token for each identifier. Types: `namespace`, `type`, `variable`, `parameter`, `property`, `function`. Modifiers: `declaration`, `readonly` (constants, imported variables, structured value parameters), `defaultLibrary` (predeclared names). |

## Installation

Build from the source text (Rust 1.88 or later):

```sh
cargo install --locked --git https://github.com/aixp/o7-language-server
```

The build needs read access to the compiler repository (see "Front-end
version").

Or download a release archive for your system from the
[releases](https://github.com/aixp/o7-language-server/releases). Each archive
contains one file: `o7-language-server` (`o7-language-server.exe` on Windows).

The Zed extension (`zed-o7`) finds the server in the settings, then on
`PATH`, then downloads the latest release.

## Use

```sh
o7-language-server            # the protocol on the standard input and output
o7-language-server --stdio    # the same
o7-language-server --version  # server version, o7-front version, compiler commit
```

The answer to `initialize` has the same version text in `serverInfo`.

## Settings

The client gives the settings in the initialization options, in
`workspace/didChangeConfiguration`, or as the answer to
`workspace/configuration` (section `o7-language-server` or `o7`):

```json
{ "searchPaths": ["lib/Micro", "lib/Micro/hsebyp", "tests/lib"], "target": "thumbv7em-m4f" }
```

| Key | Meaning |
| --- | ------- |
| `searchPaths` | The directories for imported modules, in the order of the search. They have the meaning of the `-I` options of `o7c`. A relative path starts at the first workspace folder. Default: none. |
| `target` | The target of the compiler. The front end needs some of its properties (pointer size, `SYSTEM.REG` for the stack pointer, `SYSTEM.EXCFRAME`, the procedures of `SYSTEM` that the target has and the special registers of `SYSTEM.SREG` and `SYSTEM.LDSREG`, the predeclared procedure `CAS`, the type `REAL`). Values: `thumbv7em-m4f` (default), `rv32imac` (no `REAL`, `FLT`, `FLOOR`, `SYSTEM.SQRT`), `rv32imafc`. |

The settings object can also be the value of the key `o7-language-server` or
`o7`. Example for Zed:

```json
{ "lsp": { "o7-language-server": {
    "initialization_options": { "searchPaths": ["lib/Micro", "lib/Micro/hsebyp"] } } } }
```

## How the server finds and analyses modules

- Each open document is a root module. Its search list is its own
  directory, then `searchPaths`. This is the rule of `o7c` for the root
  module: the source text of module `M` is the file `M.Mod` in the first
  directory of the list that has it.
- The server uses the functions of the compiler (`project::collect`) for the
  search. Thus the messages "cannot find the source file", "circular import"
  and "the file contains module X" are the messages of the compiler.
- Results are stored for each file path, not for each module name. If two
  roots in different directories see different files for one module name
  (for example board variants), each root uses its own file.
- The text of an open document has priority over the file on the disk. A
  change in an open document has an effect on its importers before it is
  saved.
- After each change the server analyses the complete module again (about
  0.1 ms to 0.6 ms for each module). The analysis of a module stays valid
  while its text and the interfaces of its imports do not change. The
  interface is the symbol file of the compiler, made in memory; its key
  changes when the interface changes. Thus a change in the body of a
  procedure causes no new analysis of the importers.
- A module with errors: its importers use the last version of its interface
  that had no errors. If there is no such version, the import gives the
  message "cannot import module A: module A has errors". The command-line
  compiler does not compile the importers of a module with errors; the
  server is softer so that the information in the importers stays available
  while you type.
- The server publishes the diagnostics after a pause of 100 ms without
  changes. A request (hover, completion) does not wait: it analyses the
  current text.
- Workspace index: the server analyses all `*.Mod` files of the workspace
  folders (without directories whose name starts with `.`, `target` and
  `node_modules`) and of `searchPaths` in the background, one file at a time
  when no message waits. Find References, Rename and Workspace Symbols bring
  the index up to date before they answer.
- The server registers a file watcher for `**/*.Mod` if the client permits
  dynamic registration. Also without it, the server reads a file again when
  its modification time or its size changed. But without the watcher, a file
  that is not imported by an open document is not analysed again before the
  next change in an open document.

## Limits

- Near a syntax error the index is not complete: the parser skips text to
  the next statement or declaration. Hover and completion are weaker there.
  Some errors (an `END` too many, a procedure heading without a body) make
  the procedures after the error local procedures of the wrong procedure
  until the text is correct again.
- A diagnostic range is one symbol wide.
- The parameter names of a procedure type and a type that `POINTER TO T`
  names before its declaration are not in the completion list.
- A module cannot be renamed (the file name would have to change too).
- Find References and Rename of an exported field need the declaring module
  without errors.
- Semantic tokens: only the request for the full document.
- The server and the `o7c` that you use can contain different versions of the
  front end. Then the messages can be different. `--version` shows the
  compiler commit of the server.

## Integration contract with the editor extension

The Zed extension depends on these items only:

1. The release repository: `aixp/o7-language-server`.
2. The asset names (one archive for each target):

   | Zed `current_platform()` | Asset |
   | ------------------------ | ----- |
   | `Linux`, `X8664` | `o7-language-server-x86_64-unknown-linux-musl.tar.gz` |
   | `Linux`, `Aarch64` | `o7-language-server-aarch64-unknown-linux-musl.tar.gz` |
   | `Mac`, `X8664` | `o7-language-server-x86_64-apple-darwin.tar.gz` |
   | `Mac`, `Aarch64` | `o7-language-server-aarch64-apple-darwin.tar.gz` |
   | `Windows`, `X8664` | `o7-language-server-x86_64-pc-windows-msvc.zip` |
   | `Windows`, `Aarch64` | `o7-language-server-aarch64-pc-windows-msvc.zip` |

   Each release also has `SHA256SUMS`.
3. Each archive contains one executable, `o7-language-server` (or
   `o7-language-server.exe`), at its root.
4. The server starts without arguments and uses the standard input and
   output.
5. The setting names `searchPaths` and `target`.

A change of one of these items is a new major version of the server.

## Front-end version

The server depends on a fixed commit of the compiler repository:

```toml
o7-front = { git = "ssh://git@github.com/aixp/o7c.git", rev = "30cfb8d3b8b55ed1de930d2f01bcd558783559c2" }
```

`Cargo.lock` is committed, so the build is reproducible. Cargo builds only
the crate `o7-front` from the compiler workspace; LLVM is not necessary
(`cargo tree` shows no `o7-llvm` and no `cc`). The first commit is the tag
`o7-front-v0.2.1`.

The compiler repository is private. Thus the URL is SSH, and
`.cargo/config.toml` sets `net.git-fetch-with-cli = true`: Cargo uses the
`git` command of the system with your SSH configuration and agent. To build
the server, you need read access to `aixp/o7c` with an SSH key. (Cargo does
not accept the form `git@github.com:aixp/o7c.git`; `ssh://git@github.com/...`
is the same address.)

The CI and release workflows need the secret `O7C_SSH_KEY`: the private part
of a deploy key of `aixp/o7c` (read only). Add the public part to `aixp/o7c`
(Settings, Deploy keys) and the private part to this repository (Settings,
Secrets and variables, Actions).

To use a new front end, change `rev` and run `cargo update -p o7-front`.
For daily work on the compiler and the server at the same time, a `[patch]`
section in a local `.cargo/config.toml` (not committed) can point to the
working copy of the compiler. If the version of the working copy is not the
locked version, Cargo ignores the patch until `cargo update -p o7-front`
runs with the patch; that changes `Cargo.lock`, which must then not be
committed.

## Development

```sh
cargo test
```

The tests run the server in the same process on a connection in memory:

| File | Tests |
| ---- | ----- |
| `tests/diagnostics.rs` | Initialization, diagnostics of one module, small edits against the final text, CRLF and UTF-8 positions, byte order mark, speed (MicroSTM32F4.Mod: less than 10 ms after the pause). |
| `tests/multi_module.rs` | Interface change in an open buffer, no analysis of importers after a body change (counter), missing import, circular import, wrong module name, module with errors, stale interface, changed search paths. |
| `tests/differential.rs` | For each module of the compiler repository: the diagnostics of the server are equal to those of `front::parse_module`. |
| `tests/navigation.rs` | Definition, hover, document symbols, references (also of `tests/lib/Out.Mod` in files that are not open), workspace symbols. |
| `tests/editing.rs` | Completion, rename (with the refusals), semantic tokens (one token for each identifier of each correct file of the compiler repository). |
| `tests/robust.rs` | All requests on cut texts; the binary on the standard input and output. |

Some tests need the compiler repository. They look for it in the sibling
directory `../oberon-compiler`, or in the directory that `O7C_REPO` gives.
Without it they print "skipped" and pass.

The request `o7/status` returns counters of the analysis (`analyses`,
`indexFiles`, `indexPending`). The tests use it.

Source files:

| File | Contents |
| ---- | -------- |
| `src/main.rs` | Arguments, start. |
| `src/server.rs` | Capabilities, message loop, dispatch. One thread owns all analysis data. |
| `src/config.rs` | `searchPaths`, `target`. |
| `src/vfs.rs` | Documents, line tables, conversion of positions (front end: line and byte column from 1, `\n` only; LSP: UTF-16 or UTF-8, `\n`, `\r\n`, `\r`). |
| `src/workspace.rs` | Module graph, memo table, interface snapshots, workspace index; implements `SourceHost` and `ImportLoader`. |
| `src/features/` | One module for each kind of request. |

## Release

A tag `vX.Y.Z` starts `.github/workflows/release.yml`. It builds the six
targets of the table above (Linux with musl and static linking), runs a smoke
test (`ci/smoke.sh`: `initialize`, `shutdown`, `exit`) on the targets that the
runners can execute, and makes a GitHub release with the archives and
`SHA256SUMS`. The release notes name the compiler commit.

## License

MIT. See `LICENSE`. The server contains the front end of `o7c` (MIT).
