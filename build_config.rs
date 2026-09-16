use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct BuildPaths {
    pub header: PathBuf,
    // None keeps the linker's normal search, with additional local prefixes.
    pub library_dir: Option<PathBuf>,
    pub watched_env: Vec<&'static str>,
}

impl BuildPaths {
    pub fn resolve(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Result<Self, String> {
        let mut watched_env = Vec::new();
        let mut configured = |name: &'static str| {
            let value = lookup(name);
            if value.is_some() {
                watched_env.push(name);
            }
            value
        };
        let maestro_dir = configured_path("MAESTRO_DIR", configured("MAESTRO_DIR"))?;
        let header = configured_path("MAESTRO_HEADER", configured("MAESTRO_HEADER"))?;
        let library_dir =
            configured_path("MAESTRO_LIBRARY_DIR", configured("MAESTRO_LIBRARY_DIR"))?;

        let header = match header {
            Some(path) => path,
            None => match &maestro_dir {
                Some(directory) => directory.join("maestrolib/Interface.h"),
                None => {
                    let home = configured("HOME").filter(|value| !value.is_empty()).ok_or(
                        "Cannot locate ~/maestro: HOME is unset or empty; set MAESTRO_DIR or MAESTRO_HEADER",
                    )?;
                    PathBuf::from(home).join("maestro/maestrolib/Interface.h")
                }
            },
        };
        let library_dir = library_dir.or_else(|| maestro_dir.map(|path| path.join("build")));

        Ok(Self {
            header,
            library_dir,
            watched_env,
        })
    }
}

fn configured_path(name: &str, value: Option<OsString>) -> Result<Option<PathBuf>, String> {
    match value {
        Some(value) if value.is_empty() => Err(format!(
            "{name} is empty; unset it to use the default build paths"
        )),
        value => Ok(value.map(PathBuf::from)),
    }
}
