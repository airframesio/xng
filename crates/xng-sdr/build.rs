//! Link the native Airspy libraries when the corresponding features are on.
//! pkg-config emits the search paths and link directives; the fallback covers
//! installs without .pc files (plain `make install` to a standard prefix).

fn link(feature_env: &str, pc_name: &str, lib: &str) {
    if std::env::var_os(feature_env).is_none() {
        return;
    }
    if pkg_config::probe_library(pc_name).is_ok() {
        return;
    }
    for dir in ["/opt/homebrew/lib", "/usr/local/lib"] {
        if std::path::Path::new(dir).is_dir() {
            println!("cargo:rustc-link-search=native={dir}");
        }
    }
    println!("cargo:rustc-link-lib={lib}");
}

fn main() {
    // Windows: SoapySDR is delay-loaded (see src/soapy_runtime.rs), which
    // needs MSVC's delay-load helper. The /DELAYLOAD switch itself has to
    // come from each program's own build script.
    let target = |k: &str, v: &str| std::env::var(k).is_ok_and(|x| x == v);
    if std::env::var_os("CARGO_FEATURE_SOAPY").is_some()
        && target("CARGO_CFG_TARGET_OS", "windows")
        && target("CARGO_CFG_TARGET_ENV", "msvc")
    {
        println!("cargo:rustc-link-lib=delayimp");
    }
    link("CARGO_FEATURE_AIRSPY", "libairspy", "airspy");
    link("CARGO_FEATURE_AIRSPYHF", "libairspyhf", "airspyhf");
}
