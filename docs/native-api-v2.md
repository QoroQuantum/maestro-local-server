# Native Maestro API v2

The daemon and all workers are native Rust/C/C++. No Python interpreter is embedded
or spawned. Legacy `SESSION`/`TASK` commands and payloads remain supported.
Rebuild Maestro first, then this server against its updated `Interface.h` and
library. `MAESTRO_HEADER` and `MAESTRO_LIBRARY_DIR` select nondefault build inputs;
ensure the matching library is found at runtime. Default build paths retain the
previous installed-library behavior.

## Protocol

One compact JSON document follows `API ` and ends with a newline. JSON escaping
preserves multiline QASM, comments, parameters, and noise/distribution configuration.
Use API version 2 and native schema version 2 independently:

```json
{"version":2,"command":"capabilities"}
{"version":2,"command":"submit","session_id":0,"request":{"schema_version":2,"operation":"execute","circuit":{"num_qubits":1,"source":"OPENQASM 2.0; qreg q[1]; creg c[1]; x q[0]; measure q->c;"}}}
{"version":2,"command":"logs","session_id":0,"task_id":0}
{"version":2,"command":"keepalive","session_id":0}
```

Each wire request needs the `API ` prefix. Responses are `OK <JSON>\n` where the
JSON `ok` field indicates success. Errors carry `error:{code,message}`; the `OK`
transport prefix does not mean the operation succeeded. Submit validates the
entire native contract and launch configuration before allocating a task ID and
enqueuing it under one session lock. Native circuit/configuration validation runs
in an isolated child before submission; parser failures cannot abort the daemon.
Invalid submissions leave no partial task.
Use existing session creation, status, result, cancellation, and deletion commands.
A native failure produces the existing failed-task status and retained diagnostics.
Oversized `API ` frames use the same versioned error envelope with `invalid_input`;
legacy command framing is unchanged. Invalid native validation envelopes become
`native_failure`, including missing/nonstring error codes or messages.

Capabilities contain the native operation/backend/method/option/noise catalog,
server commands, lease duration, transport bound, and configured MPI profile names
and prerequisites. Compiled plugin support is distinct from runtime readiness.
Diagnostics contain bounded stdout/stderr, `failure_message`, and the structured
native error when available. Output capture retains a 64 KiB prefix. Diagnostics
remain available after successful result consumption until session deletion/expiry.
Clients should fetch logs before discarding the acquisition token.

When execution/protocol handling and cleanup both fail, diagnostics retain both.
The primary error has `code:"cleanup_unconfirmed"` and `cleanup_confirmed:false`;
its `causes` array retains earlier structured error codes/messages. Causes can
themselves have `causes`, including a supervisor cleanup error with the original
native failure underneath it. Clients should inspect the whole chain. Failed
cleanup still prevents successful execution or cancellation acknowledgement, even
after the child has been reaped. A transient signalling failure with a still-live
child can be retried. Native result frames are not treated as ordinary log lines.

Diagnostic messages retain at most 8192 UTF-8 bytes plus a truncation marker, codes
256 bytes plus that marker, four causes per node and three nested cause levels.
`causes_truncated:true` identifies omitted branches. These bounds keep error
composition within the existing private/public response size limit.

`MAESTRO_SOCKET` overrides the daemon socket path for isolated deployments/tests.
`MAESTRO_SESSION_LEASE_SECONDS` sets a positive lease duration (default 3600).
Legacy task commands and native submit/logs/`keepalive` refresh the lease, including
logs after result consumption. Session `EXISTS` and capability queries do not.
Clients of long-running jobs must continue actual status polling, logs or keepalive
comfortably within the lease interval; locally cached status does not send traffic.
No automatic client heartbeat is implied. Expiration cancels the worker. Requests
are bounded to 16 MiB, replies/private worker frames to 64 MiB.

Ordinary execution and validation workers are fresh copies of the Rust daemon
executable launched with `--worker`; they call the native Maestro C ABI. MPI
profiles launch the separate C++ `maestro-mpi-worker`. Capability catalog queries
remain in the daemon. JSON is bounded and serialized across process boundaries;
this protocol does not provide zero-copy transport.

