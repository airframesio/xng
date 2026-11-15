fn main() {
    // Windows: link SoapySDR delay-loaded, so the app starts on a machine
    // without PothosSDR (`xng_sdr::soapy_runtime` loads it on first use).
    let target = |k: &str, v: &str| std::env::var(k).is_ok_and(|x| x == v);
    if std::env::var_os("CARGO_FEATURE_SOAPY").is_some()
        && target("CARGO_CFG_TARGET_OS", "windows")
        && target("CARGO_CFG_TARGET_ENV", "msvc")
    {
        println!("cargo:rustc-link-arg=/DELAYLOAD:SoapySDR.dll");
    }
    tauri_build::build()
}
