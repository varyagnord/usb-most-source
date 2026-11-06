#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p build/tests-native
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cc -std=c11 -Wall -Wextra -Werror -O2 -pthread tests/native/trace_engine.c -o build/tests-native/trace_engine
build/tests-native/trace_engine > build/tests-native/c-reference.trace
cargo test --locked reference_trace -- --nocapture > build/tests-native/rust-trace.log
python3 - <<'PY'
import pathlib,re
p=pathlib.Path('build/tests-native')
c=(p/'c-reference.trace').read_text().splitlines()
r=re.findall(r'TRACE -?\d+ -?\d+ -?\d+ \d+', (p/'rust-trace.log').read_text())
assert len(c)==96 and c==r, 'Rust differs from the retained C reference'
print('PASS: 96 C/Rust differential traces match exactly')
PY
