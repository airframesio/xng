//! Frequency argument parsing.

/// Parse a frequency string into Hz. Accepts `131550000`, `131.55M`,
/// `131.55MHz`, `136975K`, or a bare number `< 1000` treated as MHz
/// (the common way ACARS channels are written, e.g. `131.550`).
pub fn parse_hz(s: &str) -> anyhow::Result<u64> {
    let t = s.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(p) = t.strip_suffix("mhz").or_else(|| t.strip_suffix('m')) {
        (p, 1e6)
    } else if let Some(p) = t.strip_suffix("khz").or_else(|| t.strip_suffix('k')) {
        (p, 1e3)
    } else if let Some(p) = t.strip_suffix("hz") {
        (p, 1.0)
    } else {
        (t.as_str(), 0.0) // bare number: decide by magnitude
    };
    let v: f64 = num.trim().parse().map_err(|_| anyhow::anyhow!("invalid frequency: {s}"))?;
    let hz = if mult > 0.0 {
        v * mult
    } else if v < 1000.0 {
        v * 1e6
    } else {
        v
    };
    anyhow::ensure!(hz > 0.0 && hz < 30e9, "frequency out of range: {s}");
    Ok(hz.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_hz("131550000").unwrap(), 131_550_000);
        assert_eq!(parse_hz("131.55M").unwrap(), 131_550_000);
        assert_eq!(parse_hz("131.55MHz").unwrap(), 131_550_000);
        assert_eq!(parse_hz("131.550").unwrap(), 131_550_000);
        assert_eq!(parse_hz("136975k").unwrap(), 136_975_000);
        assert!(parse_hz("bogus").is_err());
    }
}
