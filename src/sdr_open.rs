//! Opening SDR devices from `--sdr`-style selector strings, and probing
//! what sample rates they can do. Shared by the CLI commands and anything
//! embedding the decode engine (e.g. the desktop app).

/// Open an SDR from `--sdr` args, returning the source and the backend that
/// served it. `driver=airspy` / `driver=airspyhf` use the native backends
/// when compiled in (falling through to SoapySDR otherwise, where a
/// SoapyAirspy module may exist); `backend=soapy` forces the fallthrough.
pub fn open_sdr(
    sdr: &str,
    sample_rate: f64,
    center_hz: u64,
    gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    let args = crate::sdr_args::SdrArgs::parse(sdr);
    match args.driver.as_deref() {
        Some("airspy") if !args.force_soapy => {
            #[cfg(feature = "airspy")]
            {
                let serial =
                    args.serial.as_deref().map(crate::sdr_args::parse_airspy_serial).transpose()?;
                let src = xng_sdr::airspy::AirspyIqSource::open(
                    serial,
                    sample_rate,
                    center_hz,
                    gain,
                    args.bias,
                )?;
                return Ok((Box::new(src), "airspy"));
            }
        }
        Some("airspyhf") if !args.force_soapy => {
            #[cfg(feature = "airspyhf")]
            {
                let serial =
                    args.serial.as_deref().map(crate::sdr_args::parse_airspy_serial).transpose()?;
                let src = xng_sdr::airspyhf::AirspyHfIqSource::open(
                    serial,
                    sample_rate,
                    center_hz,
                    gain,
                    args.bias,
                )?;
                return Ok((Box::new(src), "airspyhf"));
            }
        }
        _ => {}
    }
    open_soapy(&args.soapy, sample_rate, center_hz, gain)
}

#[cfg(feature = "soapy")]
fn open_soapy(
    args: &str,
    sample_rate: f64,
    center_hz: u64,
    gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    let src = xng_sdr::soapy::SoapyIqSource::open(args, sample_rate, center_hz, gain)?;
    Ok((Box::new(src), "soapysdr"))
}

#[cfg(not(feature = "soapy"))]
fn open_soapy(
    _args: &str,
    _sample_rate: f64,
    _center_hz: u64,
    _gain: Option<f64>,
) -> anyhow::Result<(Box<dyn xng_sdr::IqSource>, &'static str)> {
    anyhow::bail!("built without SDR support; use --file or rebuild with --features soapy")
}

/// Probe what sample rates the device `--sdr` selects can do. Native backends
/// report an exact discrete set; SoapySDR devices (RTL-SDR, HackRF, SDRplay…)
/// report advertised ranges; otherwise [`RateCaps::Unknown`] so callers fall
/// back to the mode's plan rate.
pub fn probe_rate_caps(sdr: &str) -> crate::commands::scan::RateCaps {
    use crate::commands::scan::RateCaps;
    let args = crate::sdr_args::SdrArgs::parse(sdr);
    let serial = || args.serial.as_deref().and_then(|s| crate::sdr_args::parse_airspy_serial(s).ok());
    let _ = &serial; // used only by the cfg'd arms below
    if !args.force_soapy {
        match args.driver.as_deref() {
            #[cfg(feature = "airspy")]
            Some("airspy") => {
                if let Ok(r) = xng_sdr::airspy::device_rates(serial()) {
                    return RateCaps::Discrete(r);
                }
            }
            #[cfg(feature = "airspyhf")]
            Some("airspyhf") => {
                if let Ok(r) = xng_sdr::airspyhf::device_rates(serial()) {
                    return RateCaps::Discrete(r);
                }
            }
            _ => {}
        }
    }
    #[cfg(feature = "soapy")]
    {
        let ranges = xng_sdr::soapy::sample_rate_ranges(&args.soapy);
        if !ranges.is_empty() {
            return RateCaps::Ranges(ranges);
        }
    }
    RateCaps::Unknown
}
