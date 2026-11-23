//! Planning one capture for every session that shares a `[[device]]`: the
//! center frequency and sample rate that keep all of their channels in band.

use super::scan::RateCaps;

/// Keep every channel inside this fraction of the capture when planning the
/// rate: filter roll-off near the band edges (an Airspy R2 at 10 MS/s is flat
/// to about ±4.5 MHz) would otherwise cost the outermost channels.
pub const USABLE_FRACTION: f64 = 0.9;

/// Plan one device's capture.
///
/// `needs` are each channel's frequency and one-sided passband (Hz). A zero
/// passband marks a whole-capture mode (Mode S, UAT), which must sit at the
/// capture center. `min_rate` is the slowest capture any of the sessions can
/// work from.
///
/// An explicit `rate` or `center` is used as given and checked. Otherwise the
/// center is the midpoint of the channels, nudged off any channel that would
/// land at DC, and the rate is the smallest the device supports that keeps
/// every channel within [`USABLE_FRACTION`] of the capture.
pub fn plan_capture(
    needs: &[(u64, f64)],
    rate: Option<f64>,
    center: Option<u64>,
    min_rate: f64,
    caps: &RateCaps,
) -> Result<(f64, u64), String> {
    let freqs: Vec<u64> = needs.iter().map(|n| n.0).collect();
    let (Some(&lo), Some(&hi)) = (freqs.iter().min(), freqs.iter().max()) else {
        return Err("no channels to plan for".into());
    };
    let pinned: Vec<u64> = needs.iter().filter(|n| n.1 == 0.0).map(|n| n.0).collect();
    let mhz = |hz: f64| hz / 1e6;
    let center = match (center, pinned.first()) {
        (_, Some(&c)) if pinned.iter().any(|&f| f != c) => {
            return Err(
                "whole-capture sessions on one device need the same center frequency".into(),
            )
        }
        (Some(c), Some(&p)) if c != p => {
            return Err(format!(
                "a whole-capture session needs the center at {:.4} MHz",
                mhz(p as f64)
            ))
        }
        (_, Some(&p)) => p,
        (Some(c), None) => c,
        (None, None) => off_channels(&freqs),
    };
    // Half the capture width that every channel's passband needs.
    let half = needs
        .iter()
        .map(|&(f, pb)| (f as f64 - center as f64).abs() + pb)
        .fold(0.0f64, f64::max);
    if let Some(r) = rate {
        if !(r.is_finite() && r > 0.0) {
            return Err(format!("sample-rate {r} isn't a usable rate"));
        }
        if r < min_rate {
            return Err(format!(
                "sample-rate {:.3} MS/s is below the {:.3} MS/s a session needs",
                mhz(r),
                mhz(min_rate)
            ));
        }
        if 2.0 * half > r {
            return Err(format!(
                "channels span {:.3}–{:.3} MHz, which needs at least {:.3} MS/s around {:.4} MHz; \
                 sample-rate is {:.3} MS/s",
                mhz(lo as f64),
                mhz(hi as f64),
                mhz(2.0 * half),
                mhz(center as f64),
                mhz(r)
            ));
        }
        return Ok((r, center));
    }
    let need = (2.0 * half / USABLE_FRACTION).max(min_rate);
    let smallest = |m: Option<f64>, r: f64| Some(m.map_or(r, |m: f64| m.min(r)));
    let picked = match caps {
        RateCaps::Discrete(rates) => rates
            .iter()
            .map(|&r| f64::from(r))
            .filter(|&r| r >= need)
            .fold(None, smallest),
        RateCaps::Ranges(ranges) => ranges
            .iter()
            .filter_map(|&(min, max, step)| {
                // Round up to the range's own step, or to a 100 kHz-friendly
                // rate (2.4, 2.5 MS/s, …) when it is continuous.
                let v = need.max(min);
                let r = if step > 0.0 {
                    min + ((v - min) / step).ceil() * step
                } else {
                    (v / 100_000.0).ceil() * 100_000.0
                };
                match r <= max {
                    true => Some(r),
                    // A continuous range takes the exact rate.
                    false => (step <= 0.0 && v <= max).then_some(v),
                }
            })
            .fold(None, smallest),
        RateCaps::Unknown => {
            return Err(
                "can't tell which sample rates this device supports; set `sample-rate`".into(),
            )
        }
    };
    picked.map(|r| (r, center)).ok_or_else(|| {
        format!(
            "channels span {:.3}–{:.3} MHz, which needs about {:.3} MS/s; the device offers no rate that wide",
            mhz(lo as f64),
            mhz(hi as f64),
            mhz(need)
        )
    })
}

