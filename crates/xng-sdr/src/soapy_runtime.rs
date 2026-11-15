//! Finding the SoapySDR runtime on Windows.
//!
//! Elsewhere SoapySDR is a normal shared library that is present whenever
//! xng was built against it. On Windows it comes from a separate install
//! (PothosSDR), so xng links it delay-loaded: the program starts without it,
//! and [`ensure`] loads it on first use or explains what to install. The
//! lookup never includes the working directory.

/// Where to get SoapySDR for Windows.
pub const INSTALL_HINT: &str = "SoapySDR is not installed. Install PothosSDR \
(https://downloads.myriadrf.org/builds/PothosSDR/), then start xng again. \
Recordings work without it.";

/// Make sure SoapySDR can be called. Always `Ok` off Windows.
pub fn ensure() -> Result<(), String> {
    #[cfg(windows)]
    {
        static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOADED.get_or_init(windows::load) {
            Ok(())
        } else {
            Err(INSTALL_HINT.to_owned())
        }
    }
    #[cfg(not(windows))]
    Ok(())
}

/// The folders to look for `SoapySDR.dll` in, after the program's own
/// folder and the system folder: every absolute `PATH` entry, an explicit
/// `SOAPY_SDR_ROOT`, then PothosSDR's default install locations.
///
/// The working directory is never searched as such. Relative entries,
/// in `PATH` or in any of the variables, are left out for the same
/// reason: they would be read from wherever xng was started.
#[cfg_attr(not(windows), allow(dead_code))]
fn candidate_dirs(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut add = |dir: std::path::PathBuf| {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(path) = env("PATH") {
        std::env::split_paths(&path).for_each(&mut add);
    }
    if let Some(root) = env("SOAPY_SDR_ROOT") {
        add(std::path::PathBuf::from(root).join("bin"));
    }
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(pf) = env(var) {
            add(std::path::PathBuf::from(pf).join("PothosSDR").join("bin"));
        }
    }
    dirs
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const DLL: &str = "SoapySDR.dll";

    /// Resolve the DLL's own imports from the folder it is in.
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
    const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x0000_0200;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut core::ffi::c_void, flags: u32) -> *mut core::ffi::c_void;
    }

    fn load_library(path: &Path, flags: u32) -> bool {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives
        // the call, and the reserved file handle is null as required. The
        // module handle is deliberately never freed: the library stays
        // loaded for the life of the process.
        !unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), flags) }.is_null()
    }

    /// Load SoapySDR, so the delay-loaded imports resolve to it.
    ///
    /// The DLL is never looked up through the default search order, which
    /// includes the working directory: a `SoapySDR.dll` dropped in the
    /// folder xng happens to be started from must not be loaded.
    pub fn load() -> bool {
        // By name, from the program's folder and the system folder only.
        if load_library(Path::new(DLL), LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) {
            return true;
        }
        let on_path: Vec<std::path::PathBuf> =
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        for dir in super::candidate_dirs(|k| std::env::var_os(k)) {
            let dll = dir.join(DLL);
            if !dll.is_file() {
                continue;
            }
            // SoapySDR's driver modules load their own libraries (rtlsdr,
            // libusb, ...) from this folder by name, so it has to be on
            // the search path, not just the one DLL loaded by full path.
            if !on_path.contains(&dir) {
                let mut path = vec![dir.clone()];
                path.extend(on_path.iter().cloned());
                if let Ok(joined) = std::env::join_paths(path) {
                    std::env::set_var("PATH", joined);
                }
            }
            if load_library(&dll, LOAD_WITH_ALTERED_SEARCH_PATH) {
                // This crate has no logger; stderr is where the CLI logs go.
                eprintln!("using SoapySDR from {}", dir.display());
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_on_path_then_the_explicit_root_then_the_default_install() {
        use std::path::PathBuf;
        // An absolute folder on whichever system runs the test.
        let abs = |p: &str| -> PathBuf {
            let root = if cfg!(windows) { "C:\\" } else { "/" };
            PathBuf::from(root).join(p)
        };
        let path = std::env::join_paths([abs("opt/sdr"), PathBuf::from("relative"), PathBuf::from("."), abs("tools")])
            .unwrap();
        let (root, pf) = (abs("d/sdr"), abs("pf"));
        let env = {
            let (root, pf) = (root.clone(), pf.clone());
            move |k: &str| match k {
                "PATH" => Some(path.clone()),
                "SOAPY_SDR_ROOT" => Some(root.clone().into_os_string()),
                "ProgramFiles" | "ProgramW6432" => Some(pf.clone().into_os_string()),
                _ => None,
            }
        };
        // Relative PATH entries and the working directory are never searched.
        assert_eq!(
            candidate_dirs(env),
            [abs("opt/sdr"), abs("tools"), root.join("bin"), pf.join("PothosSDR").join("bin")]
        );
    }

    #[test]
    fn leaves_out_relative_roots() {
        let env = |k: &str| match k {
            "SOAPY_SDR_ROOT" => Some(std::ffi::OsString::from("sdr")),
            "ProgramFiles" => Some(std::ffi::OsString::from(".")),
            "ProgramW6432" => Some(std::ffi::OsString::from("")),
            _ => None,
        };
        assert!(candidate_dirs(env).is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn is_always_ready_off_windows() {
        assert!(ensure().is_ok());
    }
}
