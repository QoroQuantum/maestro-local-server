# Installation and Deployment Guide

This guide covers building, installing, configuring, and verifying `maestro-local-server` from source or as a Debian/Ubuntu systemd service.

---

## 1. System Requirements

`maestro-local-server` runs on Linux systems (and POSIX environments for local development/testing) and requires:

- **Rust toolchain** (stable, supporting Rust 2024 edition, Rust 1.85+) — install via [rustup](https://rustup.rs)
- **`libclang`** — required at build time by `bindgen` to generate Rust FFI bindings from Maestro's C header (`Interface.h`)
- **[Maestro](https://github.com/QoroQuantum/maestro) C++ shared library (`libmaestro.so`) and headers (`Interface.h`, `InterfaceTypes.h`)** — built from a matching Maestro revision
- **Boost 1.89 or newer** with the `program_options`, `json`, `container`, and `serialization` libraries — older releases (for example, Boost 1.83 on Ubuntu 24.04) fail to compile Maestro with `call of overloaded 'serialize(double&)' is ambiguous`

### Installing System Packages

**Ubuntu / Debian:**

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  cmake \
  ninja-build \
  git \
  curl \
  libclang-dev \
  libfftw3-dev \
  libopenblas-dev
```

Then install Boost. Check the version your distribution provides with `apt-cache policy libboost-all-dev`.

- **Ubuntu 26.04 or newer** (Boost 1.90): install the distribution package:

  ```bash
  sudo apt-get install -y libboost-all-dev
  ```

- **Ubuntu 24.04 / 25.10 and other distributions with Boost older than 1.89**: build Boost from source into `/usr/local` (this is what CI does). Do not also install `libboost-all-dev`.

  ```bash
  cd /tmp
  curl -L https://archives.boost.io/release/1.90.0/source/boost_1_90_0.tar.gz -o boost_1_90_0.tar.gz
  tar -xzf boost_1_90_0.tar.gz
  cd boost_1_90_0
  ./bootstrap.sh --prefix=/usr/local
  sudo ./b2 -j"$(nproc)" --with-program_options --with-json --with-container --with-serialization install
  sudo ldconfig
  ```

**Fedora / RHEL:**

```bash
sudo dnf install -y \
  gcc-c++ \
  cmake \
  ninja-build \
  git \
  curl \
  clang-devel \
  fftw-devel \
  boost-devel \
  openblas-devel
```

Check that `rpm -q boost-devel` reports 1.89 or newer (Fedora 45 ships 1.90). If it is older, remove `boost-devel` from the command and build Boost from source as shown for Ubuntu above.

### Installing the Rust Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

---

## 2. Building and Installing the Maestro C++ Library

`maestro-local-server` links dynamically against `libmaestro.so` and generates bindings against `maestrolib/Interface.h`. Always build Maestro and `maestro-local-server` from compatible revisions.

```bash
git clone https://github.com/QoroQuantum/maestro.git "$HOME/maestro"
cd "$HOME/maestro"

cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=/usr/local \
  -DBUILD_PYTHON_BINDINGS=OFF \
  -DCOMPILE_TESTS=OFF

cmake --build build -j2
```

`-DCOMPILE_TESTS=OFF` skips Maestro's own test binaries, which `maestro-local-server` does not need. This roughly halves the build time and memory use.

> **ARM (`aarch64`) hosts:** Qiskit Aer, which is enabled by default, uses x86-only intrinsics and fails to build on ARM with `immintrin.h: No such file or directory`. Add `-DENABLE_QISKIT_AER=OFF` to the `cmake -S . -B build ...` command. The other backends are unaffected.

To install `libmaestro.so` system-wide and register it with the dynamic linker:

```bash
sudo cmake --install build
echo "/usr/local" | sudo tee /etc/ld.so.conf.d/maestro.conf
echo "/usr/local/lib" | sudo tee -a /etc/ld.so.conf.d/maestro.conf
sudo ldconfig
```

> **Memory Tip:** Each Maestro compile job can use several gigabytes of RAM, more with Qiskit Aer enabled. With 8 GB of RAM, `-j3` runs out of memory, so the command above uses `-j2`. On machines with 32 GB or more, you can raise it (for example, `-j"$(nproc)"`). If the compiler is killed, rerun with `-j1`; the build continues where it stopped.

---

## 3. Building `maestro-local-server` from Source

Clone this repository:

```bash
git clone https://github.com/QoroQuantum/maestro-local-server.git
cd maestro-local-server
```

### Option A: Using Default Paths (`~/maestro` + System Linker)

If Maestro is checked out at `~/maestro` and `libmaestro.so` is installed in the system linker path:

```bash
cargo build --release
```

### Option B: Pointing to a Specific Maestro Checkout (`MAESTRO_DIR`)

If your Maestro checkout and CMake build directory (`build/libmaestro.so`) are in another location, set `MAESTRO_DIR`:

```bash
MAESTRO_DIR=/path/to/maestro cargo build --release
```

When `MAESTRO_DIR` or `MAESTRO_LIBRARY_DIR` is set at build time, `build.rs` verifies that `libmaestro.so` exists in that directory and embeds it into the server binary's `rpath`.

### Option C: Separate Header and Library Overrides

You can override the header and library directory independently:

| Input | Specific Override | With `MAESTRO_DIR` Set | Default (No Override) |
| --- | --- | --- | --- |
| Header | `MAESTRO_HEADER` | `$MAESTRO_DIR/maestrolib/Interface.h` | `$HOME/maestro/maestrolib/Interface.h` |
| Library dir | `MAESTRO_LIBRARY_DIR` | `$MAESTRO_DIR/build` | System linker search + `/usr/local/lib` + `/usr/local` |

```bash
MAESTRO_HEADER=/opt/maestro/include/Interface.h \
MAESTRO_LIBRARY_DIR=/opt/maestro/lib \
cargo build --release
```

> **Note:** Cargo tracks only Maestro environment variables that were defined during the build. If you first set a previously unset override after a prior build, run `cargo clean -p maestro-local-server --release` before rebuilding. Without `--release`, Cargo cleans only the debug profile and the release binary keeps its old settings.

The compiled binary is produced at:

```text
target/release/maestro-local-server
```

---

## 4. Installing as a Debian/Ubuntu Package (`.deb`)

For production Linux deployments, `maestro-local-server` can be packaged into a `.deb` archive that installs `/usr/bin/maestro-local-server` and manages the `maestro.service` systemd unit.

### Step 1: Install `cargo-deb`

```bash
cargo install cargo-deb
```

### Step 2: Build the `.deb` Package

```bash
cargo deb
```

(Or `MAESTRO_DIR=/path/to/maestro cargo deb` if building against a specific Maestro checkout.)

This produces an archive under `target/debian/`:

```text
target/debian/maestro-local-server_<version>_<arch>.deb
```

### Step 3: Install the Package

```bash
sudo dpkg -i target/debian/maestro-local-server_*.deb
```

The package's post-install script (`debian/scripts/postinst`) unmasks `maestro.service`, enables it, and starts it. Check that it is running:

```bash
sudo systemctl status maestro.service
```

If it is not running (for example, after you disabled it earlier), enable and start it:

```bash
sudo systemctl enable --now maestro.service
```

### Step 4: Uninstalling

```bash
# Stop and remove package (masks unit)
sudo dpkg -r maestro-local-server

# Or purge completely
sudo dpkg -P maestro-local-server
```

---

## 5. Configuring the Systemd Service and Socket Permissions

### Socket Location and Permissions

By default, the production daemon creates its Unix domain socket at `/run/maestro.sock` and sets permissions to `0660` (`rw-rw----`). If it cannot create the socket, or later stops accepting connections, it reports the cause, stops its runner and exits with status 1, so the service's `Restart=always` policy restarts it.

The packaged systemd service ([`debian/maestro.service`](../debian/maestro.service)) runs with `Group=users`:

```ini
[Unit]
AssertPathExists=/usr/bin/maestro-local-server

[Service]
WorkingDirectory=~
ExecStart=/usr/bin/maestro-local-server
Restart=always
PrivateTmp=true
NoNewPrivileges=true
RuntimeDirectoryMode=0770
Group=users

[Install]
Alias=maestro.service
WantedBy=default.target
```

To grant a local user access to `/run/maestro.sock`, ensure they belong to the configured group:

```bash
sudo usermod -aG users <username>
```

(Log out and back in for the group membership to take effect.)

### Customizing Runtime Environment Variables

You can customize the service without editing `/lib/systemd/system/maestro.service` directly by creating a systemd drop-in override:

```bash
sudo systemctl edit maestro.service
```

Add any required environment variables under `[Service]`. The example below sets the session lease to two hours:

```ini
[Service]
Environment="MAESTRO_SESSION_LEASE_SECONDS=7200"
# Only set this once the file exists. If MAESTRO_MPI_PROFILES points to a
# missing or invalid file, the capabilities command fails with invalid_config.
#Environment="MAESTRO_MPI_PROFILES=/etc/maestro/mpi-profiles.json"
# Not needed when libmaestro.so is registered with ldconfig (section 2).
#Environment="LD_LIBRARY_PATH=/usr/local/lib:/usr/local"
```

Then reload and restart:

```bash
sudo systemctl daemon-reload
sudo systemctl restart maestro.service
```

### Environment Variable Reference

| Variable | Default | Description |
| --- | --- | --- |
| `MAESTRO_SOCKET` | `/run/maestro.sock` | Path to the Unix domain socket created by the daemon. (The `cargo test` suite uses `/tmp/maestro.sock`.) |
| `MAESTRO_SESSION_LEASE_SECONDS` | `3600` | Positive integer setting the idle session lease duration in seconds. Sessions expire after `5 * MAESTRO_SESSION_LEASE_SECONDS` ticks (at 200 ms per tick, i.e., `MAESTRO_SESSION_LEASE_SECONDS` seconds) without activity. |
| `MAESTRO_MPI_PROFILES` | *(unset)* | Path to an administrator-owned JSON file defining allowed MPI launch profiles (`mpirun` or `srun`). See [Native MPI profiles](native-api-v2.md#native-mpi-profiles). |

---

## 6. Running Manually (Without Systemd)

For local development or isolated testing, you can run the server directly with a custom socket path that does not require `root` privileges:

```bash
MAESTRO_SOCKET=/tmp/maestro-dev.sock ./target/release/maestro-local-server
```

Or run with the default `/run/maestro.sock` path under `root` and group `users`:

```bash
sudo -u root -g users ./target/release/maestro-local-server
```

---

## 7. Verifying the Installation

Once the daemon is running, verify connectivity over the Unix socket using `nc` from OpenBSD netcat (`sudo apt-get install -y netcat-openbsd`) or `socat` (`sudo apt-get install -y socat`). The examples below use `nc`; with `socat`, replace `nc -U -N /run/maestro.sock` with `socat - UNIX-CONNECT:/run/maestro.sock`.

### 1. Liveness Check (`PING`)

```bash
printf 'PING\n' | nc -U -N /run/maestro.sock
```

Expected response:

```text
OK
```

### 2. Runner Health Status (`API status`)

```bash
printf 'API {"version":2,"command":"status"}\n' | nc -U -N /run/maestro.sock
```

Expected response:

```text
OK {"version":2,"ok":true,"status":{"accepting_jobs":true,"healthy":true,"busy":false,"pending_job_count":0,"status_reason":null,"health_scope":"runner"}}
```

### 3. Capabilities and Build Metadata (`API capabilities`)

```bash
printf 'API {"version":2,"command":"capabilities"}\n' | nc -U -N /run/maestro.sock
```

Verify that `"ok":true` is returned and inspect `server.build` and `native.backends`.

---

## 8. Troubleshooting

### `call of overloaded 'serialize(double&)' is ambiguous` while building Maestro

Maestro was configured against a Boost release older than 1.89. Build Boost 1.90 from source into `/usr/local` as shown in [section 1](#installing-system-packages), then delete Maestro's `build` directory and configure again so CMake picks up the new Boost.

### `fatal error: immintrin.h: No such file or directory` while building Maestro

You are building on an ARM (`aarch64`) host with Qiskit Aer enabled. Aer's sources use x86-only intrinsics. Configure Maestro with `-DENABLE_QISKIT_AER=OFF` (see [section 2](#2-building-and-installing-the-maestro-c-library)).

### `c++: fatal error: Killed signal terminated program cc1plus` while building Maestro

The compiler ran out of memory. Lower the parallel job count (for example, `cmake --build build -j2` or `-j1`) and run the build command again. It continues where it stopped.

### `error while loading shared libraries: libmaestro.so: cannot open shared object file`

The runtime dynamic linker cannot find `libmaestro.so`. Either:
- Install `libmaestro.so` to `/usr/local/lib` or `/usr/local`, add the path to `/etc/ld.so.conf.d/maestro.conf`, and run `sudo ldconfig`, or
- Build with `MAESTRO_DIR=/path/to/maestro` or `MAESTRO_LIBRARY_DIR=/path/to/lib` so the library directory is embedded in the binary's `rpath`, or
- Set `LD_LIBRARY_PATH` in the process or systemd environment.

### `Maestro header not found at ...; set MAESTRO_DIR or MAESTRO_HEADER`

`build.rs` could not find `Interface.h` at the default `$HOME/maestro/maestrolib/Interface.h` path. Set `MAESTRO_DIR=/path/to/maestro` or `MAESTRO_HEADER=/path/to/Interface.h` when running `cargo build`. Also ensure `InterfaceTypes.h` is present in the same directory as `Interface.h`.

### `Unable to find libclang` during `cargo build`

`bindgen` requires `libclang` at build time. Install `libclang-dev` (Ubuntu/Debian) or `clang-devel` (Fedora/RHEL). If `libclang` is installed in a non-standard path, set `LIBCLANG_PATH=/path/to/llvm/lib`.

### `Cannot listen on <path>` and the service restarting

The daemon could not create its socket, for example because the directory in `MAESTRO_SOCKET` does not exist or is not writable by the service user. It exits with status 1 rather than running without a socket; check `journalctl -u maestro.service` for the cause.

### `Permission denied` when connecting to `/run/maestro.sock`

The socket is created with mode `0660` (`rw-rw----`). Ensure your user account is a member of the group under which `maestro-local-server` runs (`users` by default in `maestro.service`), or connect using a user in that group.