/// The channels' midpoint, or the nearest offset from it (in 12.5 kHz steps)
/// that keeps every channel at least 5 kHz from DC, where the capture's DC
/// spike sits.
fn off_channels(freqs: &[u64]) -> u64 {
    let mid = crate::tuning::centroid_off_dc(freqs, false) as i64;
    let clear = |c: i64| freqs.iter().all(|&f| (f as i64 - c).abs() >= 5_000);
    (0..=40)
        .flat_map(|k| [k * 12_500, -k * 12_500])
        .map(|d| mid + d)
        .find(|&c| clear(c))
        .unwrap_or(mid) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::scan::passband;
    use xng_types::Mode;

    const ACARS: [u64; 14] = [
        129_000_000,
        129_350_000,
        130_025_000,
        130_425_000,
        130_450_000,
        130_850_000,
        131_125_000,
        131_425_000,
        131_475_000,
        131_525_000,
        131_550_000,
        131_670_000,
        131_725_000,
        131_825_000,
    ];
    const VDL2: [u64; 5] = [
        136_100_000,
        136_650_000,
        136_700_000,
        136_800_000,
        136_975_000,
    ];

    /// The ACARS and VDL2 channels of a 129–137 MHz station, with passbands.
    fn needs() -> Vec<(u64, f64)> {
        let (a, v) = (passband(Mode::AcarsPoa), passband(Mode::Vdl2));
        ACARS
            .iter()
            .map(|&f| (f, a))
            .chain(VDL2.iter().map(|&f| (f, v)))
            .collect()
    }

    /// The rates an Airspy R2 advertises.
    fn r2() -> RateCaps {
        RateCaps::Discrete(vec![10_000_000, 2_500_000])
    }

    /// The rate ranges SoapySDR reports for an RTL-SDR.
    fn rtl() -> RateCaps {
        RateCaps::Ranges(vec![
            (225_001.0, 300_000.0, 0.0),
            (900_001.0, 3_200_000.0, 0.0),
        ])
    }

    /// ACARS and VDL2 across 129–137 MHz fit one R2 at 10 MS/s.
    #[test]
    fn plans_acars_and_vdl2_on_an_r2() {
        assert_eq!(
            plan_capture(&needs(), None, None, 0.0, &r2()),
            Ok((10_000_000.0, 132_987_500))
        );
    }

    /// A narrow channel set takes the device's smaller rate.
    #[test]
    fn narrow_set_takes_the_smaller_rate() {
        let n: Vec<_> = VDL2[1..].iter().map(|&f| (f, 8_500.0)).collect();
        let (rate, _) = plan_capture(&n, None, None, 0.0, &r2()).unwrap();
        assert_eq!(rate, 2_500_000.0);
    }

    /// On a continuous range the rate rounds up to a multiple of 100 kHz, and
    /// every channel stays in the usable band.
    #[test]
    fn rtl_sdr_ranges_round_up_to_a_friendly_rate() {
        let n: Vec<_> = ACARS[6..].iter().map(|&f| (f, 5_000.0)).collect();
        let (rate, center) = plan_capture(&n, None, None, 0.0, &rtl()).unwrap();
        assert_eq!(rate % 100_000.0, 0.0, "{rate}");
        assert!((900_001.0..=3_200_000.0).contains(&rate));
        for &(f, pb) in &n {
            assert!((f as f64 - center as f64).abs() + pb <= rate / 2.0 * USABLE_FRACTION);
        }
    }

    /// Channels wider apart than any rate the device offers are an error.
    #[test]
    fn too_wide_for_the_device_is_an_error() {
        let n = vec![(129_000_000, 5_000.0), (162_000_000, 5_000.0)];
        let e = plan_capture(&n, None, None, 0.0, &r2()).unwrap_err();
        assert!(e.contains("no rate that wide"), "{e}");
    }

    /// An explicit rate and center are kept when the channels fit, and refused
    /// when they don't.
    #[test]
    fn explicit_rate_and_center_are_checked_not_replaced() {
        assert_eq!(
            plan_capture(&needs(), Some(10e6), Some(133_000_000), 0.0, &r2()),
            Ok((10e6, 133_000_000))
        );
        let e = plan_capture(&needs(), Some(2.5e6), None, 0.0, &r2()).unwrap_err();
        assert!(e.contains("needs at least"), "{e}");
    }

    /// Without the device's rates, the rate has to be given.
    #[test]
    fn unknown_rates_need_an_explicit_rate() {
        assert!(plan_capture(&needs(), None, None, 0.0, &RateCaps::Unknown).is_err());
    }

    /// No channel is left sitting at DC.
    #[test]
    fn center_moves_off_a_channel_at_dc() {
        let n = vec![
            (136_000_000, 8_500.0),
            (136_500_000, 8_500.0),
            (137_000_000, 8_500.0),
        ];
        let (_, center) = plan_capture(&n, None, None, 0.0, &r2()).unwrap();
        assert_ne!(center, 136_500_000);
    }

    /// A whole-capture mode fixes the center and sets a floor on the rate.
    #[test]
    fn whole_capture_modes_pin_the_center_and_set_a_floor() {
        let n = vec![(1_090_000_000, 0.0)];
        assert_eq!(
            plan_capture(&n, None, None, 2_000_000.0, &rtl()),
            Ok((2_000_000.0, 1_090_000_000))
        );
        assert!(plan_capture(&n, None, Some(1_089_000_000), 2e6, &rtl()).is_err());
        assert!(plan_capture(&n, Some(1e6), None, 2e6, &rtl())
            .unwrap_err()
            .contains("below"));
    }

    /// The center steps off every channel, not just the one at the midpoint.
    #[test]
    fn center_avoids_every_channel() {
        let n: Vec<_> = [131_500_000, 131_550_000, 131_575_000, 131_600_000]
            .iter()
            .map(|&f| (f, 5_000.0))
            .collect();
        let (_, center) = plan_capture(&n, None, None, 0.0, &r2()).unwrap();
        for &(f, _) in &n {
            assert!((f as i64 - center as i64).abs() >= 5_000, "{center} is on {f}");
        }
    }

    /// A rate that isn't a positive number is refused.
    #[test]
    fn nonsense_rates_are_refused() {
        for r in [f64::NAN, f64::INFINITY, 0.0, -1e6] {
            assert!(plan_capture(&needs(), Some(r), None, 0.0, &r2()).is_err(), "{r}");
        }
    }

    /// When rounding up would leave a continuous range, the exact rate is used.
    #[test]
    fn a_narrow_continuous_range_takes_the_exact_rate() {
        // Needs 1.03 MS/s, which rounds up to 1.1 MS/s.
        let n = vec![(99_900_000, 363_500.0), (100_100_000, 363_500.0)];
        let caps = RateCaps::Ranges(vec![(1_020_000.0, 1_050_000.0, 0.0)]);
        let (rate, _) = plan_capture(&n, None, None, 0.0, &caps).unwrap();
        assert!((1_020_000.0..=1_050_000.0).contains(&rate), "{rate}");
    }
}
