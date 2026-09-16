extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MAESTRO_DIR");
    println!("cargo:rerun-if-env-changed=MAESTRO_LIBRARY_DIR");
    println!("cargo:rerun-if-env-changed=MAESTRO_HEADER");

    let maestro_dir = configured_path("MAESTRO_DIR");
    let header_override = configured_path("MAESTRO_HEADER");
    let maestro_header = header_override.clone().unwrap_or_else(|| {
        maestro_dir
            .as_ref()
            .expect("Set MAESTRO_DIR to the Maestro checkout or provide MAESTRO_HEADER")
            .join("maestrolib/Interface.h")
    });
    let maestro_lib_dir = configured_path("MAESTRO_LIBRARY_DIR").unwrap_or_else(|| {
        maestro_dir
            .as_ref()
            .expect("Set MAESTRO_DIR to the Maestro checkout or provide MAESTRO_LIBRARY_DIR")
            .join("build")
    });
    let maestro_library = maestro_lib_dir.join("libmaestro.so");

    if !maestro_header.is_file() {
        panic!(
            "Maestro header not found at {}; set MAESTRO_DIR or MAESTRO_HEADER",
            maestro_header.display()
        );
    }

    if !maestro_library.is_file() {
        panic!(
            "Maestro library not found at {}; set MAESTRO_DIR or MAESTRO_LIBRARY_DIR",
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
        .clang_arg(format!("-I{}", maestro_header.parent().unwrap().display()))
        // The input header we would like to generate
        // bindings for.
        .header(
            header_override
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "wrapper.h".into()),
        )
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

fn configured_path(variable: &str) -> Option<PathBuf> {
    let value = env::var_os(variable)?;
    let path = PathBuf::from(value);

    Some(
        path.canonicalize()
            .unwrap_or_else(|error| panic!("Invalid {variable} path {}: {error}", path.display())),
    )
}
