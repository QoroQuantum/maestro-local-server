# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in `maestro-local-server`, please report it privately to Qoro Quantum rather than opening a public GitHub issue:

- **Email**: <team@qoroquantum.de>
- **Subject line**: `[SECURITY] maestro-local-server: <brief summary>`

Please include:

- A description of the vulnerability and its potential impact.
- Affected versions or commit revisions.
- Step-by-step instructions or a minimal proof-of-concept to reproduce the issue.
- Any relevant configuration details (systemd unit settings, socket permissions, or `MAESTRO_MPI_PROFILES`).

We will acknowledge receipt of your report promptly, investigate the issue, and coordinate disclosure once a fix is available.

---

## Security Model and Deployment Considerations

`maestro-local-server` is designed as a **local Unix-domain socket daemon** for trusted local users or compute-node services (such as QRMI adapters). Keep the following architectural boundaries in mind when deploying the service:

1. **Unix Domain Socket Access Control**:
   - In production mode, the daemon creates `/run/maestro.sock` and sets file mode `0660` (`rw-rw----`).
   - Any local user or process with read/write access to the socket file can create sessions, query capabilities, and submit quantum simulation workloads, as well as query or cancel sessions by numeric ID.
   - The packaged `debian/maestro.service` runs with `Group=users` and `NoNewPrivileges=true`. On shared multi-tenant hosts, restrict socket access by configuring a dedicated Unix group (for example, `Group=maestro`) and placing only authorized local accounts in that group.

2. **Isolated Worker Processes**:
   - Circuit validation and task execution run in isolated child worker processes (`maestro-local-server --worker`) rather than inside the long-lived daemon process, preventing parser or simulator crashes from corrupting daemon state.
   - Request frames are bounded to 16 MiB and private worker/result frames to 64 MiB.
   - Idle sessions automatically expire according to `MAESTRO_SESSION_LEASE_SECONDS` (default: 3600 seconds), terminating any active worker process group.

3. **MPI Launch Profiles (`MAESTRO_MPI_PROFILES`)**:
   - The file referenced by `MAESTRO_MPI_PROFILES` **must be owned and writable only by the system administrator** (e.g., `root:root` with mode `0644`).
   - Clients can only select a named profile and a power-of-two rank count within `max_ranks`. Clients cannot pass arbitrary executable paths, shell commands, environment variables, or MPI launcher arguments over the socket.
