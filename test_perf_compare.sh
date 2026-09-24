#!/bin/bash
set -e
cargo build --release

# Clear cache
export XDG_CACHE_HOME=/tmp/darash_cache
rm -rf $XDG_CACHE_HOME/darash/fetch

# Seed cache
./target/release/darash fetch https://example.com > /dev/null 2>&1

echo "Benchmarking load (post-optimization)..."
time for i in {1..200}; do
  ./target/release/darash fetch https://example.com > /dev/null 2>&1
done
