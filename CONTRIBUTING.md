# Contributing to maestro-local-server

Thank you for your interest in `maestro-local-server` and the [Maestro](https://github.com/QoroQuantum/maestro) ecosystem.

Like Maestro, `maestro-local-server` is distributed under a dual-licensing model (GPL-3.0 for open-source use, and a separate commercial license). To keep the copyright and licensing model simple while the project is evolving, we currently do not accept direct code pull requests from external contributors without prior arrangement.

However, we very much welcome bug reports, feature requests, deployment feedback, and discussions.

## How You Can Contribute

### 1. Bug Reports

If you encounter a bug, crash, socket protocol issue, or packaging problem:

- Open a GitHub issue with the prefix `[Bug]`.
- Include:
  - `maestro-local-server` version and Git revision (reported in `API {"version":2,"command":"capabilities"}` under `server.build`).
  - [Maestro](https://github.com/QoroQuantum/maestro) library version and build configuration (CPU-only, Qiskit Aer, GPU plugin, or MPI).
  - Operating system, distribution, and Rust toolchain version (`rustc --version`).
  - Exact steps or socket commands (`API ...` or `SESSION ...`) to reproduce the issue.
  - Expected behavior versus actual behavior.
  - Relevant daemon output (`journalctl -u maestro.service`) and task diagnostics (`API {"version":2,"command":"logs",...}`).

### 2. Feature Requests

If you have ideas for protocol improvements, operational diagnostics, packaging, or HPC/Slurm integration:

- Open a GitHub issue with the prefix `[Feature Request]`.
- Clearly describe:
  - The use case or deployment scenario (workstation, multi-user node, Slurm allocation, QRMI integration, etc.).
  - Any operational or security constraints.
  - Proposed request/response format if the change touches the socket API.

### 3. Operational and Performance Feedback

If you deploy `maestro-local-server` in HPC or hybrid quantum-classical environments, sharing observations on worker startup overhead, cancellation behavior, or MPI launch profiles in an issue or discussion is very helpful.

---

## Development Standards (For Maintainers and Authorized Contributors)

When developing changes in this repository:

1. **Build against a matching Maestro library**:
   ```bash
   export MAESTRO_DIR="/path/to/maestro"
   cargo build
   ```
2. **Format and lint**:
   All code must pass `rustfmt` and `clippy` with warnings denied:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   ```
3. **Run the test suite**:
   Ensure all unit, worker CLI, build-config, and native API integration tests pass:
   ```bash
   cargo test
   ```

### Releasing

Releases are published by the [Release workflow](.github/workflows/release.yml) when a version tag is pushed:

1. Set the new version in `Cargo.toml` (and `CITATION.cff`), and, if the release should use a newer Maestro, update `MAESTRO_VERSION` in `scripts/install.sh` and the pinned version in `docs/installation.md` and `README.md`. Merge to `main` and wait for CI to pass.
2. Tag the merged commit and push the tag. The tag must match the `Cargo.toml` version:
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```
3. The workflow builds `.deb` packages for x86_64 and aarch64, installs and runs each on a clean Debian 12 system, and publishes the GitHub release with the packages, their checksums, and install notes. A tag with a hyphen (for example `v0.2.0-rc1`) is published as a prerelease.

If the workflow fails, fix the problem, delete the tag (`git push origin :refs/tags/v0.1.0` and `git tag -d v0.1.0`), and tag again.

---

## About Code Snippets in Issues

By posting code snippets, configurations, or protocol examples in issues or discussions, you agree that:

- You have the right to share that material.
- Qoro Quantum may freely use, modify, adapt, and redistribute that material as part of `maestro-local-server` and Maestro under its existing licenses (including GPL-3.0 and commercial licenses), without additional obligations beyond what GPL-3.0 mandates.

## Contact

For questions about commercial licensing, HPC integrations, or collaborations, reach out to Qoro Quantum at <team@qoroquantum.de>.
