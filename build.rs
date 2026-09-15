extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MAESTRO_DIR");

    let maestro_dir = required_directory("MAESTRO_DIR");
    let maestro_include_dir = maestro_dir.join("maestrolib");
    let maestro_lib_dir = maestro_dir.join("build");
    let maestro_header = maestro_include_dir.join("Interface.h");
    let maestro_library = maestro_lib_dir.join("libmaestro.so");

    if !maestro_header.is_file() {
        panic!(
            "MAESTRO_DIR does not contain maestrolib/Interface.h; expected {}",
            maestro_header.display()
        );
    }

    if !maestro_library.is_file() {
        panic!(
            "MAESTRO_DIR does not contain build/libmaestro.so; expected {}",
            maestro_library.display()
        );
    }

    println!("cargo:rerun-if-changed={}", maestro_header.display());
    println!("cargo:rerun-if-changed={}", maestro_library.display());
    println!(
        "cargo:rustc-link-search=native={}",
        maestro_lib_dir.display()
    );

    println!("cargo:rustc-link-lib=dylib=maestro");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        maestro_lib_dir.display()
    );

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // clang args, we use c++17, although we expose C api
        .clang_args(["-x", "c++", "-std=c++17"].iter())
        .clang_arg(format!("-I{}", maestro_include_dir.display()))
        // The input header we would like to generate
        // bindings for.
        .header("wrapper.h")
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        // Finish the builder and generate the bindings.
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

fn required_directory(variable: &str) -> PathBuf {
    let value = env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} must point to the Maestro checkout"));
    let path = PathBuf::from(value);

    path.canonicalize()
        .unwrap_or_else(|error| panic!("Invalid {variable} path {}: {error}", path.display()))
}
