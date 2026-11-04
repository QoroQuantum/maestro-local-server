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

#[test]
fn status_and_build_information_are_available_without_acquisition() {
    let server = Server::start();
    let status = server.api(object! {version: 2, command: "status"});
    assert_eq!(status["ok"], true);
    assert_eq!(status["status"]["accepting_jobs"], true);
    assert_eq!(status["status"]["healthy"], true);
    assert_eq!(status["status"]["busy"], false);
    assert_eq!(status["status"]["pending_job_count"], 0);
    let caps = server.api(object! {version: 2, command: "capabilities"});
    assert!(caps["server"]["commands"].members().any(|v| v == "status"));
    assert!(caps["server"]["build"]["source_revision"].is_string());
    assert!(caps["native"]["build"]["source_revision"].is_string());
    assert!(
        caps["native"]["diagnostics"]
            .members()
            .any(|v| v["name"] == "operator_expectation")
    );
    // Every result within Maestro's own bound fits the server's transport.
    let native = caps["native"]["max_result_bytes"].as_u64().unwrap();
    let server = caps["server"]["max_result_bytes"].as_u64().unwrap();
    assert!(native > 0 && server >= native, "{native} > {server}");
}

fn option<'a>(caps: &'a JsonValue, name: &str) -> &'a JsonValue {
    caps["native"]["options"]
        .members()
        .find(|option| option["name"] == name)
        .unwrap_or_else(|| panic!("Missing option: {name}"))
}

fn supports(option: &JsonValue, backend: &str, method: &str) -> bool {
    option["supported_configurations"]
        .members()
        .any(|config| config["backend"] == backend && config["method"] == method)
}

