#!/bin/sh
# Build the native extension and place it inside the arclt package, so
# `PYTHONPATH=python python3 -c "import arclt"` works without maturin.
# (`maturin develop` / `pip install ./python` work too.)
set -e
cd "$(dirname "$0")"
PYO3_BUILD_EXTENSION_MODULE=1 cargo build --release -p arclt
cp ../target/release/lib_arclt.so arclt/_arclt.abi3.so
echo "built $(pwd)/arclt/_arclt.abi3.so"
