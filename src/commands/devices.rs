//! `xng devices` — enumerate SDR hardware.

#[cfg(feature = "soapy")]
pub fn run(filter: &str) -> anyhow::Result<()> {
    let devices = xng_sdr::soapy::enumerate(filter)?;
    if devices.is_empty() {
        println!("No SDR devices found{}.", if filter.is_empty() { String::new() } else { format!(" matching '{filter}'") });
        return Ok(());
    }
    println!("{} device(s):", devices.len());
    for (i, d) in devices.iter().enumerate() {
        println!("  [{i}] driver={} {}", d.driver, if d.label.is_empty() { &d.args } else { &d.label });
    }
    Ok(())
}

#[cfg(not(feature = "soapy"))]
pub fn run(_filter: &str) -> anyhow::Result<()> {
    anyhow::bail!("xng was built without SDR device support; rebuild with --features soapy")
}
