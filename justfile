# Datacube - Data Provider Service
# https://github.com/hypercube/datacube

set shell := ["bash", "-uc"]

# Default recipe - show available commands
default:
    @just --list

# Build in debug mode
build:
    cargo build

# Build in release mode
release:
    cargo build --release

# Run the daemon in debug mode
run *ARGS:
    cargo run --bin datacube -- {{ARGS}}

# Run the CLI client
cli *ARGS:
    cargo run --bin datacube-cli -- {{ARGS}}

# Run with debug logging
debug:
    RUST_LOG=debug cargo run --bin datacube

# Run tests
test:
    cargo test

# Run tests with output
test-verbose:
    cargo test -- --nocapture

# Check code without building
check:
    cargo check

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Run all checks (format, lint, test)
ci: fmt-check lint test

# Clean build artifacts
clean:
    cargo clean

# Generate protobuf code
proto:
    cargo build --build-plan 2>/dev/null || true

# Watch for changes and rebuild
watch:
    cargo watch -x build

# Install locally
install:
    cargo install --path .

# Create systemd user service file
install-service:
    mkdir -p ~/.config/systemd/user
    cp contrib/datacube.service ~/.config/systemd/user/
    systemctl --user daemon-reload
    @echo "Service installed. Enable with: systemctl --user enable --now datacube"

# Show logs
logs:
    journalctl --user -u datacube -f

# Query applications (for testing)
query-apps QUERY:
    @just cli query --provider applications "{{QUERY}}"

# Query calculator (for testing)
calc EXPR:
    @just cli query --provider calculator "={{EXPR}}"
