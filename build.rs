extern crate bindgen;

mod build_config;

use build_config::BuildPaths;
use std::env;
use std::path::PathBuf;

fn main() {
    let paths =
        BuildPaths::resolve(|name| env::var_os(name)).unwrap_or_else(|error| panic!("{error}"));
    for variable in &paths.watched_env {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let maestro_header = paths.header;

    if !maestro_header.is_file() {
        panic!(
            "Maestro header not found at {}; set MAESTRO_DIR or MAESTRO_HEADER",
            maestro_header.display()
        );
    }

    let maestro_header = maestro_header
        .canonicalize()
        .expect("Cannot resolve Maestro header");
    println!("cargo:rerun-if-changed={}", maestro_header.display());

    if let Some(directory) = paths.library_dir {
        let maestro_lib_dir = directory.canonicalize().unwrap_or_else(|error| {
            panic!(
                "Invalid Maestro library directory {}: {error}",
                directory.display()
            )
        });
        let maestro_library = maestro_lib_dir.join("libmaestro.so");
        if !maestro_library.is_file() {
            panic!(
                "Maestro library not found at {}; set MAESTRO_DIR or MAESTRO_LIBRARY_DIR",
                maestro_library.display()
            );
        }
        println!("cargo:rerun-if-changed={}", maestro_library.display());
        println!(
            "cargo:rustc-link-search=native={}",
            maestro_lib_dir.display()
        );
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,{}",
            maestro_lib_dir.display()
        );
    } else {
        // Preserve normal linker discovery rather than requiring one specific
        // installed location. /usr/local was the original additional path.
        println!("cargo:rustc-link-search=native=/usr/local/lib");
        println!("cargo:rustc-link-search=native=/usr/local");
    }

    println!("cargo:rustc-link-lib=dylib=maestro");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // clang args, we use c++17, although we expose C api
        .clang_args(["-x", "c++", "-std=c++17"].iter())
        .clang_arg(format!("-I{}", maestro_header.parent().unwrap().display()))
        // The input header we would like to generate
        // bindings for.
        .header(if env::var_os("MAESTRO_HEADER").is_some() {
            maestro_header.to_string_lossy().into_owned()
        } else {
            "wrapper.h".into()
        })
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
