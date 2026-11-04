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

With the Maestro header at `~/maestro/maestrolib/Interface.h` and a matching
`libmaestro.so` installed, no Maestro environment variables are required:

```bash
cargo build --release
sudo -u root -g users ./target/release/maestro-local-server
```

The linker uses its normal library search paths, with `/usr/local/lib` and
`/usr/local` added. The build does not require the library to be in either of
those directories. At runtime the library must be discoverable by the system
loader, for example through the existing linker cache or `LD_LIBRARY_PATH`.

Explicit paths remain available, in this order of precedence:

| Input | Specific override | With `MAESTRO_DIR` set | With no override |
| --- | --- | --- | --- |
| Header | `MAESTRO_HEADER` | `$MAESTRO_DIR/maestrolib/Interface.h` | `$HOME/maestro/maestrolib/Interface.h` |
| Library directory | `MAESTRO_LIBRARY_DIR` | `$MAESTRO_DIR/build` | Normal linker search, plus `/usr/local/lib` and `/usr/local` |

For example, `MAESTRO_DIR=/path/to/maestro cargo build --release` builds against
that checkout. The specific header/library overrides work independently and
also support an installed header or a separate CMake build directory. An
explicit library directory must contain `libmaestro.so` and is embedded in the
server binary's runtime search path. Invalid explicit paths report an error.

Cargo environment-change tracking is emitted only for defined Maestro variables.
`HOME` is tracked only when it supplies the default header location. When setting
a previously unset override after a successful build, use
`cargo clean -p maestro-local-server` before rebuilding so Cargo reads the new
setting. Changes to already tracked variables or the selected header trigger
rebuilds normally.

Rebuild Maestro and the server together so their native API versions match.
See [Native Maestro API v2](docs/native-api-v2.md) for the request protocol,
MPI profiles, and runtime configuration.
The native API also exposes current MPS/MPO diagnostics, batched expectations,
wide MPO bitstrings, gate-fusion controls, routing/reduction/GPU Pauli options,
adjoint differentiation, observable mirror fidelity, and instruction-level noise
controls; task logs report the Maestro build that ran each task. See the
[option migration table](docs/native-api-v2.md#current-native-options-and-client-migration)
before upgrading clients: native schema version 2 includes renamed options.
Follow the [native/GPU upgrade guidance](docs/native-api-v2.md#upgrading-native-and-gpu-libraries)
when replacing libraries or installing the current headers.

After changing Maestro, rebuild both projects and restart the server. For a
checkout selected with `MAESTRO_DIR`:

```bash
export MAESTRO_DIR="$HOME/maestro"
cmake --build "$MAESTRO_DIR/build" -j8
cargo build --release
```

When using normal library discovery, also update the installed library after
rebuilding Maestro.

The compiled binary is written to `target/release/maestro-local-server`.

### Formatting and linting

Before submitting changes, run both checks and resolve all formatting issues,
warnings, and errors:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

To apply Rust formatting fixes, run:

```bash
cargo fmt --all --
```

Then rerun both checks. Clippy uses the same Maestro header and library
configuration as `cargo build`.

## Testing

Tests exercise the Unix socket server, worker processes, and native simulation end-to-end.
Tests sharing `/tmp/maestro.sock` or the in-process Maestro engine use `#[serial]`;
isolated worker tests also cover cancellation, crashes, and IPC failures. A plain:

```bash
cargo test
```

is sufficient.

The `native_api` integration suite launches private daemons and tests the current
native JSON contract through the public socket and production workers. It runs
on CPU without GPU plugins. Run it alone with `cargo test --test native_api`.
GPU hardware verification is described in the
[native API verification guide](docs/native-api-v2.md#verification).

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
