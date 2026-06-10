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
    link("CARGO_FEATURE_AIRSPY", "libairspy", "airspy");
    link("CARGO_FEATURE_AIRSPYHF", "libairspyhf", "airspyhf");
}
