# Client Protocol and Usage Guide

`maestro-local-server` listens on a Unix domain socket (`/run/maestro.sock` in production, `/tmp/maestro.sock` in test mode, or the path set in `MAESTRO_SOCKET`) and accepts newline-terminated UTF-8 commands up to 16 MiB per request frame.

A client may send multiple commands over a single persistent socket connection or open a new connection per command.

The server supports two complementary command families on the same socket:

1. **Native JSON API v2 (`API <json>`)** — lossless, versioned JSON requests for capability discovery, runner health checks, native circuit execution/estimation/diagnostics/batches, session keepalives, and retained task logs.
2. **Session & Task Text Commands (`SESSION ...`, `PING`)** — line-based commands for creating/deleting sessions, polling task status, cancelling tasks, retrieving results, and submitting legacy tasks.

For deep architectural details on native options, tensor diagnostics, and MPI launch profiles, see [Native Maestro API v2](native-api-v2.md).

---

## 1. Quick-Start Walkthrough (Native JSON API v2)

A typical workflow using the Native JSON API v2 combines `API` commands with `SESSION` lifecycle and result polling commands:

1. **Create a session**:
   ```text
   -> SESSION CREATE
   <- OK 0
   ```
2. **Submit a native v2 simulation request** (`session_id: 0`):
   ```text
   -> API {"version":2,"command":"submit","session_id":0,"request":{"schema_version":2,"operation":"execute","circuit":{"num_qubits":2,"source":"OPENQASM 2.0; qreg q[2]; creg c[2]; h q[0]; cx q[0],q[1]; measure q->c;"},"execution":{"shots":100}}}
   <- OK {"version":2,"ok":true,"task_id":0}
   ```
3. **Poll until finished**:
   ```text
   -> SESSION 0 TASK 0 FINISHED
   <- OK YES
   ```
4. **Check whether the task failed**:
   ```text
   -> SESSION 0 TASK 0 FAILED
   <- OK NO
   ```
