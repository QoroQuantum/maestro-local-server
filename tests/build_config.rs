//! Build path selection is independent of installed libraries and process-wide
//! environment changes, so all default/override combinations can be tested.

#[path = "../build_config.rs"]
mod build_config;

use build_config::BuildPaths;
use std::ffi::OsString;
use std::path::PathBuf;

fn resolve(variables: &[(&str, &str)]) -> Result<BuildPaths, String> {
    BuildPaths::resolve(|name| {
        variables
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    })
}

#[test]
fn defaults_use_home_header_and_normal_library_discovery() {
    let paths = resolve(&[("HOME", "/home/build user")]).unwrap();
    assert_eq!(
        paths.header,
        PathBuf::from("/home/build user/maestro/maestrolib/Interface.h")
    );
    assert_eq!(paths.library_dir, None);
}

#[test]
fn checkout_override_preserves_build_directory_without_home() {
    let paths = resolve(&[("MAESTRO_DIR", "/opt/maestro")]).unwrap();
    assert_eq!(
        paths.header,
        PathBuf::from("/opt/maestro/maestrolib/Interface.h")
    );
    assert_eq!(paths.library_dir, Some(PathBuf::from("/opt/maestro/build")));
}

#[test]
fn header_override_keeps_normal_library_discovery_without_home() {
    let paths = resolve(&[("MAESTRO_HEADER", "/opt/include/Interface.h")]).unwrap();
    assert_eq!(paths.header, PathBuf::from("/opt/include/Interface.h"));
    assert_eq!(paths.library_dir, None);
}

#[test]
fn library_override_keeps_default_home_header() {
    let paths = resolve(&[
        ("HOME", "/home/builder"),
        ("MAESTRO_LIBRARY_DIR", "/opt/lib"),
    ])
    .unwrap();
    assert_eq!(
        paths.header,
        PathBuf::from("/home/builder/maestro/maestrolib/Interface.h")
    );
    assert_eq!(paths.library_dir, Some(PathBuf::from("/opt/lib")));
}

#[test]
fn individual_overrides_take_precedence_over_checkout() {
    let header = resolve(&[
        ("MAESTRO_DIR", "/opt/maestro"),
        ("MAESTRO_HEADER", "/other/Interface.h"),
    ])
    .unwrap();
    assert_eq!(header.header, PathBuf::from("/other/Interface.h"));
    assert_eq!(
        header.library_dir,
        Some(PathBuf::from("/opt/maestro/build"))
    );

    let library = resolve(&[
        ("MAESTRO_DIR", "/opt/maestro"),
        ("MAESTRO_LIBRARY_DIR", "/other/lib"),
    ])
    .unwrap();
    assert_eq!(
        library.header,
        PathBuf::from("/opt/maestro/maestrolib/Interface.h")
    );
    assert_eq!(library.library_dir, Some(PathBuf::from("/other/lib")));
}

#[test]
fn explicit_pair_needs_neither_checkout_nor_home() {
    for checkout in [None, Some(("MAESTRO_DIR", "/unused/checkout"))] {
        let mut variables = vec![
            ("MAESTRO_HEADER", "/opt/include/Interface.h"),
            ("MAESTRO_LIBRARY_DIR", "/opt/lib"),
        ];
        variables.extend(checkout);
        let paths = resolve(&variables).unwrap();
        assert_eq!(paths.header, PathBuf::from("/opt/include/Interface.h"));
        assert_eq!(paths.library_dir, Some(PathBuf::from("/opt/lib")));
    }
}

#[test]
fn missing_or_empty_home_explains_how_to_select_header() {
    for variables in [
        vec![],
        vec![("HOME", "")],
        vec![("MAESTRO_LIBRARY_DIR", "/opt/lib")],
    ] {
        let error = resolve(&variables).unwrap_err();
        assert!(error.contains("HOME"));
        assert!(error.contains("MAESTRO_DIR or MAESTRO_HEADER"));
    }
}

#[test]
fn empty_explicit_paths_are_not_silently_defaulted() {
    for variable in ["MAESTRO_DIR", "MAESTRO_HEADER", "MAESTRO_LIBRARY_DIR"] {
        let error = resolve(&[("HOME", "/home/builder"), (variable, "")]).unwrap_err();
        assert!(error.contains(variable));
        assert!(error.contains("unset it"));
    }
}

#[test]
fn relative_paths_are_preserved_for_build_time_resolution() {
    let paths = resolve(&[("MAESTRO_DIR", "../maestro")]).unwrap();
    assert_eq!(
        paths.header,
        PathBuf::from("../maestro/maestrolib/Interface.h")
    );
    assert_eq!(paths.library_dir, Some(PathBuf::from("../maestro/build")));
}

#[test]
fn cargo_watches_only_defined_environment_inputs() {
    let defaults = resolve(&[("HOME", "/home/builder")]).unwrap();
    assert_eq!(defaults.watched_env, ["HOME"]);

    let header = resolve(&[("MAESTRO_HEADER", "/opt/include/Interface.h")]).unwrap();
    assert_eq!(header.watched_env, ["MAESTRO_HEADER"]);

    let library = resolve(&[
        ("HOME", "/home/builder"),
        ("MAESTRO_LIBRARY_DIR", "/opt/lib"),
    ])
    .unwrap();
    assert_eq!(library.watched_env, ["MAESTRO_LIBRARY_DIR", "HOME"]);

    let explicit = resolve(&[
        ("MAESTRO_DIR", "/opt/maestro"),
        ("MAESTRO_HEADER", "/opt/include/Interface.h"),
        ("MAESTRO_LIBRARY_DIR", "/opt/lib"),
        ("HOME", "/unused/home"),
    ])
    .unwrap();
    assert_eq!(
        explicit.watched_env,
        ["MAESTRO_DIR", "MAESTRO_HEADER", "MAESTRO_LIBRARY_DIR"]
    );
}
