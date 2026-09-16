include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use std::ffi::CString;
extern crate json;

#[derive(Debug)]
pub struct SimpleSimulator {
    handle: ::std::os::raw::c_ulong,
}

impl SimpleSimulator {
    pub fn new(nr_qubits: i32) -> Self {
        unsafe {
            let obj = GetMaestroObjectWithMute();

            if obj.is_null() {
                eprintln!("Failed to get Maestro object.");
            }

            let handle = CreateSimpleSimulator(nr_qubits);

            if handle == 0 {
                eprintln!("Failed to create SimpleSimulator.");
            }

            Self { handle }
        }
    }

    pub fn get_maestro_object() -> *mut std::os::raw::c_void {
        unsafe { GetMaestroObjectWithMute() }
    }

    pub fn is_valid(&mut self) -> bool {
        self.handle != 0
    }

    pub fn destroy(&mut self) {
        if self.handle != 0 {
            unsafe {
                DestroySimpleSimulator(self.handle);
            }

            self.handle = 0;
        }
    }

    pub fn remove_all_optimization_simulators_and_add(
        &mut self,
        sim_type: i32,
        sim_exec_type: i32,
    ) -> bool {
        if self.handle == 0 {
            return false;
        }

        unsafe { RemoveAllOptimizationSimulatorsAndAdd(self.handle, sim_type, sim_exec_type) == 1 }
    }

    pub fn add_optimization_simulator(&mut self, sim_type: i32, sim_exec_type: i32) -> bool {
        if self.handle == 0 {
            return false;
        }

        unsafe { AddOptimizationSimulator(self.handle, sim_type, sim_exec_type) == 1 }
    }

    pub fn execute(&mut self, json_circuit: String, json_config: String) -> String {
        if self.handle == 0 {
            return "".to_string();
        }

        let c_json_circuit_string = match CString::new(json_circuit) {
            Ok(cstr) => cstr,
            Err(_) => {
                eprintln!("String contains interior null bytes, cannot convert json circuit.");
                return "".to_string();
            }
        };

        let c_json_config_string = match CString::new(json_config) {
            Ok(cstr) => cstr,
            Err(_) => {
                eprintln!("String contains interior null bytes, cannot convert json config.");
                return "".to_string();
            }
        };

        unsafe {
            let resp = SimpleExecute(
                self.handle,
                c_json_circuit_string.as_ptr(),
                c_json_config_string.as_ptr(),
            );
            if resp.is_null() {
                eprintln!("NULL string returned as execution result.");
                return "".to_string();
            }

            let c_str = std::ffi::CStr::from_ptr(resp);
            let str_res = c_str.to_string_lossy().to_string();

            FreeResult(resp);

            str_res
        }
    }

    pub fn estimate(
        &mut self,
        json_circuit: String,
        observable_str: String,
        json_config: String,
    ) -> String {
        if self.handle == 0 {
            return "".to_string();
        }

        let c_json_circuit_string = match CString::new(json_circuit) {
            Ok(cstr) => cstr,
            Err(_) => {
                eprintln!("String contains interior null bytes, cannot convert json circuit.");
                return "".to_string();
            }
        };

        let c_observable_string = match CString::new(observable_str) {
            Ok(cstr) => cstr,
            Err(_) => {
                eprintln!("String contains interior null bytes, cannot convert observable string.");
                return "".to_string();
            }
        };

        let c_json_config_string = match CString::new(json_config) {
            Ok(cstr) => cstr,
            Err(_) => {
                eprintln!("String contains interior null bytes, cannot convert json config.");
                return "".to_string();
            }
        };

        unsafe {
            let resp = SimpleEstimate(
                self.handle,
                c_json_circuit_string.as_ptr(),
                c_observable_string.as_ptr(),
                c_json_config_string.as_ptr(),
            );
            if resp.is_null() {
                eprintln!("NULL string returned as estimation result.");
                return "".to_string();
            }

            let c_str = std::ffi::CStr::from_ptr(resp);
            let str_res = c_str.to_string_lossy().to_string();

            FreeResult(resp);

            str_res
        }
    }
}

// TODO: implement it if needed, for now the simple one should do
/*
#[derive(Debug)]
pub struct Simulator {
handle: ::std::os::raw::c_ulong,
simulatorPtr: ::std::os::raw::c_ulong,
}
*/

#[cfg(test)]
mod tests_simulator {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn simple_execute() {
        let mut simulator = SimpleSimulator::new(5);
        assert!(
            simulator.is_valid(),
            "Simulator is not valid, handle is zero"
        );

        let res = simulator.remove_all_optimization_simulators_and_add(test_backend_id("qcsim"), 1);
        assert!(
            res,
            "remove_all_optimization_simulators_and_add returned false"
        );

        let qasm = "OPENQASM 2.0; \
            creg c[3]; \
            qreg q[3]; \
            x q[0]; \
            cx q[0],q[1]; \
            measure q[0]->c[0]; \
            measure q[1]->c[1];"
            .to_string();

        let exec_res = simulator.execute(qasm, String::new());
        simulator.destroy();

        assert!(!exec_res.is_empty(), "Execute returned empty string");

        let res_str: &str = &exec_res;

        let parsed = json::parse(res_str).unwrap();