#[test]
fn routing_reduction_trajectory_and_gpu_pauli_options_keep_their_contract() {
    let server = Server::start();
    let caps = server.api(object! {version: 2, command: "capabilities"});
    // Discovery metadata is forwarded unchanged from the loaded library.
    let strategy = option(&caps, "mps_routing_strategy");
    assert_eq!(strategy["enum"], json::array!["combined", "legacy"]);
    assert_eq!(strategy["default"], "legacy");
    assert!(supports(strategy, "qcsim", "matrix_product_operator"));
    assert!(!supports(strategy, "gpu", "matrix_product_state"));
    for name in [
        "mps_routing_virtual_bonds",
        "mps_routing_truncation_weight",
        "mps_routing_legacy_cap",
    ] {
        assert!(supports(option(&caps, name), "gpu", "matrix_product_state"));
    }
    let policy = option(&caps, "trajectory_policy");
    assert_eq!(policy["enum"], json::array!["block_v1", "shot_v1"]);
    assert_eq!(policy["default"], "shot_v1");
    let workers = option(&caps, "pp_workers");
    assert_eq!(workers["maximum"], 1024);
    assert_eq!(
        workers["constraints"],
        json::array![object! {backend: "gpu", minimum: 1, maximum: 64}]
    );
    assert!(supports(workers, "gpu", "pauli_propagator"));
    assert_eq!(option(&caps, "pp_max_fused_gates")["maximum"], 256);
    assert!(!supports(
        option(&caps, "pp_memory_limit_bytes"),
        "qcsim",
        "pauli_propagator"
    ));
    let tolerance = option(&caps, "observable_error_tolerance");
    assert_eq!(tolerance["exclusiveMinimum"], 0);
    assert_eq!(tolerance["maximum"], 1);
    for name in [
        "auto_reduce",
        "enable_causal_cone_reduction",
        "enable_observable_clustering",
        "max_cluster_qubits",
        "min_cluster_qubits",
        "allow_reset_marginalization",
        "enable_sampling_reduction",
        "trajectory_max_memory_mb",
    ] {
        option(&caps, name);
    }

    // Accepted forms validate without a GPU, plugin or licence.
    for (backend, method, options) in [
        (
            "qcsim",
            "matrix_product_state",
            object! {mps_routing_strategy: "combined", lookahead_depth: -1},
        ),
        (
            "qcsim",
            "matrix_product_operator",
            object! {mps_routing_virtual_bonds: true, mps_routing_truncation_weight: 0.5, mps_routing_legacy_cap: false},
        ),
        (
            "gpu",
            "pauli_propagator",
            object! {pp_workers: 64, pp_memory_limit_bytes: 1073741824, pp_max_fused_gates: 256, pp_sampling_cache_nodes: 0},
        ),
        (
            "qcsim",
            "pauli_propagator",
            object! {pp_workers: 0, pp_sampling_cache_nodes: 8192},
        ),
        (
            "qcsim",
            "density_matrix",
            object! {trajectory_policy: "block_v1", trajectory_max_memory_mb: 64},
        ),
        (
            "qcsim",
            "statevector",
            object! {auto_reduce: false, enable_causal_cone_reduction: true, enable_observable_clustering: true,
            min_cluster_qubits: 1, max_cluster_qubits: 4, observable_error_tolerance: 1e-3,
            allow_reset_marginalization: true, enable_sampling_reduction: true},
        ),
    ] {
        let mut input = request("validate", method, "");
        input["simulator"]["backend"] = backend.into();
        input["simulator"]["options"] = options;
        assert_eq!(server.run(input)["valid"], true);
    }

    // Rejections happen during submission, before a task ID is allocated.
    let session = server.session();
    for (backend, method, options, code) in [
        (
            "gpu",
            "pauli_propagator",
            object! {pp_workers: 0},
            "invalid_input",
        ),
        (
            "gpu",
            "pauli_propagator",
            object! {pp_workers: 65},
            "invalid_input",
        ),
        (
            "qcsim",
            "pauli_propagator",
            object! {pp_workers: 1025},
            "invalid_input",
        ),
        (
            "gpu",
            "pauli_propagator",
            object! {pp_max_fused_gates: 257},
            "invalid_input",
        ),
        (
            "gpu",
            "pauli_propagator",
            object! {pp_max_fused_gates: 0},
            "invalid_input",
        ),
        (
            "qcsim",
            "pauli_propagator",
            object! {pp_memory_limit_bytes: 1024},
            "unsupported_capability",
        ),
        (
            "gpu",
            "matrix_product_state",
            object! {mps_routing_strategy: "combined"},
            "unsupported_capability",
        ),
        (
            "qcsim",
            "matrix_product_state",
            object! {mps_routing_strategy: "current"},
            "invalid_input",
        ),
        (
            "qcsim",
            "statevector",
            object! {mps_routing_virtual_bonds: true},
            "unsupported_capability",
        ),
        (
            "qcsim",
            "statevector",
            object! {min_cluster_qubits: 5, max_cluster_qubits: 4},
            "invalid_input",
        ),
        (
            "qcsim",
            "statevector",
            object! {observable_error_tolerance: 0},
            "invalid_input",
        ),
        (
            "qcsim",
            "statevector",
            object! {trajectory_policy: "per_shot"},
            "invalid_input",
        ),
        (
            "qcsim",
            "matrix_product_state",
            object! {trajectory_policy: "shot_v1"},
            "unsupported_capability",
        ),
    ] {
        let mut input = request("execute", method, "");
        input["simulator"]["backend"] = backend.into();
        input["simulator"]["options"] = options.clone();
        let result = server.submit(session, input);
        assert_eq!(result["error"]["code"], code, "{options}: {result}");
        assert!(!result.has_key("task_id"));
    }
    assert_eq!(
        server.command(&format!("SESSION {session} TASK 0 EXISTS")),
        "NO"
    );

    // Clustering and reductions report their effect with the estimate.
    let mut input = request("estimate", "statevector", "h q[0]; x q[1];");
    input["observables"] = json::array!["XI", "IZ"];
    input["simulator"]["options"] = object! {enable_observable_clustering: true};
    let result = server.run(input);
    near(&result["expectation_values"][0], 1.0);
    near(&result["expectation_values"][1], -1.0);
    assert_eq!(result["clusters_count"], 2, "{result}");
    assert_eq!(result["largest_cluster_qubits"], 1);
}

