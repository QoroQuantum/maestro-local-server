#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use std::ffi::CString;


#[derive(Debug)]
pub struct SimpleSimulator {
handle : ::std::os::raw::c_ulong,
}

impl SimpleSimulator {

pub fn new(nrQubits: i32) -> Self {
    unsafe {
	GetMaestroObjectWithMute();
	let handle = CreateSimpleSimulator(nrQubits);

	println!("Handle is {}", handle);

	Self {
	    handle
	}
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

	FreeResult(resp);

	c_str.to_string_lossy().to_string()
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

	FreeResult(resp);

	c_str.to_string_lossy().to_string()
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
mod tests {
    use super::*;

    #[test]
    fn simple_execute() {
	let mut simulator = SimpleSimulator::new(5);
	assert!(simulator.IsValid(), "Simulator is not valid, handle is zero");

	let res = simulator.RemoveAllOptimizationSimulatorsAndAdd(1, 0);
        assert!(res == true, "RemoveAllOptimizationSimulatorsAndAdd returned false");
    }
}