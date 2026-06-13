#!/usr/bin/env bash
# Build and package all four Orchard 3.0 distribution targets into ./dist.
#
#   1. CLI            — `orch` release binary (also `cargo install`-able)
#   2. C-FFI          — static + dynamic lib + the C header
#   3. Python (PyO3)  — an abi3 wheel via maturin
#   4. WebAssembly    — a wasm-bindgen package via wasm-pack
#
# Targets that need an external toolchain (maturin / wasm-pack) are skipped with
# a notice if the tool is absent, so the core artifacts always build.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
export PATH="$HOME/.cargo/bin:$PATH"

rm -rf "$DIST"
mkdir -p "$DIST/cli" "$DIST/ffi/include" "$DIST/python" "$DIST/wasm"
cd "$ROOT"

echo "==> [1/4] CLI"
cargo build --release -p orch-cli
cp "target/release/orch" "$DIST/cli/"

echo "==> [2/4] C-FFI (static + dynamic + header)"
cargo build --release -p orchard-ffi
for f in liborchard_ffi.a liborchard_ffi.dylib liborchard_ffi.so; do
  [ -f "target/release/$f" ] && cp "target/release/$f" "$DIST/ffi/"
done
cp "crates/orchard-ffi/include/orchard.h" "$DIST/ffi/include/"

echo "==> [3/4] Python wheel (maturin)"
if command -v maturin >/dev/null 2>&1; then
  (cd crates/orchard-py && maturin build --release --out "$DIST/python")
else
  echo "    maturin not found — skipping (install: pipx install maturin)"
fi

echo "==> [4/4] WASM package (wasm-pack)"
if command -v wasm-pack >/dev/null 2>&1; then
  (cd crates/orchard-wasm && wasm-pack build --release --target web --out-dir "$DIST/wasm")
else
  echo "    wasm-pack not found — building the raw wasm artifact instead"
  if rustup target list --installed | grep -q wasm32-unknown-unknown; then
    # orchard-wasm is excluded from the workspace, so it has a crate-local target dir.
    (cd crates/orchard-wasm && cargo build --release --target wasm32-unknown-unknown)
    cp crates/orchard-wasm/target/wasm32-unknown-unknown/release/orchard_wasm.wasm \
      "$DIST/wasm/" 2>/dev/null || true
  else
    echo "    wasm32 target not installed — skipping"
  fi
fi

echo
echo "==> Done. Artifacts in $DIST:"
find "$DIST" -type f | sed "s#$DIST/#  #"