#[test]
fn newer_native_features_reach_the_worker_through_submit() {
    let server = Server::start();
    let caps = server.api(object! {version: 2, command: "capabilities"});
    let operations = &caps["native"]["operations"];
    for operation in [
        "adjoint_expectation_values",
        "adjoint_jacobian",
        "adjoint_vjp",
    ] {
        assert!(operations.members().any(|v| v == operation), "{operation}");
    }
    assert_eq!(caps["native"]["adjoint"]["max_qubits"], 63);
    assert!(
        caps["native"]["instructions"]["gates"]
            .members()
            .any(|gate| gate["name"] == "rxx" && gate["params"] == 1)
    );

    // QASM and instruction-array two-qubit rotations agree.
    let mut qasm = request(
        "estimate",
        "statevector",
        "h q[0]; h q[1]; rzz(0.7) q[0],q[1];",
    );
    qasm["circuit"]["source"] =
        "OPENQASM 2.0; include \"qelib1.inc\"; qreg q[2]; h q[0]; h q[1]; rzz(0.7) q[0],q[1];"
            .into();
    qasm["observables"] = json::array!["XI", "YY"];
    let mut instructions = qasm.clone();
    instructions["circuit"] = object! {format: "instructions", num_qubits: 2, source: [
        {name: "h", qubits: [0]}, {name: "h", qubits: [1]}, {name: "rzz", qubits: [0, 1], params: [0.7]},
    ]};
    let left = server.run(qasm);
    let right = server.run(instructions);
    near(&left["expectation_values"][0], 0.7f64.cos());
    for index in 0..2 {
        near(
            &right["expectation_values"][index],
            left["expectation_values"][index].as_f64().unwrap(),
        );
    }

    // Instruction-level noise fields and Pauli channels.
    let noisy = |noiseless: bool| {
        let mut input = request("execute", "statevector", "");
        input["circuit"] = object! {format: "instructions", num_qubits: 1, source: [
            {name: "x", qubits: [0], noiseless: noiseless}, {name: "measure", qubits: [0]},
        ]};
        input["execution"]["shots"] = 16.into();
        input["noise"] =
            object! {realizations: 4, channels: [{kind: "bit_flip", targets: [0], probability: 1}]};
        input
    };
    assert_eq!(server.run(noisy(false))["counts"], object! {"0": 16});
    assert_eq!(server.run(noisy(true))["counts"], object! {"1": 16});
    let mut readout = request("execute", "statevector", "");
    readout["circuit"] = object! {format: "instructions", num_qubits: 1, source: [
        {name: "measure", qubits: [0], readout_error: {p_meas1_prep0: 1, p_meas0_prep1: 0}},
    ]};
    readout["execution"]["shots"] = 16.into();
    assert_eq!(server.run(readout)["counts"], object! {"1": 16});
    let mut channel = request("execute", "density_matrix", "");
    channel["circuit"] = object! {format: "instructions", num_qubits: 1, source: [
        {name: "pauli_channel", qubits: [0], probabilities: [0, 1, 0, 0]}, {name: "measure", qubits: [0]},
    ]};
    channel["execution"]["shots"] = 16.into();
    assert_eq!(server.run(channel.clone())["counts"], object! {"1": 16});
    // Pure states sample the channel, which needs a trajectory noise block.
    channel["simulator"]["method"] = "statevector".into();
    let session = server.session();
    let refused = server.submit(session, channel.clone());
    assert_eq!(
        refused["error"]["code"], "unsupported_capability",
        "{refused}"
    );
    channel["noise"] = object! {realizations: 16};
    assert_eq!(server.run(channel)["counts"], object! {"1": 16});

    // Observable mirror fidelity.
    let mut mirror = request(
        "mirror_fidelity",
        "matrix_product_state",
        "h q[0]; cx q[0],q[1];",
    );
    mirror["observables"] = json::array!["ZZ", "XX"];
    let result = server.run(mirror);
    near(&result["expectation_values"][0], 1.0);
    near(&result["expectation_values"][1], 0.0);

    // Adjoint differentiation: d<Z>/dθ of rx(θ)|0> is -sin θ.
    let mut adjoint = request("adjoint_jacobian", "statevector", "rx(0.3) q[0];");
    adjoint["observables"] = json::array!["ZI", object! {probabilities: [0]}];
    adjoint["parameter_rows"] = json::array![[0.3], [1.1]];
    let result = server.run(adjoint.clone());
    assert_eq!(result["num_parameters"], 1);
    assert_eq!(result["num_values"], 3);
    for (row, theta) in [0.3f64, 1.1].into_iter().enumerate() {
        near(&result["expectation_values"][row][0], theta.cos());
        near(&result["jacobian"][row][0][0], -theta.sin());
        // P(q0 = 1) = (1 - cos θ) / 2.
        near(&result["jacobian"][row][2][0], theta.sin() / 2.0);
    }
    adjoint["operation"] = "adjoint_vjp".into();
    adjoint["cotangents"] = json::array![[1, 0, 0], [0, 0, 2]];
    let result = server.run(adjoint.clone());
    near(&result["vjp"][0][0], -0.3f64.sin());
    near(&result["vjp"][1][0], 1.1f64.sin());
    // Shape errors are input errors before task allocation.
    adjoint["cotangents"] = json::array![[1, 0, 0]];
    let refused = server.submit(session, adjoint);
    assert_eq!(refused["error"]["code"], "invalid_input", "{refused}");
    assert_eq!(
        server.command(&format!("SESSION {session} TASK 0 EXISTS")),
        "NO"
    );

    // Diagnostics identify the native library build that ran the task.
    let submitted = server.submit(session, request("statevector", "statevector", "x q[0];"));
    let task = submitted["task_id"].as_u32().unwrap();
    server.result(session, task);
    let logs =
        server.api(object! {version: 2, command: "logs", session_id: session, task_id: task});
    assert_eq!(logs["native_build"], caps["native"]["build"], "{logs}");
}

