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
server commands, lease duration, request and result bounds, and configured MPI profile names
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
`CANCEL` and session `DELETE` therefore reply only after cleanup, which for an
MPI task may take the launcher's 20-second budget plus five seconds; clients
should wait at least that long (QRMI allows 60 seconds). Meanwhile the server
answers other commands for the same session: the task is still listed and
`RUNNING` until its cleanup is confirmed, then it is removed and its
diagnostics end with `cancelled`.

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
  original request to replay it with the same backend, configuration and native
  library build (`native_build` below). Seeded samples are reproducible only
  within one Maestro build: for example, Maestro 0.3.8 changed qubit compaction
  and 0.3.9 disabled compiler multiply-add contraction, so earlier seeds can give
  different samples (with the same distributions) on a newer library.
  Checkpoint batches retain their shared parent seed. Validation and results
  without a reported seed add no entries; deterministic adjoint results have no
  seed. At most 256 entries and four batch levels are retained; `seeds_truncated`
  indicates unvisited results. This seed history has the same lifetime as the
  other diagnostics, not disk persistence.
- `native_build` is `{version, source_revision}` of the Maestro library loaded by
  the worker that ran the task, as reported by that worker, or `null` when
  unknown (queued or cancelled before a result, or an MPI task, whose ranks load
  the separate `maestro-mpi-worker` library). Workers are fresh processes, so
  after replacing `libmaestro.so` this can differ from the daemon's own
  `native.build` until the daemon restarts. Each string is capped at 256 bytes.

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
are bounded to 16 MiB (`server.max_request_bytes`). Task results are bounded by
`server.max_result_bytes`: Maestro's 64 MiB `max_result_bytes`, which covers a
whole native result document, plus 64 KiB of headroom for metadata an MPI worker
appends. A result reply adds only `OK ` and a newline, so clients should accept a
line of `server.max_result_bytes` plus that framing.

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
is still accepted. Newer native builds list diagnostic subqueries and their supported backend/method
pairs in `native.diagnostics`; validation remains authoritative.

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
GPU tensor method. QCSim computes in double precision. The default Pauli
deduplication cadence is 10 operations.

Newer Maestro releases add these options. Ranges and applicability come from the
loaded library's `native.options` entries (`enum`, `default`, `minimum`,
`maximum`, `exclusiveMinimum`, per-backend `constraints` and
`supported_configurations`); validation remains authoritative for cross-field
rules:

| Option(s) | Applies to | Values and constraints |
| --- | --- | --- |
| `pp_workers` | QCSim and GPU Pauli propagation | QCSim 0–1024; GPU 1–64 (default 1), advertised as a `gpu` constraint |
| `pp_sampling_cache_nodes` | QCSim and GPU Pauli propagation | Nonnegative (GPU default 8192; zero disables the cache) |
| `pp_memory_limit_bytes` | GPU Pauli propagation only | Positive budget (default 90% of free GPU memory); a limit, not a reservation |
| `pp_max_fused_gates` | GPU Pauli propagation only | 1–256 (default 64; one disables fusion) |
| `mps_routing_strategy` | QCSim MPS/MPO only | `legacy` (default) or opt-in `combined`; `current` is rejected |
| `mps_routing_virtual_bonds`, `mps_routing_truncation_weight`, `mps_routing_legacy_cap` | QCSim and GPU MPS/MPO | Boolean, nonnegative number, boolean; experimental virtual-bond routing |
| `auto_reduce`, `enable_causal_cone_reduction`, `enable_observable_clustering` | All | Booleans, default true; estimation reductions |
| `max_cluster_qubits`, `min_cluster_qubits` | All | Positive; `min_cluster_qubits` must not exceed `max_cluster_qubits` |
| `observable_error_tolerance` | All | In (0, 1]; opts into bounded noisy-channel evaluation |
| `allow_reset_marginalization`, `enable_sampling_reduction` | All | Booleans, default false; sampling reduction changes seeded random streams |
| `trajectory_policy` | QCSim statevector, density matrix, path integral | `block_v1` or `shot_v1` (default) |
| `trajectory_max_memory_mb` | QCSim statevector and density matrix | Positive MiB count |

