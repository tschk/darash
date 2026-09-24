#!/bin/bash
set -e
cargo build --release

# Clear cache
export XDG_CACHE_HOME=/tmp/darash_cache
rm -rf $XDG_CACHE_HOME/darash/fetch

# Seed cache
./target/release/darash fetch https://example.com > /dev/null 2>&1

echo "Warming up..."
for i in {1..5}; do
  ./target/release/darash fetch https://example.com > /dev/null 2>&1
done

echo "Benchmarking load..."
time for i in {1..100}; do
  ./target/release/darash fetch https://example.com > /dev/null 2>&1
done