5. **Retrieve the JSON result** (consuming the result removes the task's pending/result record while preserving diagnostics):
   ```text
   -> SESSION 0 TASK 0 GET_RESULTS
   <- OK {"counts":{"00":52,"11":48},"ok":true,"operation":"execute","schema_version":2,...}
   ```
6. **(Optional) Inspect retained task logs, lifecycle events, and seeds**:
   ```text
   -> API {"version":2,"command":"logs","session_id":0,"task_id":0}
   <- OK {"session_id":0,"task_id":0,"logs":"","logs_truncated":false,"logs_omitted_bytes":0,"error":null,"failure_message":null,"context":{...},"events":[...],"native_build":{"version":"...","source_revision":"..."},"events_truncated":false,"seeds":[...],"seeds_truncated":false,"version":2,"ok":true}
   ```
7. **Delete the session when done**:
   ```text
   -> SESSION 0 DELETE
   <- OK
   ```

---

## 2. Command Reference

### Liveness

| Command | Response | Notes |
| --- | --- | --- |
| `PING` | `OK` | Simple transport liveness check. Does not require or renew a session. |

### Native JSON API v2 (`API <json>`)

Every API v2 command starts with `API ` followed by a single-line JSON object and a terminating newline (`\n`).
All transport responses have the form `OK <json>\n`; inspect the JSON `"ok"` boolean (`true` or `false`) to determine whether the command succeeded. On failure, the JSON response includes `"error": {"code": "<code_str>", "message": "<msg_str>"}`.

| Command (`"command"`) | Required Fields | Renews Lease? | Description |
| --- | --- | --- | --- |
| `"status"` | `"version": 2` | No | Returns runner health, whether jobs are accepted, busy state, and pending queue count. |
| `"capabilities"` | `"version": 2` | No | Returns the full native Maestro capability catalog (`native`), server commands, build version/revision, session lease duration, and configured MPI profiles (`server`). |
| `"submit"` | `"version": 2`, `"session_id"`, `"request"` | Yes | Atomically validates and enqueues a native Maestro request (schema version 2) in an isolated worker. Returns `{"version":2,"ok":true,"task_id":<id>}`. |
| `"keepalive"` | `"version": 2`, `"session_id"` | Yes | Resets the idle expiration timer for `"session_id"`. |
| `"logs"` | `"version": 2`, `"session_id"`, `"task_id"` | Yes | Returns bounded stdout/stderr (`logs`), lifecycle `events`, `context`, resolved `seeds`, structured `error` details, and the `native_build` (Maestro version and revision) that ran `"task_id"`. Remains available after `GET_RESULTS` or `CANCEL` until session deletion/expiration. |

### Session Management Commands

| Command | Response | Renews Lease? | Description |
| --- | --- | --- | --- |
| `SESSION CREATE` | `OK <session_id>` | N/A | Creates a new session and returns its numeric `u32` ID. |
| `SESSION <id> EXISTS` | `OK YES` or `OK NO` | No | Checks whether session `<id>` currently exists. |
| `SESSION <id> DELETE` | `OK` or `ERROR ...` | N/A | Cancels any queued or running tasks in session `<id>`, clears retained diagnostics, and deletes the session. |

### Task Polling, Result, and Cancellation Commands

These commands work for tasks created via either `API {"version":2,"command":"submit",...}` or legacy `SESSION <id> TASK CREATE`:

| Command | Response | Description |
| --- | --- | --- |
| `SESSION <id> TASK <task_id> EXISTS` | `OK YES` or `OK NO` | `YES` while the task is initiated, queued, running, or waiting for its result to be consumed. |
| `SESSION <id> TASK <task_id> PENDING` | `OK YES` or `OK NO` | `YES` while the task has been submitted to the runner and its result has not yet been consumed. |
| `SESSION <id> TASK <task_id> RUNNING` | `OK YES` or `OK NO` | `YES` while the task is actively executing in a worker process. |
| `SESSION <id> TASK <task_id> FINISHED` | `OK YES` or `OK NO` | `YES` once execution has finished (either succeeded or failed) and a result is ready. |
| `SESSION <id> TASK <task_id> FAILED` | `OK YES` or `OK NO` | `YES` if the finished task failed (e.g., worker error or native execution failure). |
| `SESSION <id> TASK <task_id> GET_RESULTS` | `OK <json>` or `ERROR ...` | Returns the task's JSON result and removes the task from the session's active/result list. |
| `SESSION <id> TASK <task_id> CANCEL` | `OK YES`, `OK NO`, or `ERROR ...` | Cancels an unsubmitted, queued, or running task. If running, terminates and reaps the worker process group before returning `OK YES`; MPI cleanup can take 25 seconds, while other commands for the session are still answered. |

### Legacy Task Builder Commands

For backward compatibility with legacy clients, tasks can also be constructed field-by-field before calling `EXECUTE`:

| Command | Description |
| --- | --- |
| `SESSION <id> TASK CREATE` | Allocates a new unsubmitted task and returns `OK <task_id>`. |
| `SESSION <id> TASK <task_id> SET_TYPE <EXECUTE\|ESTIMATE>` | Sets the operation type (default: `EXECUTE`). |
| `SESSION <id> TASK <task_id> SET_QUBITS <n>` | Sets the number of qubits (`n > 0`). |
| `SESSION <id> TASK <task_id> SET_SIM_TYPE <int>` | Sets the legacy simulator backend ID. |
| `SESSION <id> TASK <task_id> SET_METHOD <int>` | Sets the legacy simulation method ID. |
| `SESSION <id> TASK <task_id> SET_QASM <qasm_str>` | Sets the single-line OpenQASM 2.0 circuit string. |
| `SESSION <id> TASK <task_id> SET_OBSERVABLES <obs>` | Sets semicolon-separated Pauli observables (required when type is `ESTIMATE`, e.g. `ZZ;XX`). |
| `SESSION <id> TASK <task_id> SET_OPTIONS <json>` | Sets legacy JSON simulator options (e.g. `{"shots":1024}`). |
| `SESSION <id> TASK <task_id> VALID` | Returns `OK YES` if required fields (`qasm`, `nr_qubits > 0`, and `observables` for `ESTIMATE`) are set. |
| `SESSION <id> TASK <task_id> EXECUTE` | Enqueues the valid task for execution in the runner and returns `OK`. |

---

## 3. Complete Python Client Example

Below is a self-contained Python 3 example using only the standard library (`socket`, `json`, `time`) to run a Bell-state circuit through `maestro-local-server`:

```python
import json
import socket
import time

SOCKET_PATH = "/run/maestro.sock"


class MaestroLocalClient:
    def __init__(self, socket_path: str = SOCKET_PATH):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(socket_path)
        self.reader = self.sock.makefile("r", encoding="utf-8")

    def send_line(self, command: str) -> str:
        self.sock.sendall((command + "\n").encode("utf-8"))
        return self.reader.readline().rstrip("\n")

    def api(self, payload: dict) -> dict:
        reply = self.send_line("API " + json.dumps(payload, separators=(",", ":")))
        if not reply.startswith("OK "):
            raise RuntimeError(f"Unexpected transport reply: {reply}")
        return json.loads(reply[3:])

    def close(self):
        self.reader.close()
        self.sock.close()


if __name__ == "__main__":
    client = MaestroLocalClient()
    try:
        # 1. Check runner status
        status = client.api({"version": 2, "command": "status"})
        print("Runner status:", status["status"])

        # 2. Create a session
        session_reply = client.send_line("SESSION CREATE")
        session_id = int(session_reply.removeprefix("OK ").strip())

        # 3. Submit a native v2 circuit execution request
        submit = client.api({
            "version": 2,
            "command": "submit",
            "session_id": session_id,
            "request": {
                "schema_version": 2,
                "operation": "execute",
                "circuit": {
                    "num_qubits": 2,
                    "source": (
                        "OPENQASM 2.0; qreg q[2]; creg c[2]; "
                        "h q[0]; cx q[0],q[1]; measure q->c;"
                    ),
                },
                "simulator": {
                    "backend": "qcsim",
                    "method": "statevector",
                },
                "execution": {"shots": 1024, "seed": 42},
            },
        })
        if not submit["ok"]:
            raise RuntimeError(f"Submission failed: {submit['error']}")
        task_id = submit["task_id"]

        # 4. Poll until finished
        while client.send_line(f"SESSION {session_id} TASK {task_id} FINISHED") != "OK YES":
            time.sleep(0.02)

        # 5. Fetch results
        failed = client.send_line(f"SESSION {session_id} TASK {task_id} FAILED")
        if failed == "OK YES":
            logs = client.api({
                "version": 2,
                "command": "logs",
                "session_id": session_id,
                "task_id": task_id,
            })
            raise RuntimeError(f"Task failed: {logs}")

        raw_result = client.send_line(f"SESSION {session_id} TASK {task_id} GET_RESULTS")
        result = json.loads(raw_result.removeprefix("OK "))
        print("Counts:", result["counts"])

        # 6. Clean up session
        client.send_line(f"SESSION {session_id} DELETE")
    finally:
        client.close()
```
