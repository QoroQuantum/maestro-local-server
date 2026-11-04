# Installation and Deployment Guide

This guide covers building, installing, configuring, and verifying `maestro-local-server` on Linux, either from source or as a Debian/Ubuntu systemd service. It uses a prebuilt Maestro library from a release archive, so you do not compile Maestro or install Boost, OpenBLAS or OpenMP yourself. [Appendix A](#appendix-a-building-maestro-from-source) covers building Maestro from source instead.

**Shortcut:** once the prerequisites in section 1 are installed, `scripts/install.sh` does sections 2 and 3 for you (download and verify the Maestro archive, extract it to `/opt/maestro`, build the server), and `scripts/install.sh --deb` also does section 4 (build and install the package, and start the service). It is safe to rerun and skips the download if the pinned Maestro version is already installed. The sections below explain each step.

---

## 1. System Requirements

- **Linux, x86_64 or aarch64, with glibc 2.28 or newer.** The Maestro archive is built in the `manylinux_2_28` image (glibc 2.28 baseline) and is checked on Debian 13 in its release workflow; this guide's steps were also run on Ubuntu 24.04. The server also builds and runs on other POSIX systems for development, but this guide only covers Linux.
- **Rust toolchain** (stable, supporting Rust 2024 edition, Rust 1.85+) — install via [rustup](https://rustup.rs)
- **`libclang`** — required at build time by `bindgen` to generate Rust FFI bindings from Maestro's C header (`Interface.h`)
- **A C compiler and linker, `curl` and `git`**

The Maestro library, with the Boost, OpenBLAS and OpenMP libraries it loads, comes in the archive from section 2.

**Ubuntu / Debian:**

```bash
sudo apt-get update
sudo apt-get install -y build-essential curl git libclang-dev
```

**Fedora / RHEL:**

```bash
sudo dnf install -y gcc-c++ curl git clang-devel
```

**Rust toolchain:**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

---

## 2. Installing Maestro (Prebuilt)

This version of `maestro-local-server` is pinned to **Maestro 0.3.12**. Download the archive for your architecture, check it against the published checksums, and extract it to `/opt/maestro`:

```bash
MAESTRO_VERSION=0.3.12
ARCH=$(uname -m)   # x86_64 or aarch64
BASE_URL="https://github.com/QoroQuantum/maestro/releases/download/v${MAESTRO_VERSION}"

cd /tmp
curl -fsSLO "${BASE_URL}/maestro-c-${MAESTRO_VERSION}-linux-${ARCH}.tar.gz"
curl -fsSLO "${BASE_URL}/maestro-c-${MAESTRO_VERSION}-SHA256SUMS"
sha256sum --ignore-missing -c "maestro-c-${MAESTRO_VERSION}-SHA256SUMS"

sudo mkdir -p /opt/maestro
sudo tar -xzf "maestro-c-${MAESTRO_VERSION}-linux-${ARCH}.tar.gz" -C /opt/maestro --strip-components=1
cat /opt/maestro/BUILD_INFO
```

The archive contains:

| Path | Contents |
| --- | --- |
| `include/maestrolib/` | `Interface.h` and `InterfaceTypes.h`, the C headers the server's bindings are generated from |
| `lib/` | `libmaestro.so` and the libraries it loads (Boost and OpenMP, plus OpenBLAS on x86_64). Each has `RUNPATH=$ORIGIN`, so the directory works wherever it is placed |
| `bin/maestro-request` | Maestro's command-line request runner |
| `BUILD_INFO` | Version, source commit, architecture, whether Qiskit Aer is included, and the bundled libraries |

Things to know:

- The **x86_64** archive includes Qiskit Aer. The **aarch64** archive does not, because Aer uses x86-only intrinsics; the other backends are unaffected.
- Maestro 0.3.12 predates the Maestro changes behind the server's newest native features: adjoint differentiation, observable mirror fidelity, mixed-state overlap and fidelity queries, instruction-level noise, and the readout and result-size checks. Requests that use them are rejected at submission with `unsupported_capability`; everything else works. To use them, build Maestro from the commit CI tests against (see [Appendix A](#appendix-a-building-maestro-from-source)).
- There is no archive for other architectures or operating systems. Use Appendix A there.

---

## 3. Building `maestro-local-server` from Source

Clone this repository and build it against the archive:

```bash
cd "$HOME"
git clone https://github.com/QoroQuantum/maestro-local-server.git
cd maestro-local-server

export MAESTRO_HEADER=/opt/maestro/include/maestrolib/Interface.h
export MAESTRO_LIBRARY_DIR=/opt/maestro/lib
cargo build --release
```

`build.rs` verifies that `libmaestro.so` exists in `MAESTRO_LIBRARY_DIR` and embeds that directory in the server binary's runtime search path, so the service needs no `LD_LIBRARY_PATH` or `ldconfig` setup.

The compiled binary is produced at:

```text
target/release/maestro-local-server
```

> **Note:** Cargo tracks only Maestro environment variables that were defined during the build. If you first set a previously unset override after a prior build, run `cargo clean -p maestro-local-server --release` before rebuilding. Without `--release`, Cargo cleans only the debug profile and the release binary keeps its old settings.

### Using a different Maestro location

The two variables above are the general mechanism. You can also point the build at a Maestro source checkout (see [Appendix A](#appendix-a-building-maestro-from-source)):

| Input | Specific Override | With `MAESTRO_DIR` Set | Default (No Override) |
| --- | --- | --- | --- |
| Header | `MAESTRO_HEADER` | `$MAESTRO_DIR/maestrolib/Interface.h` | `$HOME/maestro/maestrolib/Interface.h` |
| Library dir | `MAESTRO_LIBRARY_DIR` | `$MAESTRO_DIR/build` | System linker search + `/usr/local/lib` + `/usr/local` |

For example, `MAESTRO_DIR=/path/to/maestro cargo build --release` builds against a Maestro checkout with its CMake build in `build/`. An explicit library directory must contain `libmaestro.so`.

---

## 4. Installing as a Debian/Ubuntu Package (`.deb`)

For production Linux deployments, `maestro-local-server` can be packaged into a `.deb` archive that installs `/usr/bin/maestro-local-server` and manages the `maestro.service` systemd unit.

> **Prebuilt packages:** each [GitHub release](https://github.com/QoroQuantum/maestro-local-server/releases) has `.deb` packages for x86_64 (`amd64`) and aarch64 (`arm64`), built against the pinned Maestro release, for Ubuntu 22.04+ and Debian 12+. Install Maestro to `/opt/maestro` as in section 2, then install the package with `sudo apt install ./maestro-local-server_<version>-1_<arch>.deb`; `apt` (unlike `dpkg -i`) also installs its dependencies. You can skip the build steps below.

### Step 1: Install `cargo-deb`

```bash
cargo install cargo-deb
```

### Step 2: Build the `.deb` Package

```bash
cargo deb
```

Run it with the same two variables as in section 3 (for example `MAESTRO_HEADER=/opt/maestro/include/maestrolib/Interface.h MAESTRO_LIBRARY_DIR=/opt/maestro/lib cargo deb`), or export them first.

> **The package contains only the server, not `libmaestro.so`.** The server finds Maestro through the library directory embedded at build time (`/opt/maestro/lib` above), so every host that installs the package needs the Maestro archive extracted at that same path (section 2). Without it the service fails to start with `error while loading shared libraries: libmaestro.so`.

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
# Not needed: the server binary finds libmaestro.so through the library
# directory embedded at build time.
#Environment="LD_LIBRARY_PATH=/opt/maestro/lib"
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

### `error while loading shared libraries: libmaestro.so: cannot open shared object file`

The server cannot find Maestro at the library directory embedded when it was built. Either:
- Extract the Maestro archive at that path on this host (`/opt/maestro/lib` if you followed section 2); this is the usual cause when the `.deb` was installed on a different machine than it was built on, or
- Rebuild with `MAESTRO_LIBRARY_DIR` pointing at where the library actually is, or
- Set `LD_LIBRARY_PATH` in the process or systemd environment as a stopgap.

The journal (`journalctl -u maestro.service`) shows the same message. After fixing the library path, clear systemd's failure state before restarting. The unit is set to restart on failure, and systemd stops retrying after several quick failures ("Start request repeated too quickly"), so a plain `restart` is refused:

```bash
sudo systemctl reset-failed maestro.service
sudo systemctl restart maestro.service
```

### `Maestro header not found at ...; set MAESTRO_DIR or MAESTRO_HEADER`

`build.rs` could not find `Interface.h`. Set `MAESTRO_HEADER=/opt/maestro/include/maestrolib/Interface.h` (or `MAESTRO_DIR=/path/to/maestro` for a source checkout) when running `cargo build`. `InterfaceTypes.h` must be in the same directory as `Interface.h`.

### `Unable to find libclang` during `cargo build`

`bindgen` requires `libclang` at build time. Install `libclang-dev` (Ubuntu/Debian) or `clang-devel` (Fedora/RHEL). If `libclang` is installed in a non-standard path, set `LIBCLANG_PATH=/path/to/llvm/lib`.

### `Cannot listen on <path>` and the service restarting

The daemon could not create its socket, for example because the directory in `MAESTRO_SOCKET` does not exist or is not writable by the service user. It exits with status 1 rather than running without a socket; check `journalctl -u maestro.service` for the cause.

### `sha256sum: ... FAILED` or `no properly formatted checksum lines found`

The download is incomplete or corrupt, or you downloaded the files for different versions. Delete both files from `/tmp` and repeat the download in section 2. Do not extract an archive that failed its check.

### `curl: (22) The requested URL returned error: 404`

There is no archive for that version and architecture. Archives exist for `x86_64` and `aarch64` only; check `uname -m`. On other platforms, build Maestro from source (Appendix A).

### `Permission denied` when connecting to `/run/maestro.sock`

The socket is created with mode `0660` (`rw-rw----`). Ensure your user account is a member of the group under which `maestro-local-server` runs (`users` by default in `maestro.service`), or connect using a user in that group.

---

## Appendix A: Building Maestro from Source

Build Maestro yourself when you need native features newer than the pinned release, when no archive exists for your platform, or when you want custom build options.

### Choosing a revision

- **To match the archive**, check out `v0.3.12`.
- **For the newest native features**, check out the commit that CI tests this server against. It is `MAESTRO_REF` in [`.github/workflows/build-and-test.yml`](../.github/workflows/build-and-test.yml).

Always build Maestro and `maestro-local-server` from compatible revisions.

### Requirements

Besides the packages in section 1, building Maestro needs CMake, Ninja, OpenBLAS and FFTW, and **Boost 1.89 or newer** with the `program_options`, `json`, `container` and `serialization` libraries. Older Boost releases (for example 1.83 on Ubuntu 24.04) fail to compile Maestro with `call of overloaded 'serialize(double&)' is ambiguous`.

**Ubuntu / Debian:**

```bash
sudo apt-get install -y cmake ninja-build libfftw3-dev libopenblas-dev
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
sudo dnf install -y cmake ninja-build fftw-devel boost-devel openblas-devel
```

Check that `rpm -q boost-devel` reports 1.89 or newer (Fedora 45 ships 1.90). If it is older, remove `boost-devel` from the command and build Boost from source as shown for Ubuntu above.

### Building

```bash
git clone https://github.com/QoroQuantum/maestro.git "$HOME/maestro"
cd "$HOME/maestro"
git checkout v0.3.12   # or the MAESTRO_REF commit

cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=/usr/local \
  -DBUILD_PYTHON_BINDINGS=OFF \
  -DCOMPILE_TESTS=OFF

cmake --build build -j2
```

`-DCOMPILE_TESTS=OFF` skips Maestro's own test binaries, which `maestro-local-server` does not need. This roughly halves the build time and memory use.

> **ARM (`aarch64`) hosts:** Qiskit Aer, which is enabled by default, uses x86-only intrinsics and fails to build on ARM with `immintrin.h: No such file or directory`. Add `-DENABLE_QISKIT_AER=OFF` to the `cmake -S . -B build ...` command. The other backends are unaffected.

> **Memory Tip:** Each Maestro compile job can use several gigabytes of RAM, more with Qiskit Aer enabled. With 8 GB of RAM, `-j3` runs out of memory, so the command above uses `-j2`. On machines with 32 GB or more, you can raise it (for example, `-j"$(nproc)"`). If the compiler is killed, rerun with `-j1`; the build continues where it stopped.

### Building the server against it

Point the server build at the checkout and its CMake build directory:

```bash
cd maestro-local-server
MAESTRO_DIR="$HOME/maestro" cargo build --release
```

`MAESTRO_DIR` selects `$MAESTRO_DIR/maestrolib/Interface.h` and `$MAESTRO_DIR/build/libmaestro.so`; the build directory is embedded in the binary's runtime search path, so no `ldconfig` setup is needed. (Installing with `sudo cmake --install build` is only needed if you want other software to find Maestro in `/usr/local`; it places `libmaestro.so` directly in `/usr/local`, not `/usr/local/lib`.)

### Troubleshooting the Maestro build

#### `call of overloaded 'serialize(double&)' is ambiguous`

Maestro was configured against a Boost release older than 1.89. Build Boost 1.90 from source into `/usr/local` as shown above, then delete Maestro's `build` directory and configure again so CMake picks up the new Boost.

#### `fatal error: immintrin.h: No such file or directory`

You are building on an ARM (`aarch64`) host with Qiskit Aer enabled. Configure Maestro with `-DENABLE_QISKIT_AER=OFF`.

#### `c++: fatal error: Killed signal terminated program cc1plus`

The compiler ran out of memory. Lower the parallel job count (for example, `cmake --build build -j2` or `-j1`) and run the build command again. It continues where it stopped.
