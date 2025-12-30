# datacube

A data provider service for application launchers and desktop utilities in the Hypercube project.

## Overview

Datacube is a background service that indexes and provides data to application launchers via a Unix socket interface. It supports:

- **Application search** - Indexes desktop applications from XDG directories, including flatpak apps (searchable by ID like `org.mozilla.firefox`)
- **Calculator** - Evaluate math expressions with the `=` prefix (e.g., `=2+2`)
- **Command execution** - Run shell commands with the `/` prefix (e.g., `/htop`)

## Installation

### From COPR (Fedora)

```bash
sudo dnf copr enable hypercube/datacube
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
```

### Using the CLI

```bash
# Search for applications
datacube-cli query firefox

# Search by flatpak ID
datacube-cli query org.mozilla

# Calculator
datacube-cli query "=2+2"

# Run a command
datacube-cli query "/htop"
```

## Architecture

Datacube communicates via Protocol Buffers over a Unix socket at `$XDG_RUNTIME_DIR/datacube/datacube.sock`.

### Providers

| Provider | Prefix | Description |
|----------|--------|-------------|
| applications | (none) | Desktop applications from XDG data dirs |
| calculator | `=` | Math expression evaluation |
| command | `/` | Shell command execution |

## License

Apache-2.0
