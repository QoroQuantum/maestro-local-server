# maestro-local-server

[![Build and Test](https://github.com/QoroQuantum/maestro-local-server/actions/workflows/build-and-test.yml/badge.svg)](https://github.com/QoroQuantum/maestro-local-server/actions/workflows/build-and-test.yml)

A local daemon that exposes [Maestro](https://github.com/QoroQuantum/maestro) quantum circuit
simulation over a Unix domain socket. Clients connect to the socket and issue simple
line-based text commands to create sessions, submit simulation tasks (circuit execution or
observable estimation), poll their status, and retrieve results as JSON.

The server listens on `/run/maestro.sock` when run as the installed systemd service (or
`/tmp/maestro.sock` in test mode). Each client session tracks a set of tasks; tasks are queued
and executed one at a time by a background runner thread. Each task runs in a fresh copy of
the server executable in `--worker` mode, which calls Maestro through its C interface.
Cancelling a running task terminates and reaps its worker process. Session deletion,
idle-session cleanup, and daemon shutdown also cancel active work.

The [worker process design](docs/worker-process.md) describes the implementation plan,
module layout, private IPC protocol, cancellation ordering, and tests.

Supported commands (newline-terminated):

- `SESSION CREATE` / `SESSION <id> EXISTS` / `SESSION <id> DELETE`
- `SESSION <id> TASK CREATE`
- `SESSION <id> TASK <task_id> {EXISTS, VALID, PENDING, RUNNING, FINISHED, FAILED, CANCEL, EXECUTE, GET_RESULTS}`
- `SESSION <id> TASK <task_id> {SET_TYPE, SET_QUBITS, SET_SIM_TYPE, SET_METHOD, SET_QASM, SET_OBSERVABLES, SET_OPTIONS} <value>`
- `PING`

## Prerequisites

- Rust toolchain (stable, edition 2024) — install via [rustup](https://rustup.rs)
- `libclang` (required by `bindgen` at build time), e.g. `sudo apt install libclang-dev`
- The [Maestro](https://github.com/QoroQuantum/maestro) C++ library built from source.
- For building the `.deb` package: `cargo install cargo-deb`

## Local development setup

With `maestro` and `maestro-local-server` checked out next to each other:

```bash
export MAESTRO_DIR="$(realpath ../maestro)"
cargo build --release
sudo -u root -g users ./target/release/maestro-local-server
```

`MAESTRO_DIR` may point to a Maestro checkout anywhere on the system. Its `build/libmaestro.so`
location is embedded in the server binary, so `LD_LIBRARY_PATH` is not required.

For a separate CMake build directory, set `MAESTRO_LIBRARY_DIR` to the directory
containing `libmaestro.so`. `MAESTRO_HEADER` can override the path to `Interface.h`.
Both overrides take precedence over `MAESTRO_DIR`; setting both also allows builds
without `MAESTRO_DIR`. The selected library directory is used for the runtime
search path. Rebuild Maestro and the server together so their native API versions
match. See [Native Maestro API v2](docs/native-api-v2.md) for the request protocol,
MPI profiles, and runtime configuration.

After changing Maestro, rebuild both projects and restart the server:

```bash
cd ../maestro
cmake --build build -j8

cd ../maestro-local-server
export MAESTRO_DIR="$(realpath ../maestro)"
cargo build --release
```

The compiled binary is written to `target/release/maestro-local-server`.

## Testing

Tests exercise the Unix socket server, worker processes, and native simulation end-to-end.
Tests sharing `/tmp/maestro.sock` or the in-process Maestro engine use `#[serial]`;
isolated worker tests also cover cancellation, crashes, and IPC failures. A plain:

```bash
cargo test
```

is sufficient.

## Building the .deb package

```bash
cargo deb
```

This builds a release binary and packages it, along with the `maestro.service` systemd unit,
into a `.deb` under `target/debian/`.

## Installing

```bash
sudo dpkg -i target/debian/maestro-local-server_<version>_<arch>.deb
```

This installs the binary to `/usr/bin/maestro-local-server` and the systemd unit to
`/lib/systemd/system/maestro.service`, then enables and starts the `maestro` service
automatically via the package's post-install hook. To manage it manually:

```bash
sudo systemctl status maestro.service
sudo systemctl restart maestro.service
```

## Uninstalling

```bash
sudo dpkg -r maestro-local-server
```

This stops the service and masks the systemd unit before removal. To purge configuration/state
as well, use `sudo dpkg -P maestro-local-server` instead.
