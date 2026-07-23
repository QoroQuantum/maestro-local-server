
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

//use libloading::{Library, Symbol};
//use std::sync::Arc;


use std::ffi::CString;
extern crate json;


//type GetMaestroObjectWithMuteFn = unsafe extern "C" fn() -> *mut std::os::raw::c_void;

#[derive(Debug)]
pub struct SimpleSimulator {
    handle : ::std::os::raw::c_ulong,
    //lib : Arc::<Library>,
    //get_maestro_object_with_mute_fn : Arc<Symbol<'a, GetMaestroObjectWithMuteFn>>,
}

impl SimpleSimulator {

pub fn new(nrQubits: i32) -> Self {
    unsafe {
        //let lib = Arc::new(Library::new("libmaestro.so").unwrap());
        //let get_maestro_object_with_mute_fn: Symbol<GetMaestroObjectWithMuteFn> = lib.get(b"GetMaestroObjectWithMute").unwrap();

	    //let obj = get_maestro_object_with_mute_fn();

        let obj = GetMaestroObjectWithMute();

        if obj == std::ptr::null_mut() {
            eprintln!("Failed to get Maestro object.");
        }

	    let handle = CreateSimpleSimulator(nrQubits);

        if handle == 0 {
            eprintln!("Failed to create SimpleSimulator.");
        }

        Self { handle }
    }
}

pub fn GetMaestroObject() -> *mut std::os::raw::c_void {
    unsafe {
        GetMaestroObjectWithMute()
    }
}


pub fn IsValid(&mut self) -> bool {
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

pub fn RemoveAllOptimizationSimulatorsAndAdd(&mut self, simType: i32, simExecType: i32) -> bool {
    if self.handle == 0 {
	    return false;
    } 

    unsafe {
	    RemoveAllOptimizationSimulatorsAndAdd(self.handle, simType, simExecType) == 1
    }
}

pub fn AddOptimizationSimulator(&mut self, simType: i32, simExecType: i32) -> bool {
    if self.handle == 0 {
	    return false;
    } 

    unsafe {
	    AddOptimizationSimulator(self.handle, simType, simExecType) == 1
    }
}

pub fn Execute(&mut self, jsonCircuit: String, jsonConfig: String) -> String {
    if self.handle == 0 {
	    return "".to_string();
    } 

    let c_jsonCircuit_string = match CString::new(jsonCircuit) {
        Ok(cstr) => cstr,
        Err(_) => {
            eprintln!("String contains interior null bytes, cannot convert json circuit.");
            return "".to_string();
        }
    };

    let c_jsonConfig_string = match CString::new(jsonConfig) {
        Ok(cstr) => cstr,
        Err(_) => {
            eprintln!("String contains interior null bytes, cannot convert json config.");
            return "".to_string();
        }
    };

    unsafe {
	    let resp = SimpleExecute(self.handle, c_jsonCircuit_string.as_ptr(), c_jsonConfig_string.as_ptr());
	    if resp.is_null() {
            eprintln!("NULL string returned as execution result.");
	        return "".to_string();
	    }
 
        let c_str = std::ffi::CStr::from_ptr(resp);
	    let strRes = c_str.to_string_lossy().to_string();

        FreeResult(resp);

	    strRes
    }
}

pub fn Estimate(&mut self, jsonCircuit: String, observableStr: String, jsonConfig: String) -> String {
    if self.handle == 0 {
	    return "".to_string();
    } 

    let c_jsonCircuit_string = match CString::new(jsonCircuit) {
        Ok(cstr) => cstr,
        Err(_) => {
            eprintln!("String contains interior null bytes, cannot convert json circuit.");
            return "".to_string();
        }
    };

    let c_observable_string = match CString::new(observableStr) {
        Ok(cstr) => cstr,
        Err(_) => {
            eprintln!("String contains interior null bytes, cannot convert observable string.");
            return "".to_string();
        }
    };


    let c_jsonConfig_string = match CString::new(jsonConfig) {
        Ok(cstr) => cstr,
        Err(_) => {
            eprintln!("String contains interior null bytes, cannot convert json config.");
            return "".to_string();
        }
    };

    unsafe {
	    let resp = SimpleEstimate(self.handle, c_jsonCircuit_string.as_ptr(), c_observable_string.as_ptr(), c_jsonConfig_string.as_ptr());
	    if resp.is_null() {
            eprintln!("NULL string returned as estimation result.");
	        return "".to_string();
	    }
    
        let c_str = std::ffi::CStr::from_ptr(resp);
	    let strRes = c_str.to_string_lossy().to_string();

        FreeResult(resp);

	    strRes
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

    #[test]
    #[serial]
    fn simple_execute() {
        //let obj = SimpleSimulator::GetMaestroObject();
	    let mut simulator = SimpleSimulator::new(5);
	    assert!(simulator.IsValid(), "Simulator is not valid, handle is zero");

	    let res = simulator.RemoveAllOptimizationSimulatorsAndAdd(1, 1);
        assert!(res, "RemoveAllOptimizationSimulatorsAndAdd returned false");

        let qasm = "OPENQASM 2.0; \
            creg c[3]; \
            qreg q[3]; \
            x q[0]; \
            cx q[0],q[1]; \
            measure q[0]->c[0]; \
            measure q[1]->c[1];".to_string();
        
        let execRes = simulator.Execute(qasm, String::new());
        simulator.destroy();

        assert!(!execRes.is_empty(), "Execute returned empty string");

        let res_str: &str = &execRes;

        let parsed = json::parse(res_str).unwrap();
        
        assert!(parsed.has_key("counts"), "Execute result does not have 'counts' key");
        assert!(parsed.has_key("time_taken"), "Execute result does not have 'time_taken' key");
        assert!(parsed.has_key("simulator"), "Execute result does not have 'simulator' key");
        assert!(parsed.has_key("method"), "Execute result does not have 'method' key");

        assert!(parsed["counts"].has_key("11000"), "Execute result counts does not have '11000' key");
        assert!(parsed["counts"]["11000"].as_i32().unwrap() == 1, "Execute result counts '11000' value is not 1");
        assert!(parsed["simulator"] == "qcsim", "Execute result simulator is not 'qcsim'");
        assert!(parsed["method"] == "matrix_product_state", "Execute result method is not 'matrix_product_state'");
    }

    #[test]
    #[serial]
    fn simple_estimate() {
        //let obj = SimpleSimulator::GetMaestroObject();
	    let mut simulator = SimpleSimulator::new(5);
	    assert!(simulator.IsValid(), "Simulator is not valid, handle is zero");

	    let res = simulator.RemoveAllOptimizationSimulatorsAndAdd(1, 1);
        assert!(res, "RemoveAllOptimizationSimulatorsAndAdd returned false");

        let qasm = "OPENQASM 2.0; \
            creg c[3]; \
            qreg q[3]; \
            x q[0]; \
            cx q[0],q[1];".to_string();
        
        let execRes = simulator.Estimate(qasm, "zzz".to_string(), "".to_string());
        simulator.destroy();

        assert!(!execRes.is_empty(), "Estimate returned empty string");

        let res_str: &str = &execRes;

        let parsed = json::parse(res_str).unwrap();
        
        assert!(parsed.has_key("expectation_values"), "Estimate result does not have 'expectation_values' key");
        assert!(parsed.has_key("time_taken"), "Estimate result does not have 'time_taken' key");
        assert!(parsed.has_key("simulator"), "Estimate result does not have 'simulator' key");
        assert!(parsed.has_key("method"), "Estimate result does not have 'method' key");

        // expectation_values is a list of floats, check if the result is meaningful!
        assert!(parsed["simulator"] == "qcsim", "Estimate result simulator is not 'qcsim'");
        assert!(parsed["method"] == "matrix_product_state", "Estimate result method is not 'matrix_product_state'");
    }
}
