#!/bin/bash
# Build the release .deb for this machine's architecture.
#
# Runs inside an ubuntu:22.04 container with the repository mounted at the
# current directory. Building on 22.04 keeps the package's glibc requirement low
# enough (2.34) to install on Ubuntu 22.04+ and Debian 12+; building on the
# runner's own Ubuntu 24.04 would require glibc 2.39.
#
# Uses scripts/install.sh, so the package is built exactly the way the
# installation guide describes: against the Maestro release that script pins,
# extracted to /opt/maestro.
#
# Writes dist/maestro-local-server_<version>-1_<arch>.deb and dist/MAESTRO_VERSION.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
echo "::group::Install build prerequisites (guide section 1)"
apt-get update -qq
apt-get install -y -qq build-essential ca-certificates curl git libclang-dev > /dev/null
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
# shellcheck source=/dev/null
. "$HOME/.cargo/env"
# The checkout belongs to the runner's user, not root; without this, build.rs
# cannot read the git revision and the server reports it as unknown.
git config --global --add safe.directory "$PWD"
echo "::endgroup::"

echo "::group::Install Maestro and build the server (scripts/install.sh)"
scripts/install.sh
echo "::endgroup::"

echo "::group::Build the .deb"
export MAESTRO_HEADER=/opt/maestro/include/maestrolib/Interface.h
export MAESTRO_LIBRARY_DIR=/opt/maestro/lib
cargo install cargo-deb --locked
cargo deb
mkdir -p dist
cp target/debian/maestro-local-server_*.deb dist/
sed -n 's/^version=//p' /opt/maestro/BUILD_INFO > dist/MAESTRO_VERSION
dpkg-deb --field dist/maestro-local-server_*.deb Package Version Architecture Depends
echo "::endgroup::"
