#!/bin/bash
# Check a release .deb on a clean system, following the release notes: install
# Maestro's prebuilt library to /opt/maestro, install the package with apt,
# start the daemon and run a Bell circuit through it.
#
# Runs inside a debian:12 container (the oldest Debian the package targets),
# with the repository mounted at the current directory and dist/ from
# build_release_deb.sh.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq ca-certificates curl python3 > /dev/null

maestro_version=$(cat dist/MAESTRO_VERSION)
arch=$(uname -m)
base_url="https://github.com/QoroQuantum/maestro/releases/download/v${maestro_version}"
echo "::group::Install Maestro ${maestro_version} (${arch})"
work=$(mktemp -d)
(
    cd "$work"
    curl -fsSLO "${base_url}/maestro-c-${maestro_version}-linux-${arch}.tar.gz"
    curl -fsSLO "${base_url}/maestro-c-${maestro_version}-SHA256SUMS"
    sha256sum --ignore-missing -c "maestro-c-${maestro_version}-SHA256SUMS"
)
mkdir -p /opt/maestro
tar -xzf "$work/maestro-c-${maestro_version}-linux-${arch}.tar.gz" -C /opt/maestro --strip-components=1
echo "::endgroup::"

echo "::group::Install the package"
# apt (not dpkg -i) resolves the package's dependencies, including systemd.
apt-get install -y -qq ./dist/maestro-local-server_*.deb > /dev/null
dpkg -s maestro-local-server | sed -n 's/^\(Version\|Depends\): /\1: /p'
echo "::endgroup::"

echo "::group::Run a Bell circuit through the installed server"
export MAESTRO_SOCKET=/tmp/maestro-release-check.sock
/usr/bin/maestro-local-server > /tmp/server.log 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT
python3 - <<'EOF'
import json, os, socket, sys, time

path = os.environ["MAESTRO_SOCKET"]
for _ in range(100):
    if os.path.exists(path):
        break
    time.sleep(0.1)
else:
    sys.exit(f"server did not create {path}")

sock = socket.socket(socket.AF_UNIX)
sock.connect(path)
stream = sock.makefile("rw")

def send(line):
    stream.write(line + "\n")
    stream.flush()
    return stream.readline().rstrip("\n")

def api(payload):
    reply = send("API " + json.dumps(payload))
    assert reply.startswith("OK "), reply
    return json.loads(reply[3:])

assert send("PING") == "OK"
capabilities = api({"version": 2, "command": "capabilities"})
assert capabilities["ok"], capabilities
print("server build:", capabilities["server"]["build"])
print("Maestro build:", capabilities["native"]["build"])

session = int(send("SESSION CREATE").split()[1])
bell = ('OPENQASM 2.0; include "qelib1.inc"; qreg q[2]; creg c[2]; '
        "h q[0]; cx q[0],q[1]; measure q -> c;")
submitted = api({"version": 2, "command": "submit", "session_id": session, "request": {
    "schema_version": 2, "operation": "execute",
    "circuit": {"num_qubits": 2, "source": bell},
    "execution": {"shots": 200, "seed": 11}}})
assert submitted["ok"], submitted
task = submitted["task_id"]
for _ in range(600):
    if send(f"SESSION {session} TASK {task} FINISHED") == "OK YES":
        break
    time.sleep(0.05)
else:
    sys.exit("task did not finish")
assert send(f"SESSION {session} TASK {task} FAILED") == "OK NO"
counts = json.loads(send(f"SESSION {session} TASK {task} GET_RESULTS")[3:])["counts"]
# Both outcomes must appear; a broken simulator that skipped the gates gives only "00".
assert set(counts) == {"00", "11"} and sum(counts.values()) == 200, counts
print("Bell counts:", counts)
EOF
echo "::endgroup::"
