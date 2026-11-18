#!/usr/bin/env bash
# Smoke test of a server binary: sends initialize, initialized, shutdown and
# exit on the standard input and checks the answer to initialize.
#
#   ci/smoke.sh path/to/o7-language-server
set -euo pipefail
exe="$1"
"$exe" --version
msg() {
	local body="$1"
	printf 'Content-Length: %d\r\n\r\n%s' "${#body}" "$body"
}
out=$({
	msg '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
	msg '{"jsonrpc":"2.0","method":"initialized","params":{}}'
	msg '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
	msg '{"jsonrpc":"2.0","method":"exit"}'
} | "$exe")
if ! grep -q '"name":"o7-language-server"' <<<"$out"; then
	echo "unexpected answer: $out" >&2
	exit 1
fi
echo "smoke test passed"
