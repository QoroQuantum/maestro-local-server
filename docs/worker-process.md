# Per-task worker processes

Implementation plan:

1. Dispatch `--worker` before daemon startup.
2. Separate spawning, private IPC, and process cleanup into `maestro::worker`.
3. Replace the runner's direct execution call, keeping the old call commented
   beside it for comparison.
4. Use the same cancellation mechanism for explicit cancellation, session
   deletion/expiration, and daemon shutdown; suppress late results.
5. Test native execution/estimation and controlled worker failures and hangs.

## Code layout

- `src/main.rs`: selects daemon or worker mode before signal handlers or threads.
- `src/maestro/worker.rs`: spawns the current executable, owns its process group,
  handles cancellation/reaping, and implements the worker entry point.
- `src/maestro/worker/protocol.rs`: versioned task/result serialization and framing.
- `src/maestro/runner.rs`: keeps sessions' queued work and results in the daemon,
  registers active executions, and orders cancellation against result publication.
- `src/maestro/worker/tests.rs`: controlled subprocesses for failure and hang tests.
- `src/maestro/runner/process_tests.rs`: cancellation, session cleanup, and shutdown tests.
- `tests/worker_cli.rs`: tests the actual production `--worker` entry point.

`Task::execute()` still contains the native implementation. Only worker mode
calls it, through `worker::execute(&task, &execution)` in the runner.

## Process and protocol

Each task starts a fresh copy of the same executable with `--worker`. The child
initializes Maestro, runs exactly one task, returns its result, and exits. No raw
fork callback invokes Rust or Maestro. Tasks continue to execute one at a time.

A private Unix socket pair carries both the request and the response. Its child
end is passed as file descriptor 0 using `Command::stdin`; it is a full-duplex
socket, not a text stdin pipe. Stdout and stderr remain available for diagnostics
and cannot corrupt the result stream. Consequently, `--worker` is an internal
entry point, not a command accepting a JSON file redirected to ordinary stdin.

Messages consist of an eight-byte big-endian byte count followed by UTF-8 JSON.
Version 1 requests contain every `Task` field; replies contain the version,
session ID, task ID, and the native JSON result encoded as a string. The daemon
checks framing, version, IDs, result JSON, and the worker's exit status. A crash,
spawn error, or malformed response produces the existing `FAILED` state.

## Cancellation and ordering

The session's submission lock still covers the transfer into the runner queue.
The runner registers an `Execution` before releasing the queue lock, so a
cancellation cannot disappear between dequeue and child registration. Requests
arriving before spawn are latched. Requests racing with spawn wait for child
registration and then terminate it.

The child has its own process group. Cancellation shuts down the private socket,
sends SIGKILL to that group, and waits for the direct child to be reaped. Closing
the socket also interrupts a parent-side read or write. Normal exit is polled
without holding the process mutex during a blocking wait, keeping cancellation
available even after a result arrives if the worker hangs during cleanup.

Queue and result locks are released before termination. Result publication and
the cancellation flag are ordered by the active-task lock. After cancellation
wins, no buffered result can be published. `CANCEL` returns `OK YES` only after
the child is reaped and its output readers have drained (or before spawn when no
child can subsequently be created).
Termination errors are returned as `ERROR` instead of reporting success.

Session deletion and expiration close the session to further submissions,
including requests that still hold an `Arc` to it. Shutdown discards queued work,
cancels the active worker, and rejects further submissions.

The public socket protocol remains compatible with the existing QRMI client.
Cancelled tasks have no pending/running/result state. Existing result consumption
and completed-task removal semantics remain unchanged; this does not introduce
a persistent task-status API.

The runner stores diagnostics independently of execution/result state. Each
record shares the worker's bounded output buffer and records task lifecycle
events. Result consumption and cancellation preserve these records; session
cleanup deletes them. Late worker completion can update an existing diagnostic
record but cannot recreate one after session cleanup. Output retains a 32 KiB
prefix and 32 KiB tail, with explicit truncation metadata; see the
[public diagnostic contract](native-api-v2.md#protocol).

Process groups cover local descendants that stay in the group. A backend that
launches remote jobs or moves descendants to another group needs backend-specific
cleanup. SIGKILL does not run native destructors; the OS releases process-owned
memory and descriptors. Backend/device cleanup and startup overhead should be
evaluated for the simulators used in deployment.

## Tests

Unit tests re-execute the test binary through a dedicated worker test entry point
because libtest replaces `main`. This exercises the same process/IPC code and can
simulate a blocked reader, a hung computation, a hung exit, truncated responses,
large results, and worker failures without expensive simulations. The integration
test also runs the normal binary's production entry point for execute and estimate.

Run `cargo test`. As before, Unix socket tests need an environment that permits
local sockets, and native tests require the installed Maestro shared library.
