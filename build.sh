#!/bin/bash

# Build script for Binance Futures Collector (Rust)

set -e

echo "Building Binance Futures Collector..."

# Ensure we're in the right directory
cd "$(dirname "$0")"

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "ERROR: Rust is not installed. Please install Rust from https://rustup.rs/"
    exit 1
fi

# Note: cargo clean removed - incremental builds are faster and reliable
# Use 'cargo clean' manually only if you need to debug build issues

# Run tests
echo "Running tests..."
cargo test

# Build release version
echo "Building release version..."
cargo build --release

# Create logs directory if it doesn't exist
mkdir -p logs

# Copy binary to a more accessible location
cp target/release/binance-futures-collector ./binance-futures-collector

echo "Build complete! Binary available at: ./binance-futures-collector"
echo ""
echo "To run the collector:"
echo "  ./binance-futures-collector"
echo ""
echo "To run in test mode (10 symbols only):"
echo "  TEST_MODE=1 ./binance-futures-collector"
echo ""
echo "To use custom config:"
echo "  CONFIG_PATH=/path/to/config.toml ./binance-futures-collector"