## Native MPI profiles

Set `MAESTRO_MPI_PROFILES` to an administrator-owned JSON file:

```json
{
  "version":1,
  "profiles":{
    "local":{
      "kind":"mpirun",
      "launcher":"/usr/bin/mpirun",
      "worker":"/opt/maestro/bin/maestro-mpi-worker",
      "args":["--oversubscribe"],
      "environment":{},
      "max_ranks":8
    },
    "allocation":{
      "kind":"srun",
      "launcher":"/usr/bin/srun",
      "worker":"/opt/maestro/bin/maestro-mpi-worker",
      "scancel":"/usr/bin/scancel",
      "args":["--mpi=pmix"],
      "max_ranks":8
    }
  }
}
```

The native request selects `"launch":{"profile":"allocation","ranks":4}` at
the outer task level. Rank counts are powers of two within the profile limit.
Clients cannot supply launcher paths, shell commands, environments, arbitrary MPI
arguments, or nested launch profiles. Arguments are passed directly without a shell.
Paths and rank-zero stdin routing are controlled by the server. Local distribution
needs no launch profile and runs inside an ordinary isolated worker.
The server strips the validated outer `launch` before passing the computational
document to either native validation or the MPI executable. The standalone native
API/CLI reject this server-only field. Nested launch profiles remain invalid.

Malformed launch/rank input is `invalid_input`; unknown profiles or a missing
required MPI profile are `unsupported_capability`. Unreadable/malformed profile
files and missing/nonexecutable launcher, worker or required `scancel` paths are
`invalid_config`. An `srun` profile is checked for a cancellation executable before
task allocation; capabilities expose `cancellation_available`. These checks do
not prove that the scheduler, GPU allocation or remote runtime is usable.

MPI GPU requests require a launch profile even inside a batch. A CPU request can
use a profile for orchestration testing; its result is replicated, not distributed
simulation. Slurm profiles run in an existing suitable allocation and require
site-specific MPI/GPU binding and compatible CUDA-aware MPI. The server does not
create allocations. Allowing a profile does not prove device availability/licensing.

On cancellation, the native supervisor asks the launcher to terminate its ranks
and waits for exit. For Slurm, a validated job.step marker from rank zero also
allows explicit `scancel`, including a marker received while cancellation is
already in progress. A failed `scancel` or supervisor/cleanup timeout is an error,
not a success
acknowledgement. Process-group killing is the last local fallback; remote cleanup
cannot be promised after a broken launcher or unreachable Slurm controller.
The launcher's cleanup budget is 20 seconds; the parent allows five additional
seconds for the supervisor to report/reap. These nested deadlines deliberately
differ. An early stdin closure gets a bounded opportunity to finish a structured
failure frame. Such a failure cannot override cancellation, malformed framing or
unconfirmed cleanup. Ordinary trailing diagnostic output need not end in a
newline; result and Slurm step frames must be complete newline-terminated lines.

The private worker protocol sends a readiness frame only after cancellation
handlers are installed. The parent sends no task before that frame; cancellation
before readiness therefore cannot leave MPI ranks. After readiness the parent
waits for supervisor cleanup confirmation. A private exit code distinguishes
confirmed cleanup followed by a lost response socket from unconfirmed cleanup.
Arbitrary nonzero exits are not accepted as successful MPI cancellation.

## Verification

`cargo test` includes protocol, native numerical requests, worker isolation,
framing/log limits, diagnostics lifetime, leases, atomic validation, profile
validation, native error propagation, and cancellation while launcher stdin is
blocked. Deterministic cancellation tests cover startup, partial upload, in-flight
launchers, completed launchers, post-cleanup response loss and failed Slurm cleanup.
Boundary tests exercise oversized versioned and legacy frames. Mock launchers do
not require MPI hardware. QRMI's opt-in native
integration suite starts a private instance of this daemon and exercises the full
client/adapter path. Maestro CTest additionally covers two-rank native MPI
success/failure and optional local and MPI GPU distribution. The server CI build
enables Maestro's native request contract suite. No system service needs replacing
to run those tests.