        assert!(
            parsed.has_key("counts"),
            "Execute result does not have 'counts' key"
        );
        assert!(
            parsed.has_key("time_taken"),
            "Execute result does not have 'time_taken' key"
        );
        assert!(
            parsed.has_key("simulator"),
            "Execute result does not have 'simulator' key"
        );
        assert!(
            parsed.has_key("method"),
            "Execute result does not have 'method' key"
        );

        assert!(
            parsed["counts"].has_key("11000"),
            "Execute result counts does not have '11000' key"
        );
        assert!(
            parsed["counts"]["11000"].as_i32().unwrap() == 1,
            "Execute result counts '11000' value is not 1"
        );
        assert!(
            parsed["simulator"] == "qcsim",
            "Execute result simulator is not 'qcsim'"
        );
        assert!(
            parsed["method"] == "matrix_product_state",
            "Execute result method is not 'matrix_product_state'"
        );
    }

    #[test]
    #[serial]
    fn simple_estimate() {
        let mut simulator = SimpleSimulator::new(5);
        assert!(
            simulator.is_valid(),
            "Simulator is not valid, handle is zero"
        );

        let res = simulator.remove_all_optimization_simulators_and_add(test_backend_id("qcsim"), 1);
        assert!(
            res,
            "remove_all_optimization_simulators_and_add returned false"
        );

        let qasm = "OPENQASM 2.0; \
            creg c[3]; \
            qreg q[3]; \
            x q[0]; \
            cx q[0],q[1];"
            .to_string();

        let exec_res = simulator.estimate(qasm, "zzz;yyy;xxx".to_string(), "".to_string());
        simulator.destroy();

        assert!(!exec_res.is_empty(), "Estimate returned empty string");

        let res_str: &str = &exec_res;

        let parsed = json::parse(res_str).unwrap();

        assert!(
            parsed.has_key("expectation_values"),
            "Estimate result does not have 'expectation_values' key"
        );
        assert!(
            parsed.has_key("time_taken"),
            "Estimate result does not have 'time_taken' key"
        );
        assert!(
            parsed.has_key("simulator"),
            "Estimate result does not have 'simulator' key"
        );
        assert!(
            parsed.has_key("method"),
            "Estimate result does not have 'method' key"
        );

        assert!(
            parsed["expectation_values"].is_array(),
            "Estimate result expectation_values is not an array"
        );

        // expectation_values is a list of floats, check if the result is meaningful!
        let values = &parsed["expectation_values"];

        let values_cnt = values.len();
        assert!(
            values_cnt == 3,
            "Estimate result expectation_values is not 3"
        );

        for (i, value) in values.members().enumerate() {
            assert!(
                value.is_number(),
                "Estimate result expectation_values contains non-number value"
            );

            if i == 0 {
                assert!(
                    value.as_f64().unwrap() >= 0.99999 && value.as_f64().unwrap() <= 1.00001,
                    "Estimate result expectation_values contains value out of range [0.99999, 1.00001], Value: {}",
                    value.as_f64().unwrap()
                );
            } else {
                assert!(
                    value.as_f64().unwrap() >= -0.00001 && value.as_f64().unwrap() <= 0.00001,
                    "Estimate result expectation_values contains value out of range [-0.00001, 0.00001], Value: {}",
                    value.as_f64().unwrap()
                );
            }
        }

        assert!(
            !values.is_empty(),
            "Estimate result expectation_values is empty"
        );

        assert!(
            parsed["simulator"] == "qcsim",
            "Estimate result simulator is not 'qcsim'"
        );
        assert!(
            parsed["method"] == "matrix_product_state",
            "Estimate result method is not 'matrix_product_state'"
        );
    }
}

// Native JSON buffers are always released by the library that allocated them.
fn native_json(call: impl FnOnce() -> *mut ::std::os::raw::c_char) -> String {
    let pointer = call();
    if pointer.is_null() {
        return json::object! { ok: false, error: { code: "native_failure", message: "Native API returned a null buffer" } }.dump();
    }
    // SAFETY: the native contract returns a NUL-terminated allocated buffer.
    let result = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    unsafe { FreeResult(pointer) };
    result
}

pub fn capabilities() -> String {
    native_json(|| unsafe { MaestroGetCapabilitiesJson() })
}

pub fn validate_request(request: &str) -> String {
    match CString::new(request) {
        Ok(request) => native_json(|| unsafe { MaestroValidateRequestJson(request.as_ptr()) }),
        Err(_) => json::object! { ok: false, error: { code: "invalid_input", message: "NUL in request" } }.dump(),
    }
}

pub fn run_request(request: &str) -> String {
    match CString::new(request) {
        Ok(request) => native_json(|| unsafe { MaestroRunRequestJson(request.as_ptr()) }),
        Err(_) => json::object! { ok: false, error: { code: "invalid_input", message: "NUL in request" } }.dump(),
    }
}

#[cfg(test)]
pub fn test_backend_id(name: &str) -> i32 {
    let capabilities = json::parse(&capabilities()).unwrap();
    capabilities["backends"].members().find(|backend| backend["name"].as_str() == Some(name))
        .unwrap()["legacy_id"].as_i32().unwrap()
}
