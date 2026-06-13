#!/bin/sh
# Build + run the C embedding example against the Orchard C ABI.
set -e
cd "$(dirname "$0")/../.."
cargo build -p orchard-ffi
cc examples/embed-c/main.c -I examples/embed-c -L target/debug -lorchard_ffi -o target/debug/embed_c_demo
# macOS: DYLD_LIBRARY_PATH; Linux: LD_LIBRARY_PATH
DYLD_LIBRARY_PATH=target/debug LD_LIBRARY_PATH=target/debug ./target/debug/embed_c_demo
