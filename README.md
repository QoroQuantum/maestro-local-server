# maestro-local-server

[![Build and Test](https://github.com/QoroQuantum/maestro-local-server/actions/workflows/build-and-test.yml/badge.svg)](https://github.com/QoroQuantum/maestro-local-server/actions/workflows/build-and-test.yml)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](./LICENSE)

A local daemon that exposes [Maestro](https://github.com/QoroQuantum/maestro) quantum circuit
simulation over a Unix domain socket. Clients connect to the socket and issue line-based
commands or versioned JSON API v2 requests to create sessions, submit simulation tasks
(circuit execution, observable estimation, statevector/MPO/MPS diagnostics, or batched jobs),
poll their status, inspect bounded task logs, and retrieve results as JSON.

## Overview and Architecture

- **Unix Domain Socket Transport**: Listens on `/run/maestro.sock` when run as the installed
  systemd service (or `/tmp/maestro.sock` in test mode, overridable via `MAESTRO_SOCKET`).
- **Session and Queue Management**: Each client session tracks a set of tasks with automatic
  idle-session lease expiration (`MAESTRO_SESSION_LEASE_SECONDS`). Tasks are queued and
  executed sequentially by a background runner thread.
- **Per-Task Worker Isolation**: Each task runs in a fresh subprocess of the server executable
  in `--worker` mode (or `maestro-mpi-worker` when an MPI launch profile is selected), calling
  Maestro through its C interface. Native validation also runs in an isolated worker before a
  task ID is allocated, ensuring parser or backend crashes cannot take down the daemon.
- **Deterministic Cancellation**: Cancelling a task, deleting or expiring a session, or
  shutting down the daemon terminates and reaps the active worker process group and suppresses
  late results.

## Supported Commands

The server accepts newline-terminated commands in two protocol families on the same socket:

- **Native JSON API v2** (`API <json>`):
  - `API {"version":2,"command":"status"}` — runner health and queue status
  - `API {"version":2,"command":"capabilities"}` — native backend/method/option catalog, build metadata, and MPI profiles
  - `API {"version":2,"command":"submit","session_id":<id>,"request":{...}}` — atomic validation and submission of native schema v2 requests
  - `API {"version":2,"command":"logs","session_id":<id>,"task_id":<task_id>}` — retained stdout/stderr, lifecycle events, seeds, and structured errors
  - `API {"version":2,"command":"keepalive","session_id":<id>}` — session lease renewal
- **Session and Task Commands**:
  - `SESSION CREATE` / `SESSION <id> EXISTS` / `SESSION <id> DELETE`
  - `SESSION <id> TASK CREATE`
  - `SESSION <id> TASK <task_id> {EXISTS, VALID, PENDING, RUNNING, FINISHED, FAILED, CANCEL, EXECUTE, GET_RESULTS}`
  - `SESSION <id> TASK <task_id> {SET_TYPE, SET_QUBITS, SET_SIM_TYPE, SET_METHOD, SET_QASM, SET_OBSERVABLES, SET_OPTIONS} <value>`
  - `PING`

## Documentation

- **[Installation and Deployment Guide](docs/installation.md)** — system packages, building Maestro and `maestro-local-server` from source, `.deb` packaging, systemd configuration, socket permissions, and troubleshooting.
- **[Client Protocol and Usage Guide](docs/protocol-guide.md)** — end-to-end walkthroughs, command reference tables, and a complete Python client example.
- **[Native Maestro API v2 Reference](docs/native-api-v2.md)** — request protocol details, option migration table, MPS/MPO tensor diagnostics, gate fusion, GPU plugin upgrade guidance, and supervised MPI launch profiles.
- **[Worker Process Design](docs/worker-process.md)** — module layout, private IPC protocol, cancellation ordering, and subprocess isolation design.

## Prerequisites

- Rust toolchain (stable, edition 2024) — install via [rustup](https://rustup.rs)
- `libclang` (required by `bindgen` at build time), e.g. `sudo apt install libclang-dev`
- The [Maestro](https://github.com/QoroQuantum/maestro) C++ library (`libmaestro.so` and `maestrolib/Interface.h`) built from source, which needs Boost 1.89 or newer (Ubuntu 24.04's packaged Boost 1.83 is too old)
- For building the `.deb` package: `cargo install cargo-deb`

See the [Installation and Deployment Guide](docs/installation.md) for full step-by-step instructions.

## Local Development Setup

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
`cargo clean -p maestro-local-server --release` (or drop `--release` for debug
builds) before rebuilding so Cargo reads the new
setting. Changes to already tracked variables or the selected header trigger
rebuilds normally.

Rebuild Maestro and the server together so their native API versions match.
See [Native Maestro API v2](docs/native-api-v2.md) for the request protocol,
MPI profiles, and runtime configuration.
The native API also exposes current MPS/MPO diagnostics, batched expectations,
wide MPO bitstrings, and gate-fusion controls. See the
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

### Formatting and Linting

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

## Building and Installing the `.deb` Package

```bash
cargo deb
sudo dpkg -i target/debian/maestro-local-server_<version>_<arch>.deb
```

This installs the binary to `/usr/bin/maestro-local-server` and the systemd unit to
`/lib/systemd/system/maestro.service`, then enables and starts the `maestro` service
automatically via the package's post-install hook. To manage it manually:

```bash
sudo systemctl status maestro.service
sudo systemctl restart maestro.service
```

To uninstall:

```bash
sudo dpkg -r maestro-local-server
```

This stops the service and masks the systemd unit before removal. To purge configuration/state
as well, use `sudo dpkg -P maestro-local-server` instead.

## Contributing and Security

- See [CONTRIBUTING.md](./CONTRIBUTING.md) for guidelines on reporting bugs, requesting features, and sharing feedback.
- See [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md) for community standards.
- See [SECURITY.md](./SECURITY.md) for reporting security vulnerabilities and reviewing deployment security considerations.

## Citation

If you use Maestro or `maestro-local-server` in your research or workflow, please cite:

```bibtex
@article{bertomeu2025maestro,
  title={Maestro: Intelligent Execution for Quantum Circuit Simulation},
  author={Bertomeu, Oriol and Ghayas, Hamzah and Roman, Adrian and DiAdamo, Stephen},
  organization={Qoro Quantum},
  year={2025}
}
```

## License

GNU General Public License v3.0 (GPL-3.0) with the Maestro Plugin Linking Exception — see [LICENSE](./LICENSE) or <https://www.gnu.org/licenses/gpl-3.0.en.html>.