#[test]
fn dropped_readout_is_refused_and_mixed_state_mirror_observables_run() {
    let server = Server::start();
    let session = server.session();

    // Operations which remove terminal measurements refuse their readout_error
    // instead of computing a noiseless answer.
    let annotated = object! {format: "instructions", num_qubits: 2, source: [
        {name: "ry", qubits: [0], params: [0.4]},
        {name: "measure", qubits: [0], readout_error: 1},
    ]};
    let mut refused = Vec::new();
    for operation in [
        "adjoint_expectation_values",
        "adjoint_jacobian",
        "adjoint_vjp",
    ] {
        let mut adjoint = request(operation, "statevector", "");
        adjoint["circuit"] = annotated.clone();
        adjoint["observables"] = json::array!["ZI"];
        if operation == "adjoint_vjp" {
            adjoint["cotangents"] = json::array![[1]];
        }
        refused.push(adjoint);
    }
    let mut overlap = request("inner_product", "statevector", "ry(0.4) q[0];");
    overlap["other_circuit"] = annotated.clone();
    refused.push(overlap);
    let mut evolve = request("incremental_evolve", "statevector", "ry(0.4) q[0];");
    evolve["step_circuit"] = annotated.clone();
    evolve["steps"] = json::array![1];
    evolve["observables"] = json::array!["ZI"];
    refused.push(evolve);
    // A checkpoint prefix's terminal measurements are removed too; only its
    // suffixes are sampled.
    let mut checkpoint = request("checkpoint_batch", "statevector", "");
    checkpoint["circuit"] = annotated.clone();
    checkpoint["suffixes"] = json::array![{num_qubits: 2, source: "OPENQASM 2.0; qreg q[2]; creg c[2]; measure q[0] -> c[0];"}];
    checkpoint["execution"]["shots"] = 8.into();
    refused.push(checkpoint);
    for document in refused {
        let response = server.submit(session, document.clone());
        assert_eq!(
            response["error"]["code"], "unsupported_capability",
            "{document}: {response}"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("readout_error"),
            "{response}"
        );
    }
    assert_eq!(
        server.command(&format!("SESSION {session} TASK 0 EXISTS")),
        "NO"
    );

    // Mirror expectations need no pure-state overlap, so mixed-state methods run
    // them. Plain mirror fidelity remains pure-state only.
    for method in ["density_matrix", "matrix_product_operator"] {
        let mut mirror = request("mirror_fidelity", method, "h q[0]; cx q[0],q[1];");
        mirror["observables"] = json::array!["ZI", "ZZ", "XX"];
        let result = server.run(mirror.clone());
        near(&result["expectation_values"][0], 1.0);
        near(&result["expectation_values"][1], 1.0);
        near(&result["expectation_values"][2], 0.0);
        mirror.remove("observables");
        let response = server.submit(session, mirror);
        assert_eq!(
            response["error"]["code"], "unsupported_capability",
            "{method}: {response}"
        );
    }
}

