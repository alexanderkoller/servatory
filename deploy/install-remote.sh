#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: $0 [user@]proxmox-host" >&2
    echo "Example: $0 alex@pve.local" >&2
    echo "The remote account must be a non-root user with sudo access." >&2
}

if [[ $# -ne 1 || "$1" == -* ]]; then
    usage
    exit 2
fi

remote_host=$1
if [[ ! "$remote_host" =~ ^([A-Za-z0-9_.%+-]+@)?[A-Za-z0-9.-]+$ ]] && \
    [[ ! "$remote_host" =~ ^([A-Za-z0-9_.%+-]+@)?\[[0-9A-Fa-f:]+\]$ ]]; then
    echo "Invalid SSH destination: $remote_host" >&2
    exit 2
fi
if [[ "$remote_host" == root@* ]]; then
    echo "Refusing root SSH. Use a regular account with sudo access." >&2
    exit 2
fi

for command_name in ssh scp rustup cargo-zigbuild zig file grep; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required command not found: $command_name" >&2
        exit 1
    fi
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$script_dir/.." && pwd)

echo "Inspecting $remote_host..."
remote_uid=$(ssh -- "$remote_host" 'id -u')
if [[ "$remote_uid" == 0 ]]; then
    echo "Refusing to deploy through a UID 0 SSH session." >&2
    echo "Connect as a regular account with sudo access instead." >&2
    exit 1
elif [[ ! "$remote_uid" =~ ^[0-9]+$ ]]; then
    echo "The server returned an unexpected user ID: $remote_uid" >&2
    exit 1
fi

remote_arch=$(ssh -- "$remote_host" 'uname -m')
case "$remote_arch" in
    x86_64 | amd64)
        rust_target=x86_64-unknown-linux-musl
        ;;
    aarch64 | arm64)
        rust_target=aarch64-unknown-linux-musl
        ;;
    *)
        echo "Unsupported server architecture: $remote_arch" >&2
        echo "Supported architectures are x86_64 and aarch64." >&2
        exit 1
        ;;
esac

if ! rustup target list --installed | grep -Fxq "$rust_target"; then
    echo "Installing local Rust standard library for $rust_target..."
    rustup target add "$rust_target"
fi

echo "Building a static $rust_target daemon on this computer..."
(cd "$repo_dir" && cargo zigbuild --locked --release -p health-stick-host --target "$rust_target")
binary_path=$repo_dir/target/$rust_target/release/health-stick-host
if ! file "$binary_path" | grep -Fq 'statically linked'; then
    echo "Cross-built daemon is not statically linked: $binary_path" >&2
    exit 1
fi

remote_stage=$(ssh -- "$remote_host" 'mktemp -d /tmp/health-stick-install.XXXXXX')
if [[ ! "$remote_stage" =~ ^/tmp/health-stick-install\.[A-Za-z0-9]+$ ]]; then
    echo "The server returned an unexpected temporary path: $remote_stage" >&2
    exit 1
fi

echo "Uploading the static daemon and service files..."
scp -- "$binary_path" "$script_dir/install-on-host.sh" \
    "$script_dir/99-health-stick.rules" "$script_dir/health-stick.service" "$script_dir/health-stick.yaml" \
    "$remote_host:$remote_stage/"

# A TTY lets sudo ask for the remote user's password when root SSH is disabled.
ssh -t -- "$remote_host" "bash '$remote_stage/install-on-host.sh'"
