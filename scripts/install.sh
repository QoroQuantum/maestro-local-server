#!/usr/bin/env bash
# Install the prebuilt Maestro library and build maestro-local-server against it.
# This does what sections 2 to 4 of docs/installation.md describe; read that
# guide for the reasoning and for troubleshooting.
#
# Usage (from a clone of this repository, as a normal user with sudo):
#   scripts/install.sh          install Maestro to /opt/maestro, build the server
#   scripts/install.sh --deb    also build the .deb, install it, and start the service
#
# Environment:
#   MAESTRO_VERSION   Maestro release to install (default: the version this
#                     server is pinned to)
#   MAESTRO_PREFIX    where to extract the archive (default: /opt/maestro). The
#                     server finds Maestro through this path at run time, so it
#                     must also exist on every host that runs the service.
#
# Prerequisites (guide section 1): Rust 1.85+, libclang, a C compiler, curl.
set -euo pipefail

MAESTRO_VERSION="${MAESTRO_VERSION:-0.3.12}"
MAESTRO_PREFIX="${MAESTRO_PREFIX:-/opt/maestro}"
BASE_URL="https://github.com/QoroQuantum/maestro/releases/download/v${MAESTRO_VERSION}"

build_deb=false
case "${1:-}" in
    "") ;;
    --deb) build_deb=true ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
esac

die() { echo "error: $*" >&2; exit 1; }
step() { echo; echo "==> $*"; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
grep -q '^name = "maestro-local-server"' "$repo/Cargo.toml" 2>/dev/null ||
    die "run this from a clone of maestro-local-server ($repo is not one)"

[ "$(uname -s)" = Linux ] || die "this script supports Linux only; see Appendix A of docs/installation.md"
arch="$(uname -m)"
case "$arch" in
    x86_64|aarch64) ;;
    *) die "no prebuilt Maestro archive for $arch (x86_64 and aarch64 only); see Appendix A of docs/installation.md" ;;
esac

# Run privileged commands through sudo unless already root.
sudo_cmd=""
if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null || die "sudo is required (or run as root)"
    sudo_cmd="sudo"
fi

step "Checking prerequisites"
for tool in curl tar sha256sum cc; do
    command -v "$tool" >/dev/null || die "$tool not found; install the packages in section 1 of docs/installation.md"
done
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null || die "cargo not found; install Rust from https://rustup.rs"
rust_version="$(rustc --version | awk '{print $2}')"
[ "$(printf '%s\n1.85.0\n' "$rust_version" | sort -V | head -n1)" = "1.85.0" ] ||
    die "Rust $rust_version is too old; the server needs 1.85 or newer (rustup update)"
echo "Rust $rust_version, $arch"

archive="maestro-c-${MAESTRO_VERSION}-linux-${arch}.tar.gz"
sums="maestro-c-${MAESTRO_VERSION}-SHA256SUMS"

installed_info="$MAESTRO_PREFIX/BUILD_INFO"
if [ -f "$installed_info" ] &&
    grep -qx "version=${MAESTRO_VERSION}" "$installed_info" &&
    grep -qx "arch=${arch}" "$installed_info"; then
    step "Maestro ${MAESTRO_VERSION} already installed in $MAESTRO_PREFIX; skipping download"
else
    step "Downloading Maestro ${MAESTRO_VERSION} (${arch})"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    (
        cd "$work"
        curl -fsSLO "${BASE_URL}/${archive}" ||
            die "could not download ${BASE_URL}/${archive} (no such release or architecture?)"
        curl -fsSLO "${BASE_URL}/${sums}" || die "could not download ${BASE_URL}/${sums}"
        sha256sum --ignore-missing -c "$sums" || die "checksum mismatch; not extracting"
    )
    step "Extracting to $MAESTRO_PREFIX"
    $sudo_cmd mkdir -p "$MAESTRO_PREFIX"
    $sudo_cmd tar -xzf "$work/$archive" -C "$MAESTRO_PREFIX" --strip-components=1
fi
cat "$installed_info"

step "Building maestro-local-server"
export MAESTRO_HEADER="$MAESTRO_PREFIX/include/maestrolib/Interface.h"
export MAESTRO_LIBRARY_DIR="$MAESTRO_PREFIX/lib"
cd "$repo"
cargo build --release
echo "Built $repo/target/release/maestro-local-server"

if $build_deb; then
    step "Building and installing the .deb package"
    command -v dpkg >/dev/null || die "--deb needs a Debian-based system (dpkg not found)"
    command -v cargo-deb >/dev/null || cargo install cargo-deb
    cargo deb
    $sudo_cmd dpkg -i "$repo"/target/debian/maestro-local-server_*.deb
    if [ -d /run/systemd/system ]; then
        $sudo_cmd systemctl enable --now maestro.service
        $sudo_cmd systemctl --no-pager status maestro.service | head -n 5
    else
        echo "systemd is not running here; start the daemon manually (guide section 6)."
    fi
fi

step "Done"
echo "Run it by hand:   MAESTRO_SOCKET=/tmp/maestro-dev.sock $repo/target/release/maestro-local-server"
echo "Then verify:      printf 'PING\\n' | nc -U -N /tmp/maestro-dev.sock"
