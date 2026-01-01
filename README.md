# datacube

A data provider service for application launchers and desktop utilities in the Hypercube project.

## Overview

Datacube is a background service that provides data to application launchers via a Unix socket interface. It is a **data broker** - it provides information but does not execute commands or launch applications. That responsibility belongs to the client application.

Supported providers:

- **Applications** - Indexes desktop applications from XDG directories, including flatpak apps (searchable by ID like `org.mozilla.firefox`)
- **Calculator** - Evaluate math expressions with the `=` prefix (e.g., `=2+2`)

## Installation

### From COPR (Fedora)

```bash
sudo dnf copr enable binarypie/hypercube
sudo dnf install datacube
```

### From source

```bash
cargo build --release
sudo install -Dm755 target/release/datacube /usr/bin/datacube
sudo install -Dm755 target/release/datacube-cli /usr/bin/datacube-cli
install -Dm644 datacube.service ~/.config/systemd/user/datacube.service
```

## Usage

### Starting the service

```bash
# Enable and start for current user
systemctl --user enable --now datacube.service

# Check status
systemctl --user status datacube.service

# View logs
journalctl --user -u datacube.service -f
```

### Using the CLI

```bash
# Search for applications
datacube-cli query firefox

# Search by flatpak ID
datacube-cli query org.mozilla

# Calculator
datacube-cli query "=2+2"

# JSON output (for scripting)
datacube-cli query firefox --json

# List providers
datacube-cli providers
```

## Architecture

Datacube communicates via Protocol Buffers over a Unix socket at `$XDG_RUNTIME_DIR/datacube.sock`.

### Providers

| Provider | Prefix | Description |
|----------|--------|-------------|
| applications | (none) | Desktop applications from XDG data dirs |
| calculator | `=` | Math expression evaluation |

### Protocol

The protocol uses a simple framing format:
- 1 byte message type
- 4 bytes big-endian length
- N bytes protobuf-encoded body

Message types:
- `1` Query request
- `2` Query response
- `5` List providers request
- `6` List providers response

## Configuration

Configuration file: `~/.config/datacube/config.toml`

```toml
# Socket path (default: $XDG_RUNTIME_DIR/datacube.sock)
socket_path = "/run/user/1000/datacube.sock"

# Maximum results per query
max_results = 50

[providers.applications]
enabled = true

[providers.calculator]
enabled = true
```

## License

Apache-2.0
