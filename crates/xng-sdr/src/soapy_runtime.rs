//! Finding the SoapySDR runtime on Windows.
//!
//! Elsewhere SoapySDR is a normal shared library that is present whenever
//! xng was built against it. On Windows it comes from a separate install
//! (PothosSDR), so xng links it delay-loaded: the program starts without it,
//! and [`ensure`] loads it on first use or explains what to install.

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

/// The folders to look for `SoapySDR.dll` in when the normal DLL search
/// (program folder, system folders, PATH) does not find it: an explicit
/// `SOAPY_SDR_ROOT`, then PothosSDR's default install locations.
#[cfg_attr(not(windows), allow(dead_code))]
fn candidate_dirs(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = env("SOAPY_SDR_ROOT") {
        dirs.push(std::path::PathBuf::from(root).join("bin"));
    }
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(pf) = env(var) {
            let dir = std::path::PathBuf::from(pf).join("PothosSDR").join("bin");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const DLL: &str = "SoapySDR.dll";

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut core::ffi::c_void;
    }

    fn load_library(path: &Path) -> bool {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives
        // the call. The handle is deliberately never freed: the library
        // stays loaded for the life of the process.
        !unsafe { LoadLibraryW(wide.as_ptr()) }.is_null()
    }

    /// Load SoapySDR, so the delay-loaded imports resolve to it.
    pub fn load() -> bool {
        if load_library(Path::new(DLL)) {
            return true;
        }
        for dir in super::candidate_dirs(|k| std::env::var_os(k)) {
            if !dir.join(DLL).is_file() {
                continue;
            }
            // SoapySDR's driver modules load their own libraries (rtlsdr,
            // libusb, ...) from this folder by name, so it has to be on
            // the search path, not just the one DLL loaded by full path.
            let mut path = vec![dir.clone()];
            if let Some(existing) = std::env::var_os("PATH") {
                path.extend(std::env::split_paths(&existing));
            }
            if let Ok(joined) = std::env::join_paths(path) {
                std::env::set_var("PATH", joined);
            }
            if load_library(&dir.join(DLL)) {
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
    fn looks_in_the_explicit_root_first_then_the_default_install() {
        let env = |k: &str| match k {
            "SOAPY_SDR_ROOT" => Some("D:/sdr".into()),
            "ProgramFiles" | "ProgramW6432" => Some("C:/Program Files".into()),
            _ => None,
        };
        let dirs = candidate_dirs(env);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(dirs[0].starts_with("D:/sdr"));
        assert!(dirs[1].ends_with("PothosSDR/bin"));
    }

    #[cfg(not(windows))]
    #[test]
    fn is_always_ready_off_windows() {
        assert!(ensure().is_ok());
    }
}
