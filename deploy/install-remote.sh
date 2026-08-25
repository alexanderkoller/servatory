#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: $0 [user@]proxmox-host" >&2
    echo "Example: $0 root@pve.local" >&2
}

if [[ $# -ne 1 || "$1" == -* ]]; then
    usage
    exit 2
fi

remote_host=$1
if [[ ! "$remote_host" =~ ^[A-Za-z0-9_.:@%+\[\]-]+$ ]]; then
    echo "Invalid SSH destination: $remote_host" >&2
    exit 2
fi

for command_name in ssh scp tar; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required command not found: $command_name" >&2
        exit 1
    fi
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$script_dir/.." && pwd)
local_stage=$(mktemp -d "${TMPDIR:-/tmp}/s3-display-deploy.XXXXXX")

cleanup_local() {
    rm -rf -- "$local_stage"
}
trap cleanup_local EXIT

echo "Packaging host daemon..."
tar -C "$repo_dir" -czf "$local_stage/source.tar.gz" \
    Cargo.toml Cargo.lock host protocol deploy/99-m5stick-s3.rules deploy/s3-display.service

echo "Connecting to $remote_host..."
remote_stage=$(ssh -- "$remote_host" 'mktemp -d /tmp/s3-display-install.XXXXXX')
if [[ ! "$remote_stage" =~ ^/tmp/s3-display-install\.[A-Za-z0-9]+$ ]]; then
    echo "The server returned an unexpected temporary path: $remote_stage" >&2
    exit 1
fi

scp -- "$local_stage/source.tar.gz" "$script_dir/install-on-host.sh" \
    "$remote_host:$remote_stage/"

# A TTY lets sudo ask for the remote user's password when root SSH is disabled.
ssh -t -- "$remote_host" "bash '$remote_stage/install-on-host.sh'"