#[test]
fn mixed_state_overlap_and_statevector_fidelity_reach_the_worker() {
    let server = Server::start();
    let caps = server.api(object! {version: 2, command: "capabilities"});
    let queries = &caps["native"]["mixed_state_queries"];
    assert_eq!(
        queries["operations"],
        json::array!["density_matrix_overlap", "fidelity_with_statevector"],
        "{caps}"
    );
    assert_eq!(queries["max_target_statevector_qubits"], 20);

    let h = std::f64::consts::FRAC_1_SQRT_2;
    let bell = json::array![[h, 0], [0, 0], [0, 0], [h, 0]];
    for method in ["density_matrix", "matrix_product_operator"] {
        let mut fidelity = request("fidelity_with_statevector", method, "h q[0]; cx q[0],q[1];");
        fidelity["target_statevector"] = bell.clone();
        near(&server.run(fidelity.clone())["fidelity"], 1.0);
        // Exact amplitude damping on qubit 0 after both gates lowers it.
        fidelity["noise"] =
            object! {evaluation: "exact", channels: [{kind: "t1", targets: [0], gamma: 0.36}]};
        let noisy = server.run(fidelity)["fidelity"].as_f64().unwrap();
        assert!((noisy - 0.7624).abs() < 1e-9, "{method}: {noisy}");

        let mut overlap = request("density_matrix_overlap", method, "h q[0];");
        overlap["other_circuit"] =
            object! {num_qubits: 2, source: "OPENQASM 2.0; qreg q[2]; x q[0];"};
        complex(&server.run(overlap)["overlap"], 0.5, 0.0);
    }

    // Pure states have inner_product and the statevector queries instead, and
    // the target must be a normalized pure state.
    let session = server.session();
    let mut pure = request("fidelity_with_statevector", "statevector", "h q[0];");
    pure["target_statevector"] = bell;
    let response = server.submit(session, pure.clone());
    assert_eq!(
        response["error"]["code"], "unsupported_capability",
        "{response}"
    );
    pure["simulator"]["method"] = "density_matrix".into();
    pure["target_statevector"] = json::array![[1, 0], [1, 0], [0, 0], [0, 0]];
    let response = server.submit(session, pure);
    assert_eq!(response["error"]["code"], "invalid_input", "{response}");
    assert_eq!(
        server.command(&format!("SESSION {session} TASK 0 EXISTS")),
        "NO"
    );
}
