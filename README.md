# maestro-local-server

A local daemon that exposes [Maestro](https://github.com/QoroQuantum/maestro) quantum circuit
simulation over a Unix domain socket. Clients connect to the socket and issue simple
line-based text commands to create sessions, submit simulation tasks (circuit execution or
observable estimation), poll their status, and retrieve results as JSON.

The server listens on `/run/maestro.sock` when run as the installed systemd service (or
`/tmp/maestro.sock` in test mode). Each client session tracks a set of tasks; tasks are queued
and executed one at a time by a background runner thread, which dispatches them into the
Maestro simulation engine (via FFI bindings generated from Maestro's C `Interface.h`). Sessions
that stay idle for too long are automatically cleaned up.

Supported commands (newline-terminated):

- `SESSION CREATE` / `SESSION <id> EXISTS` / `SESSION <id> DELETE`
- `SESSION <id> TASK CREATE`
- `SESSION <id> TASK <task_id> {EXISTS, VALID, PENDING, RUNNING, FINISHED, FAILED, CANCEL, EXECUTE, GET_RESULTS}`
- `SESSION <id> TASK <task_id> {SET_TYPE, SET_QUBITS, SET_SIM_TYPE, SET_METHOD, SET_QASM, SET_OBSERVABLES, SET_OPTIONS} <value>`
- `PING`

## Prerequisites

- Rust toolchain (stable, edition 2024) — install via [rustup](https://rustup.rs)
- `libclang` (required by `bindgen` at build time), e.g. `sudo apt install libclang-dev`
- The [Maestro](https://github.com/QoroQuantum/maestro) C++ library built and installed so that
  `libmaestro.so` and `maestrolib/Interface.h` are available; this crate links against it
  dynamically and looks for it in `/usr/local/`. Build it from the sibling `maestro` checkout
  with `./build.sh` and install it before building this project.
- For building the `.deb` package: `cargo install cargo-deb`

## Compiling

```bash
cargo build --release
```

The compiled binary is written to `target/release/maestro-local-server`.

## Testing

Tests exercise the Unix socket server and the simulation runner end-to-end, sharing the
`/tmp/maestro.sock` path and the Maestro engine. They are annotated with `#[serial]`, which
already enforces serialized execution, so a plain:

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
