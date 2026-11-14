//! `xng devices` — enumerate SDR hardware.
//!
//! Native backends (libairspy/libairspyhf) are listed first when compiled in;
//! SoapySDR enumeration follows and honors the filter string. A device with
//! both a native backend and a Soapy module installed appears in both lists —
//! the native line shows the exact `--sdr` args that select the native path.

/// One SDR device xng can open.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DeviceEntry {
    /// The exact `--sdr` selector that opens this device.
    pub selector: String,
    /// Human-readable name (the device's label, or its driver).
    pub label: String,
    /// Backend that serves it: "airspy", "airspyhf" or "soapysdr".
    pub backend: &'static str,
}

/// Enumerate every device the compiled-in backends can see. Native
/// backends come first; SoapySDR results honour `filter` (e.g.
/// "driver=rtlsdr").
#[cfg(any(feature = "soapy", feature = "airspy", feature = "airspyhf"))]
pub fn list(filter: &str) -> anyhow::Result<Vec<DeviceEntry>> {
    let mut out = Vec::new();
    #[cfg(feature = "airspy")]
    for sn in xng_sdr::airspy::enumerate()? {
        out.push(DeviceEntry {
            selector: format!("driver=airspy,serial={sn:016X}"),
            label: format!("Airspy {sn:016X}"),
            backend: "airspy",
        });
    }
    #[cfg(feature = "airspyhf")]
    for sn in xng_sdr::airspyhf::enumerate()? {
        out.push(DeviceEntry {
            selector: format!("driver=airspyhf,serial={sn:016X}"),
            label: format!("Airspy HF+ {sn:016X}"),
            backend: "airspyhf",
        });
    }
    #[cfg(feature = "soapy")]
    for d in xng_sdr::soapy::enumerate(filter)? {
        let label = if d.label.is_empty() { d.driver.clone() } else { d.label.clone() };
        out.push(DeviceEntry { selector: d.args, label, backend: "soapysdr" });
    }
    #[cfg(not(feature = "soapy"))]
    let _ = filter;
    Ok(out)
}

/// Built without any SDR backend: nothing to list.
#[cfg(not(any(feature = "soapy", feature = "airspy", feature = "airspyhf")))]
pub fn list(_filter: &str) -> anyhow::Result<Vec<DeviceEntry>> {
    Ok(Vec::new())
}

#[cfg(not(any(feature = "soapy", feature = "airspy", feature = "airspyhf")))]
pub fn run(_filter: &str) -> anyhow::Result<()> {
    anyhow::bail!("xng was built without SDR device support; rebuild with --features soapy")
}

#[cfg(any(feature = "soapy", feature = "airspy", feature = "airspyhf"))]
pub fn run(filter: &str) -> anyhow::Result<()> {
    let mut lines: Vec<String> = Vec::new();

    #[cfg(feature = "airspy")]
    for sn in xng_sdr::airspy::enumerate()? {
        lines.push(format!("driver=airspy,serial={sn:016X} (native libairspy)"));
    }
    #[cfg(feature = "airspyhf")]
    for sn in xng_sdr::airspyhf::enumerate()? {
        lines.push(format!("driver=airspyhf,serial={sn:016X} (native libairspyhf)"));
    }
    #[cfg(feature = "soapy")]
    for d in xng_sdr::soapy::enumerate(filter)? {
        let label = if d.label.is_empty() { &d.args } else { &d.label };
        lines.push(format!("driver={} {} (soapysdr)", d.driver, label));
    }
    #[cfg(not(feature = "soapy"))]
    let _ = filter;

    if lines.is_empty() {
        println!(
            "No SDR devices found{}.",
            if filter.is_empty() { String::new() } else { format!(" matching '{filter}'") }
        );
        return Ok(());
    }
    println!("{} device(s):", lines.len());
    for (i, line) in lines.iter().enumerate() {
        println!("  [{i}] {line}");
    }
    Ok(())
}