Combined routing and virtual-bond routing trade runtime against truncation at
finite bond caps; neither is selected automatically. Options for another
backend or method are `unsupported_capability`; out-of-range values are
`invalid_input`. Both are returned during submission, before task allocation.
Validation accepts GPU options without a GPU, so validate first and then run a
small request on each deployed GPU backend. Estimates report reduction results
(`auto_reduced`, `qubits_before`, `qubits_after`, `clusters_count`,
`largest_cluster_qubits`, `cluster_methods`, `expectation_error_bounds`) next to
the expectation values. Maestro's
[native request reference](https://github.com/QoroQuantum/maestro/blob/main/docs/native-request-api.md)
describes each option in detail.

Behaviour changes visible through the server: Maestro 0.3.10 made
`coherent_depolarizing` isotropic (a rotation about a body diagonal whose Pauli
twirl is exactly depolarizing; it was a Z rotation), so results for that noise
kind differ from earlier libraries. QASM circuits accept qelib1.inc's `rzz`,
`rxx` and `u0` gates. Estimates remove terminal measurements before execution.

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
| Observable mirror fidelity (truncation error per observable) | `operation: "mirror_fidelity"`, `observables` |
| Adjoint values, Jacobians and vector–Jacobian products | `operation: "adjoint_expectation_values"`, `"adjoint_jacobian"` or `"adjoint_vjp"`; `observables`, optional `parameter_rows`, `cotangents` for VJP |
| Two-qubit rotations in instruction arrays | `{"name": "rzz" or "rxx", "qubits": [a, b], "params": [theta]}` |
| Per-gate and per-measurement noise controls | `"noiseless": true` on a gate; `"readout_error": p` or `{p_meas1_prep0, p_meas0_prep1}` on `measure` |
| Pauli channel instructions | `{"name": "pauli_channel", "qubits": [...], "probabilities": [...]}` |
| Mixed-state overlap `Tr(rho sigma)` | `operation: "density_matrix_overlap"`, `other_circuit` (density matrix or MPO) |
| Fidelity of a mixed state with a pure target | `operation: "fidelity_with_statevector"`, `target_statevector` (normalized `[real, imaginary]` amplitudes, at most 20 qubits) |

The adjoint operations, observable mirror fidelity, the mixed-state queries and
the instruction fields need a Maestro library that lists them in
`native.operations`, `native.adjoint`, `native.mixed_state_queries` and
`native.instructions`. Older libraries reject them during submission with
`unsupported_capability` or `invalid_input`, before task allocation. The server
needs no configuration for them: it forwards the request and returns the native
result unchanged.

Adjoint operations differentiate a unitary circuit on the QCSim CPU statevector
(`qcsim` or `composite_qcsim`). Each `observables` entry is a Pauli string, a
weighted sum `{"terms": [[coefficient, pauli], ...]}` or basis-state probabilities
`{"probabilities": [qubits]}`. Results report `num_parameters`, `num_values`,
the circuit's own angles as `circuit_parameters`, and `expectation_values[row]`,
`jacobian[row][value][parameter]` or `vjp[row][parameter]`. Output is bounded by
`max_output_elements`; a state exceeding physical or cgroup memory fails with
`insufficient_resources`. For example, this returns `d<Z>/dθ = -sin θ` for two
parameter rows:

```json
{"version":2,"command":"submit","session_id":0,"request":{"schema_version":2,
 "operation":"adjoint_jacobian",
 "circuit":{"num_qubits":1,"source":"OPENQASM 2.0; qreg q[1]; rx(0.3) q[0];"},
 "observables":["Z",{"probabilities":[0]}],"parameter_rows":[[0.3],[1.1]]}}
```

A `pauli_channel` is applied exactly on density-matrix and MPO methods. Pure-state
and MPS methods sample one Pauli per noise realization and require a `noise`
block with trajectory evaluation, whose `channels` may be omitted; set
`noise.realizations` to the shot count for independent draws. `readout_error`
applies only to `execute` and `checkpoint_batch` counts and composes with a
noise model's readout. `rxx`/`rzz` instructions expand exactly like the QASM
gates.

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

The private worker protocol (version 2) frames JSON control messages and sends a
result as a raw frame after its JSON header. Escaping a result inside JSON would
inflate quote-heavy results, such as counts, past the limit Maestro allows; a
worker whose result exceeds the server bound reports `invalid_input` instead.
It sends a readiness frame only after cancellation
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
Boundary tests exercise oversized versioned and legacy frames, and a quote-heavy
result at exactly the result bound. Mock launchers do
not require MPI hardware. QRMI's native integration suite starts a private
instance of this daemon and exercises the full client/adapter path. CI runs it on
CPU from the QRMI commit pinned as `QRMI_REF`, requiring rather than skipping the
native binaries; QRMI and Maestro are public, so no credentials are stored. Maestro CTest additionally covers two-rank native MPI
success/failure and optional local and MPI GPU distribution. The server CI build
enables Maestro's native request contract suite, `expectation_batches`,
`tensor_dispatch_current`, `tensor_dispatch_legacy`, `fusion_interface_wrappers`,
and `network_jobs`. The mock-plugin suites require no GPU.

`cargo test --test native_api` starts private production daemons and exercises the
new tensor queries, wide MPO registers, fusion metadata, current option names,
validation bounds, and result/seed retrieval over the public socket. It also
checks the routing, reduction, trajectory and GPU Pauli option metadata and
backend-specific bounds (validation only, no GPU), adjoint values/Jacobians/VJPs,
observable mirror fidelity (including density-matrix and MPO methods), `rxx`/`rzz`,
`noiseless`, `readout_error` and `pauli_channel` instructions, refusal of
`readout_error` where terminal measurements are removed (adjoint operations,
nested overlap/evolution circuits and checkpoint prefixes), mixed-state overlap
and statevector fidelity on density-matrix and MPO methods, and the
`native_build` diagnostics field. These
need a Maestro library that has the features; CI builds the Maestro commit
pinned as `MAESTRO_REF` in `.github/workflows/build-and-test.yml`. Update that
pin together with server changes that depend on newer native features.
Each daemon owns a separate temporary socket. No system service needs replacing
to run tests.

To test against an uninstalled Maestro checkout without touching the default
build, use a separate target directory and put that build first on the loader
path, since `LD_LIBRARY_PATH` takes precedence over the embedded runtime path:

```bash
MAESTRO_DIR=$HOME/maestro CARGO_TARGET_DIR=/tmp/maestro-server-target \
LD_LIBRARY_PATH=$HOME/maestro/build:$LD_LIBRARY_PATH cargo test
```

On a configured GPU host, a Maestro build with `MAESTRO_BUILD_REQUEST_TESTS=ON`
also provides these optional hardware checks:

```bash
ctest --test-dir "$MAESTRO_DIR/build" --output-on-failure \
  -R '^(gpu_tensor_queries_(single|double)|gpu_tensor_cpp_wrappers)$'
```

Missing hardware/plugin prerequisites can mark these tests skipped; review that
status separately from passing numerical tests. The distributed GPU suites have
additional MPI/CUDA-aware MPI prerequisites described in Maestro's documentation.


## Resource status and build discovery

`API {"version":2,"command":"status"}` returns an additive API v2 response:

```json
{"version":2,"ok":true,"status":{"accepting_jobs":true,"healthy":true,"busy":false,"pending_job_count":0,"status_reason":null,"health_scope":"runner"}}
```

No session is needed or renewed. The queue and active task are sampled under the
runner's lock order; pending jobs exclude the active job. Busy means queued or
active execution. Healthy means the runner thread is live and its shared state
is not poisoned; accepting_jobs additionally requires that shutdown has not
begun. Reasons include `runner_not_running`, `runner_state_poisoned` and
`shutting_down`. This describes runner availability, not GPU/plugin/licence
readiness, and does not advertise acquisition capacity. Clients may fall back to
PING only for an explicitly unsupported API/command, keeping other failures
visible. Session-specific polling/keepalive is still required for long leases.

Capabilities advertise `status` in `server.commands` and identify the server
build with `server.build.version` and `server.build.source_revision` (or `unknown`
without Git metadata). The native catalog is forwarded unchanged, including new
`build`, `diagnostics`, option `enum`/bounds and `supported_configurations` fields.
Native `capability_scope: "validation"` explicitly excludes runtime device
readiness. Optional plugin exports must match the deployed Maestro build.
