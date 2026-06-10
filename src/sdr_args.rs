//! `--sdr` argument parsing: SoapySDR-style comma-separated `key=value` pairs.
//!
//! The same string serves both backends. `driver=airspy` / `driver=airspyhf`
//! route to the native libairspy/libairspyhf backends when compiled in (and
//! fall through to SoapySDR otherwise); `backend=soapy` forces SoapySDR even
//! when a native backend exists.

pub struct SdrArgs {
    pub driver: Option<String>,
    pub serial: Option<String>,
    /// `bias=1` — enable the antenna bias tee (native backends).
    pub bias: bool,
    /// `backend=soapy` — skip native routing.
    pub force_soapy: bool,
    /// The argument string with xng-only keys stripped, for SoapySDR.
    pub soapy: String,
}

impl SdrArgs {
    pub fn parse(s: &str) -> Self {
        let mut out = Self {
            driver: None,
            serial: None,
            bias: false,
            force_soapy: false,
            soapy: String::new(),
        };
        let mut soapy_parts: Vec<&str> = Vec::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            match k {
                "driver" => out.driver = Some(v.to_owned()),
                "serial" => out.serial = Some(v.to_owned()),
                "bias" | "bias_tee" => {
                    out.bias = matches!(v, "1" | "true" | "yes" | "");
                }
                "backend" => {
                    out.force_soapy = v == "soapy";
                    continue; // xng-only key; never forward
                }
                _ => {}
            }
            soapy_parts.push(part);
        }
        out.soapy = soapy_parts.join(",");
        out
    }
}

/// Airspy serials are 64-bit values conventionally printed as 16 hex digits
/// (as by `airspy_info` and `xng devices`).
#[cfg_attr(not(any(feature = "airspy", feature = "airspyhf")), allow(dead_code))]
pub fn parse_airspy_serial(s: &str) -> anyhow::Result<u64> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(t, 16)
        .map_err(|_| anyhow::anyhow!("invalid Airspy serial '{s}' (expected hex, e.g. 644064DC2B583BD3)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_driver_serial_and_bias() {
        let a = SdrArgs::parse("driver=airspy,serial=644064DC2B583BD3,bias=1");
        assert_eq!(a.driver.as_deref(), Some("airspy"));
        assert_eq!(a.serial.as_deref(), Some("644064DC2B583BD3"));
        assert!(a.bias);
        assert!(!a.force_soapy);
    }

    #[test]
    fn backend_soapy_is_stripped_from_forwarded_args() {
        let a = SdrArgs::parse("driver=airspy,backend=soapy,serial=AA");
        assert!(a.force_soapy);
        assert_eq!(a.soapy, "driver=airspy,serial=AA");
    }

    #[test]
    fn empty_and_plain_soapy_args_pass_through() {
        let a = SdrArgs::parse("");
        assert!(a.driver.is_none());
        assert_eq!(a.soapy, "");
        let b = SdrArgs::parse("driver=rtlsdr,rtl=0");
        assert_eq!(b.driver.as_deref(), Some("rtlsdr"));
        assert_eq!(b.soapy, "driver=rtlsdr,rtl=0");
    }

    #[test]
    fn serial_parses_as_hex() {
        assert_eq!(parse_airspy_serial("0x10").unwrap(), 16);
        assert_eq!(parse_airspy_serial("644064DC2B583BD3").unwrap(), 0x644064DC2B583BD3);
        assert!(parse_airspy_serial("not-a-serial").is_err());
    }
}
