# Native Maestro API v2

The daemon and all workers are native Rust/C/C++. No Python interpreter is embedded
or spawned. Legacy `SESSION`/`TASK` commands and payloads remain supported.
Rebuild Maestro first, then this server against its updated `Interface.h` and
library. With no Maestro environment variables set, the header defaults to
`$HOME/maestro/maestrolib/Interface.h`. Library discovery uses the normal linker
search paths with `/usr/local/lib` and `/usr/local` added; neither directory is
mandatory. The installed library must also be discoverable by the runtime loader.

Setting `MAESTRO_DIR` selects `maestrolib/Interface.h` and `build/libmaestro.so`
inside that checkout. `MAESTRO_HEADER` and `MAESTRO_LIBRARY_DIR` override those
inputs independently, including without `MAESTRO_DIR`. An explicitly selected
library directory is checked for `libmaestro.so` and embedded in the binary's
runtime search path. Only defined Maestro environment variables are tracked for
Cargo rebuilds. After first setting a previously unset override, clean this
package before rebuilding; see the [build instructions](../README.md#local-development-setup).

Native requests default to fixed backend selection. With Maestro's fixed-backend
multi-shot reuse fix, this still evolves eligible circuits once per execution job
and samples repeatedly; it does not require `selection:"automatic"`. The fix lives
in `libmaestro.so`, not in this server's request forwarding. Update the library
actually loaded by the daemon/workers (and native CLI/MPI workers used by launch
profiles), then restart the daemon. Merely updating the Maestro source checkout
does not replace an older installed library. Reuse is confined to each injected
noise realization; measurements, sampled relaxation resets, and readout errors
still draw independent outcomes per shot. Exact density-matrix/MPO channels
retain their channel evolution before sampling.

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
native error when available. They remain available after result consumption and
successful cancellation, until session deletion/expiry or daemon restart. This
includes queued and unsubmitted tasks cancelled before a worker starts. Reading
retained logs also renews the session lease. Clients should fetch logs before
discarding the acquisition token.

The `logs` string combines stdout and stderr in capture order; ordering between
the two streams is not guaranteed. Capture keeps the first 32 KiB and most recent
32 KiB of raw output. Up to 64 KiB, output is unchanged. Once output is omitted,
the retained pieces are separated by `\n[... Maestro log output truncated ...]\n`,
`logs_truncated` is `true`, and `logs_omitted_bytes` counts discarded raw bytes.
Otherwise these fields are `false` and `0`. Output uses lossy UTF-8 decoding, so
invalid bytes or partial characters at a truncation boundary become replacement
characters; the text marker and decoding can make the returned string larger
than 64 KiB. Readers continue draining full pipes even after reaching the limit.
Successful cancellation waits for the child to exit and for capture to drain.

Additional diagnostic fields are additive within API v2:

- `session_id` and `task_id` identify the record.
- `events` is an ordered array of `{timestamp_unix_ms, state}` objects. Submitted
  tasks record `queued`, then `started` when dequeued for worker startup, and
  `completed`, `failed`, or `cancelled`. A queued cancellation has no `started`
  event; an unsubmitted cancellation has only `cancelled`. An unsuccessful
  cancellation records `cancellation_failed` and retains the cleanup error.
  These are server-observed lifecycle events, not native progress notifications.
  Timestamps are Unix UTC milliseconds; array order defines transition order
  even if the wall clock is adjusted. Repeated adjacent states are suppressed.
  History retains at most eight events, keeping the first and the latest seven;
  `events_truncated` indicates omitted events.
- `context` contains requested operation, simulator backend/method, and MPI
  launch profile/ranks when supplied. Legacy tasks use `simulator.type_id` and
  `simulator.method_id`. String identifiers are capped at 256 UTF-8 bytes plus
  a truncation marker. Circuits, arbitrary options, and launcher commands are
  excluded. Omitted automatic selections are not resolved here; effective native
  execution metadata remains in the task result.
- `seeds` retains resolved seeds from successful results, including after result
  consumption. Each entry has `seed` (the full unsigned 64-bit execution seed),
  optional `noise_seed`, and `result_path`. The path is empty for an ordinary
  request; for nested batches its indices select successive `results` members.
  Apply the saved values as `execution.seed` and `noise.seed` on the corresponding
  original request to replay it with the same backend and configuration.
  Checkpoint batches retain their shared parent seed. Validation and results
  without a reported seed add no entries. At most 256 entries and four batch
  levels are retained; `seeds_truncated` indicates unvisited results. This seed
  history has the same lifetime as the other diagnostics, not disk persistence.

`ok:true` means log retrieval succeeded, including for a failed or cancelled task.
Cancelled tasks still have no legacy result/status record; retained diagnostics
do not change `TASK EXISTS`, `GET_RESULTS`, or cancellation acknowledgement rules.

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

## Current native options and client migration

The server forwards the native request document (except outer `launch` metadata)
and returns the complete native result. The loaded Maestro library determines
which operations, options, and diagnostics are supported. Query
`API {"version":2,"command":"capabilities"}` after an upgrade; `native.options`
contains the current option names and applicability, and `native.backends`
contains method names and build-dependent legacy IDs. A schema version of 2 does
not identify a particular Maestro release or guarantee that an older option name
is still accepted. Diagnostic subqueries are not individually listed in the
capability catalog; native request validation checks their support.

Recent Maestro releases changed these keys under `request.simulator.options`:

| Previous JSON option | Current JSON option/value |
| --- | --- |
| `use_double_precision: true` / `false` | `precision: "double"` / `"single"` |
| `mps_measure_no_collapse: true` / `false` | `mps_sampling: "probabilities"` / `"apply_measure"` |
| `mps_sample_measure_algorithm: "mps_probabilities"` / `"mps_apply_measure"` | `mps_sampling: "probabilities"` / `"apply_measure"` |
| `mps_use_gesvd`, `mps_use_gesvdj`, `mps_use_gesvdp`, `mps_use_gesvdr` | `mps_svd_solver: "gesvd"`, `"gesvdj"`, `"gesvdp"`, or `"gesvdr"` |
| Corresponding `mpo_use_*` flags | `mpo_svd_solver` with the same solver names |
| Corresponding `tensor_network_use_*` flags | `tensor_network_svd_solver` with the same solver names |
| `pp_pauli_weight_threshold` | `pp_max_pauli_weight` |
| `pp_steps_between_trims` | `pp_gates_between_trims` |
| `pp_steps_between_deduplications` | `pp_gates_between_deduplications` |

For SVD migration, select the solver whose old flag was true; omit the new option
when all old flags were false. Do not send old and new keys together. Precision
applies to the supported Aer/GPU backends, and SVD selectors to the corresponding
GPU tensor method. QCSim computes in double precision. CPU Pauli propagation also
accepts `pp_workers` (0 through 1024) and `pp_sampling_cache_nodes` (nonnegative).
The default Pauli deduplication cadence is 10 operations.

The server deliberately uses Maestro's validation rules without translating old
keys. Removed JSON options produce `invalid_input` during submission, before
allocation of a task ID. Some native configuration aliases remain accepted;
use only aliases advertised by the loaded library. In particular, a key accepted
by low-level `ConfigureSimulator` is not necessarily accepted in the JSON request
contract. The legacy `SET_OPTIONS` payload is forwarded to the separate legacy
API and should not be treated as interchangeable with `simulator.options`.

## Tensor queries and gate fusion

These features use the existing `submit` command and production worker process;
no extra socket commands or persistent simulator handles are needed:

| Feature | Native request fields |
| --- | --- |
| Batched Pauli expectations | `operation: "estimate"`, `observables: [...]` |
| Batched expectations at successive evolution steps | `operation: "incremental_evolve"`, `step_circuit`, `steps`, `observables` |
| Complex normalized/raw MPO expectation batches | `operation: "diagnostics"`, `diagnostics: ["expectations_complex", "unnormalized_expectations"]`, `observables` |
| Normalized/raw MPO dense matrices | `diagnostics: ["density_matrix", "unnormalized_density_matrix"]` |
| Raw MPO matrix element | `diagnostics: ["element"]`, `row_state`, `col_state` |
| Apply operators to an MPO | `operators: [{qubits, matrix, normalize}]` in a diagnostics request |
| Read-only MPS operator expectation | `diagnostics: ["operator_expectation"]`, `expectation_operators` |
| MPS/MPO routing and maintenance | `move_qubits`, `maintenance: ["trim", "recanonicalize"]` in a diagnostics request |
| Full pure-state amplitudes/probabilities | `operation: "statevector"` or `"probabilities"`; omit `basis_states` for bulk output |

Batched expectations preserve observable order and duplicates. `estimate` returns
`expectation_values`; `incremental_evolve` returns `steps` and an
`expectation_values` array of arrays, one per requested step. Native batching
shares work within a prepared state, never between different noise realizations
or evolution steps. The native `batch` operation can also group independent
requests, validating all members before computation.

For example, this complete submit envelope evaluates ordered operators on an MPS.
Create a session first and substitute its ID. Serialize each example onto one line
and prefix it with `API ` when sending it over the socket:

```json
{
  "version": 2,
  "command": "submit",
  "session_id": 0,
  "request": {
    "schema_version": 2,
    "operation": "diagnostics",
    "circuit": {"num_qubits": 2, "source": "OPENQASM 2.0; qreg q[2];"},
    "simulator": {"backend": "qcsim", "method": "matrix_product_state"},
    "diagnostics": ["operator_expectation"],
    "expectation_operators": [
      {"qubit": 0, "matrix": [0, 1, 1, 0]},
      {"qubit": 0, "matrix": [0, [0, -1], [0, 1], 0]}
    ],
    "execution": {"seed": 123}
  }
}
```

Poll `SESSION <session_id> TASK <task_id> FINISHED` using the submitted task ID,
check `FAILED`, then retrieve `GET_RESULTS`. The result's `operator_expectation`
is `[0,-1]`, since the operators act in list order and `YX = -iZ`. This is a
read-only query. The separate MPO `operators` field changes the state by
`A rho A†`; `normalize` defaults to false. For example:

```json
{
  "version": 2,
  "command": "submit",
  "session_id": 0,
  "request": {
    "schema_version": 2,
    "operation": "diagnostics",
    "circuit": {"num_qubits": 2, "source": "OPENQASM 2.0; qreg q[2];"},
    "simulator": {
      "backend": "qcsim", "method": "matrix_product_operator",
      "options": {"max_bond_dimension": 0}
    },
    "operators": [{"qubits": [0], "matrix": [2, 0, 0, 1], "normalize": false}],
    "diagnostics": ["trace", "expectations_complex", "unnormalized_expectations"],
    "observables": ["ZI", "IZ", "ZI"],
    "execution": {"seed": 123}
  }
}
```

This returns trace `[4,0]`, three normalized expectations `[1,0]`, and three raw
expectations `[4,0]`. JSON matrices are flat row-major arrays with real numbers or
`[real,imaginary]` entries. Dense matrix results use `{dimension,row_major}`.
Pauli strings and bitstrings put qubit zero first; integer basis indices put it
in the least-significant bit. These conventions also hold after chain movement.
Routing can truncate a state under the configured bond cap/cutoff.

CPU and current GPU MPO support `state_probability.target_state` and execution
count strings wider than 64 qubits. `max_bond_dimension: 0` means unlimited for
MPO; it is not generally valid for other tensor methods. Full state and matrix
outputs still grow exponentially. `max_output_elements` bounds supported queries
(default 65536, maximum 1048576), independently of the 64 MiB serialized result
limit. Prefer selected basis states, elements, or expectations for large registers.

Set `simulator.options.gate_fusion` to true or false to override native defaults.
When omitted, fusion is enabled on eligible methods except small CPU statevectors
(below 11 qubits) and CPU density matrices (below 5 qubits). It is independent of
`optimize_circuit`; unsupported methods retain their existing behavior. Results
include `execution_metadata.gate_fusion` with `requested` (null if omitted),
`enabled`, and `max_qubits`, describing the backend actually used. This metadata,
including effective configured options, is returned unchanged. Save it with the
result if needed later: retained task logs keep seed history and request context,
not the full execution metadata.

## Upgrading native and GPU libraries

Build and install matching Maestro headers and `libmaestro.so`, then rebuild the
server and restart it. Include `InterfaceTypes.h` when installing the current
header; `Interface.h` now includes it. Use the Maestro-pinned QCSim revision when
building Maestro, or ensure an explicit `QCSIM_INCLUDE_DIR` points at a compatible
revision. QCSim is compiled into Maestro; updating its source alone does not
update the running server. Sampling, noise, routing, precision and kernel fixes
take effect through the rebuilt native library.

Upgrade the GPU plugin alongside Maestro when using tensor backends. Current
Maestro requires the expanded MPO API, including `MPOSampleBits` and
`MPOSampleHistogram`, and rejects older incompatible MPO plugins. Several newer
MPS queries, including arbitrary-operator expectations and explicit chain moves,
also need updated exports. Other queries retain scalar or dense-reader fallbacks
when optional exports are absent; an actual backend query failure is still an
error. Updating the plugin requires restarting processes that already loaded it.

Check the libraries visible to the installed service, including its loader paths,
license, device access, and the binaries/plugins on MPI nodes. A successful
`capabilities` response or native validation does not prove GPU runtime readiness.
Exercise a small request on each deployed backend after an upgrade and inspect
both the result and retained task logs. GPU hardware tests are opt-in; CPU mock
tests verify plugin dispatch and fallback behavior without proving CUDA execution.

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
enables Maestro's native request contract suite, `expectation_batches`,
`tensor_dispatch_current`, `tensor_dispatch_legacy`, `fusion_interface_wrappers`,
and `network_jobs`. The mock-plugin suites require no GPU.

`cargo test --test native_api` starts private production daemons and exercises the
new tensor queries, wide MPO registers, fusion metadata, current option names,
validation bounds, and result/seed retrieval over the public socket. Each daemon
owns a separate temporary socket. No system service needs replacing to run tests.

On a configured GPU host, a Maestro build with `MAESTRO_BUILD_REQUEST_TESTS=ON`
also provides these optional hardware checks:

```bash
ctest --test-dir "$MAESTRO_DIR/build" --output-on-failure \
  -R '^(gpu_tensor_queries_(single|double)|gpu_tensor_cpp_wrappers)$'
```

Missing hardware/plugin prerequisites can mark these tests skipped; review that
status separately from passing numerical tests. The distributed GPU suites have
additional MPI/CUDA-aware MPI prerequisites described in Maestro's documentation.
