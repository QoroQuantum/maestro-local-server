//! Exercise the public socket API and production workers against current Maestro.
//! Every test owns its daemon and socket; no installed service is involved.
use json::{JsonValue, object};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);
static NEXT_SERVER: AtomicU64 = AtomicU64::new(0);

struct Server {
    child: Child,
    directory: PathBuf,
}

impl Server {
    fn start() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "maestro-native-{}-{}",
            std::process::id(),
            NEXT_SERVER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let log = File::create(directory.join("server.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_maestro-local-server"))
            .env("MAESTRO_SOCKET", directory.join("server.sock"))
            .env("MAESTRO_SESSION_LEASE_SECONDS", "60")
            .env_remove("MAESTRO_MPI_PROFILES")
            .env("OMP_NUM_THREADS", "2")
            .env("OPENBLAS_NUM_THREADS", "1")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut server = Self { child, directory };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if UnixStream::connect(server.directory.join("server.sock")).is_ok() {
                break;
            }
            assert!(
                server.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "Daemon did not start: {}",
                server.logs()
            );
            thread::sleep(Duration::from_millis(10));
        }
        server
    }

    fn logs(&self) -> String {
        fs::read_to_string(self.directory.join("server.log")).unwrap_or_default()
    }

    fn command(&self, command: &str) -> String {
        let mut socket = UnixStream::connect(self.directory.join("server.sock")).unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_write_timeout(Some(TIMEOUT)).unwrap();
        writeln!(socket, "{command}").unwrap();
        let mut response = String::new();
        BufReader::new(socket).read_line(&mut response).unwrap();
        assert!(
            response == "OK\n" || response.starts_with("OK "),
            "{command}: {response}"
        );
        response.strip_prefix("OK").unwrap().trim().to_owned()
    }

    fn api(&self, request: JsonValue) -> JsonValue {
        json::parse(&self.command(&format!("API {}", request.dump()))).unwrap()
    }

    fn session(&self) -> u32 {
        self.command("SESSION CREATE").parse().unwrap()
    }

    fn submit(&self, session: u32, request: JsonValue) -> JsonValue {
        self.api(object! {version: 2, command: "submit", session_id: session, request: request})
    }

    fn result(&self, session: u32, task: u32) -> JsonValue {
        let command = format!("SESSION {session} TASK {task}");
        let deadline = Instant::now() + TIMEOUT;
        while self.command(&format!("{command} FINISHED")) != "YES" {
            assert!(Instant::now() < deadline, "Task timed out: {}", self.logs());
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            self.command(&format!("{command} FAILED")),
            "NO",
            "{}",
            self.logs()
        );
        let result = json::parse(&self.command(&format!("{command} GET_RESULTS"))).unwrap();
        assert_eq!(result["ok"], true, "{result}");
        result
    }

    fn run(&self, request: JsonValue) -> JsonValue {
        let session = self.session();
        let submitted = self.submit(session, request);
        assert_eq!(submitted["ok"], true, "{submitted}");
        let task = submitted["task_id"].as_u32().unwrap();
        let result = self.result(session, task);
        // Seed history must survive consuming a new kind of native result too.
        let logs =
            self.api(object! {version: 2, command: "logs", session_id: session, task_id: task});
        assert_eq!(logs["ok"], true, "{logs}");
        if result.has_key("seed") {
            assert_eq!(logs["seeds"][0]["seed"], result["seed"]);
        } else {
            assert!(logs["seeds"].is_empty());
        }
        assert_eq!(self.command(&format!("SESSION {session} DELETE")), "");
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // Only signal the child this fixture owns; let it cancel/reap workers.
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("Daemon log: {}", self.logs());
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn request(operation: &str, method: &str, gates: &str) -> JsonValue {
    object! {
        schema_version: 2, operation: operation,
        circuit: {num_qubits: 2, source: format!("OPENQASM 2.0; qreg q[2]; {gates}")},
        simulator: {backend: "qcsim", method: method},
        execution: {seed: 123},
    }
}

fn near(value: &JsonValue, expected: f64) {
    let actual = value
        .as_f64()
        .unwrap_or_else(|| panic!("Expected number: {value}"));
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

fn complex(value: &JsonValue, real: f64, imaginary: f64) {
    assert_eq!(value.len(), 2, "{value}");
    near(&value[0], real);
    near(&value[1], imaginary);
}

#[test]
fn mps_operator_order_routing_and_maintenance_survive_the_socket() {
    let server = Server::start();
    let mut input = request("diagnostics", "matrix_product_state", "");
    input["diagnostics"] = json::array!["operator_expectation"];
    input["expectation_operators"] = json::array![
        object! {qubit: 0, matrix: [0, 1, 1, 0]},
        object! {qubit: 0, matrix: [0, [0, -1], [0, 1], 0]},
    ];
    input["move_qubits"] = json::array![1];
    input["maintenance"] = json::array!["trim", "recanonicalize"];
    // Repeated targets are ordered: YX = -iZ, rather than XY = iZ.
    complex(&server.run(input)["operator_expectation"], 0.0, -1.0);
}

#[test]
fn mpo_operators_preserve_scale_and_batch_order_in_results() {
    let server = Server::start();
    let mut input = request("diagnostics", "matrix_product_operator", "x q[1];");
    input["simulator"]["options"] = object! {max_bond_dimension: 0};
    input["operators"] =
        json::array![object! {qubits: [0], matrix: [2, 0, 0, 1], normalize: false}];
    input["move_qubits"] = json::array![1];
    input["diagnostics"] = json::array![
        "trace",
        "element",
        "density_matrix",
        "unnormalized_density_matrix",
        "expectations_complex",
        "unnormalized_expectations",
        "partial_trace"
    ];
    input["row_state"] = "01".into();
    input["col_state"] = "01".into();
    input["observables"] = json::array!["IZ", "ZI", "IZ"];
    input["keep_qubits"] = json::array![0];
    let result = server.run(input);
    complex(&result["trace"], 4.0, 0.0);
    complex(&result["element"], 4.0, 0.0);
    for (index, expected) in [-1.0, 1.0, -1.0].into_iter().enumerate() {
        complex(&result["expectations_complex"][index], expected, 0.0);
        complex(
            &result["unnormalized_expectations"][index],
            expected * 4.0,
            0.0,
        );
    }
    for (field, scale) in [
        ("density_matrix", 1.0),
        ("unnormalized_density_matrix", 4.0),
    ] {
        assert_eq!(result[field]["dimension"], 4);
        assert_eq!(result[field]["row_major"].len(), 16);
        for (index, element) in result[field]["row_major"].members().enumerate() {
            complex(element, if index == 10 { scale } else { 0.0 }, 0.0);
        }
    }
    assert_eq!(result["partial_trace"]["dimension"], 2);
    complex(&result["partial_trace"]["row_major"][0], 1.0, 0.0);
}

#[test]
fn batched_expectations_preserve_duplicates_and_evolution_steps() {
    let server = Server::start();
    for method in ["matrix_product_state", "matrix_product_operator"] {
        let mut input = request("estimate", method, "h q[0]; cx q[0],q[1];");
        input["observables"] = json::array!["ZZ", "XX", "ZI", "ZZ"];
        let result = server.run(input);
        assert_eq!(result["expectation_values"].len(), 4);
        for (value, expected) in result["expectation_values"]
            .members()
            .zip([1.0, 1.0, 0.0, 1.0])
        {
            near(value, expected);
        }
        let mut input = request("incremental_evolve", method, "");
        input["step_circuit"] = object! {num_qubits: 2, source: "OPENQASM 2.0; qreg q[2]; x q[0];"};
        input["steps"] = json::array![0, 1, 2];
        input["observables"] = json::array!["ZI", "IZ", "ZI"];
        let result = server.run(input);
        assert_eq!(result["steps"], json::array![0, 1, 2]);
        assert_eq!(result["expectation_values"].len(), 3);
        for (index, expected) in [1.0, -1.0, 1.0].into_iter().enumerate() {
            let step = &result["expectation_values"][index];
            near(&step[0], expected);
            near(&step[1], 1.0);
            near(&step[2], expected);
        }
    }
}

#[test]
fn bulk_queries_and_fusion_keep_logical_and_classical_bit_order() {
    let server = Server::start();
    for fusion in [false, true] {
        for operation in ["statevector", "probabilities"] {
            let mut input = request(operation, "matrix_product_state", "x q[0];");
            input["simulator"]["options"] =
                object! {gate_fusion: fusion, mps_sampling: "probabilities"};
            let result = server.run(input);
            assert_eq!(result["basis_states"], json::array![0, 1, 2, 3]);
            assert_eq!(
                result["execution_metadata"]["gate_fusion"]["requested"],
                fusion
            );
            assert_eq!(
                result["execution_metadata"]["gate_fusion"]["enabled"],
                fusion
            );
            for index in 0..4 {
                let expected = if index == 1 { 1.0 } else { 0.0 };
                if operation == "statevector" {
                    complex(&result["amplitudes"][index], expected, 0.0);
                } else {
                    near(&result["probabilities"][index], expected);
                }
            }
        }
        let mut input = request(
            "execute",
            "statevector",
            "creg c[3]; x q[0]; measure q[0]->c[2]; measure q[1]->c[0];",
        );
        input["circuit"]["num_clbits"] = 3.into();
        input["execution"]["shots"] = 7.into();
        input["simulator"]["options"] = object! {gate_fusion: fusion};
        let result = server.run(input);
        assert_eq!(result["counts"], object! {"001": 7});
        assert_eq!(
            result["execution_metadata"]["gate_fusion"]["enabled"],
            fusion
        );
        assert_eq!(
            result["execution_metadata"]["configured_options"]["gate_fusion"],
            fusion
        );
    }
}

#[test]
fn mpo_probability_and_counts_support_more_than_64_qubits() {
    let server = Server::start();
    let target = format!("1{}1", "0".repeat(63));
    let mut input = request("state_probability", "matrix_product_operator", "");
    input["circuit"] =
        object! {num_qubits: 65, source: "OPENQASM 2.0; qreg q[65]; x q[0]; x q[64];"};
    input["target_state"] = target.clone().into();
    near(&server.run(input.clone())["probability"], 1.0);
    input.remove("target_state");
    input["operation"] = "execute".into();
    input["execution"]["shots"] = 3.into();
    input["circuit"]["source"] =
        "OPENQASM 2.0; qreg q[65]; creg c[65]; x q[0]; x q[64]; measure q->c;".into();
    let result = server.run(input);
    assert_eq!(result["counts"].len(), 1);
    assert_eq!(result["counts"][&target], 3);
}

#[test]
fn capabilities_and_validation_follow_current_native_options() {
    let server = Server::start();
    let caps = server.api(object! {version: 2, command: "capabilities"});
    assert_eq!(caps["ok"], true);
    let options: Vec<_> = caps["native"]["options"]
        .members()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    for name in [
        "gate_fusion",
        "mps_sampling",
        "precision",
        "mps_svd_solver",
        "mpo_svd_solver",
        "tensor_network_svd_solver",
        "pp_workers",
        "pp_sampling_cache_nodes",
        "pp_max_pauli_weight",
        "pp_gates_between_trims",
        "pp_gates_between_deduplications",
    ] {
        assert!(options.contains(&name), "Missing option: {name}");
    }
    let session = server.session();
    for (name, value, method) in [
        ("use_double_precision", true.into(), "matrix_product_state"),
        (
            "mps_measure_no_collapse",
            true.into(),
            "matrix_product_state",
        ),
        (
            "mps_sample_measure_algorithm",
            "mps_probabilities".into(),
            "matrix_product_state",
        ),
        ("mps_use_gesvd", true.into(), "matrix_product_state"),
        ("pp_pauli_weight_threshold", 2.into(), "pauli_propagator"),
        ("pp_steps_between_trims", 2.into(), "pauli_propagator"),
        (
            "pp_steps_between_deduplications",
            2.into(),
            "pauli_propagator",
        ),
        ("mps_sampling", "invalid".into(), "matrix_product_state"),
    ] {
        let mut input = request("execute", method, "");
        input["simulator"]["options"][name] = value;
        let result = server.submit(session, input);
        assert_eq!(result["ok"], false, "{name}: {result}");
        assert_eq!(result["error"]["code"], "invalid_input", "{result}");
        assert!(!result.has_key("task_id"));
    }
    let mut input = request("diagnostics", "matrix_product_state", "");
    input["diagnostics"] = json::array!["operator_expectation"];
    input["expectation_operators"] = json::array![object! {qubit: 2, matrix: [1, 0, 0, 1]}];
    assert_eq!(
        server.submit(session, input)["error"]["code"],
        "invalid_input"
    );
    let mut input = request("diagnostics", "matrix_product_operator", "");
    input["diagnostics"] = json::array!["density_matrix"];
    input["max_output_elements"] = 15.into();
    assert_eq!(
        server.submit(session, input)["error"]["code"],
        "invalid_input"
    );
    // Invalid submissions must not consume a task ID or leave a partial task.
    assert_eq!(
        server.command(&format!("SESSION {session} TASK 0 EXISTS")),
        "NO"
    );
    let mut input = request("estimate", "pauli_propagator", "x q[0];");
    input["observables"] = json::array!["ZI"];
    input["simulator"]["options"] = object! {
        pp_workers: 1, pp_sampling_cache_nodes: 16, pp_max_pauli_weight: 2,
        pp_gates_between_trims: 10, pp_gates_between_deduplications: 10,
    };
    let submitted = server.submit(session, input);
    assert_eq!(submitted["ok"], true, "{submitted}");
    assert_eq!(submitted["task_id"], 0);
    near(&server.result(session, 0)["expectation_values"][0], -1.0);

    // Validation proves the new GPU option spelling without allocating a GPU
    // simulator or requiring a plugin/license on the CPU CI host.
    for (method, solver) in [
        ("matrix_product_state", "mps_svd_solver"),
        ("matrix_product_operator", "mpo_svd_solver"),
        ("tensor_network", "tensor_network_svd_solver"),
    ] {
        let mut input = request("validate", method, "");
        input["simulator"]["backend"] = "gpu".into();
        input["simulator"]["options"] = object! {precision: "double"};
        input["simulator"]["options"][solver] = "gesvd".into();
        assert_eq!(server.run(input)["valid"], true);
    }
}

#[test]
fn independent_tensor_requests_keep_noise_results_and_seed_paths() {
    let server = Server::start();
    let session = server.session();
    let mut noisy = request("estimate", "matrix_product_operator", "h q[0];");
    noisy["observables"] = json::array!["XI", "ZI", "XI"];
    noisy["noise"] =
        object! {evaluation: "exact", channels: [{kind: "t1", targets: [0], gamma: 0.36}]};
    let mut ideal = request("estimate", "matrix_product_state", "h q[0];");
    ideal["observables"] = noisy["observables"].clone();
    ideal["execution"]["seed"] = 456.into();
    let submitted = server.submit(
        session,
        object! {
            schema_version: 2, operation: "batch", requests: [noisy, ideal],
        },
    );
    assert_eq!(submitted["ok"], true, "{submitted}");
    let task = submitted["task_id"].as_u32().unwrap();
    let result = server.result(session, task);
    assert_eq!(result["results"].len(), 2);
    for (index, expected) in [[0.8, 0.36, 0.8], [1.0, 0.0, 1.0]].into_iter().enumerate() {
        assert_eq!(result["results"][index]["expectation_values"].len(), 3);
        for (value, expected) in result["results"][index]["expectation_values"]
            .members()
            .zip(expected)
        {
            near(value, expected);
        }
    }
    let logs =
        server.api(object! {version: 2, command: "logs", session_id: session, task_id: task});
    assert_eq!(logs["seeds"].len(), 2);
    for (index, seed) in [123, 456].into_iter().enumerate() {
        assert_eq!(logs["seeds"][index]["result_path"], json::array![index]);
        assert_eq!(logs["seeds"][index]["seed"], seed);
    }
}
