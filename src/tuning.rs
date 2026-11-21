//! Tuning resolution: turning a mode plus optional rate / center / channel
//! choices into a concrete capture plan. Strict ([`parse_tune`], for
//! decode/listen) or zero-config ([`resolve_tune_auto`], deriving whatever
//! is omitted from the mode's built-in frequency plan).

/// What the user asked to tune, before resolution. Every field but `mode`
/// may be left empty; [`resolve_tune_auto`] fills the gaps.
#[derive(Clone, Debug, Default)]
pub struct Tune {
    /// Decode mode name (e.g. "acars", "vdl2", "ais").
    pub mode: String,
    /// Capture sample rate in Hz.
    pub sample_rate: Option<f64>,
    /// Capture center frequency (e.g. "131.500M").
    pub center_freq: Option<String>,
    /// Channel frequencies (e.g. "131.550").
    pub channels: Vec<String>,
    /// Receiver location as "lat,lon".
    pub receiver_pos: Option<String>,
    /// Only pass ACARS messages with these labels.
    pub filter_labels: Vec<String>,
    /// Drop ACARS messages with these labels.
    pub exclude_labels: Vec<String>,
    /// Demod effort override.
    pub demod_effort: Option<crate::runtime::DemodEffort>,
    /// VDL2 only: reject bursts beyond this carrier offset (ppm).
    pub max_ppm: Option<f64>,
}

impl Tune {
    /// The ACARS label filter this tune asks for.
    pub fn label_filter(&self) -> crate::runtime::LabelFilter {
        crate::runtime::LabelFilter {
            include: self.filter_labels.clone(),
            exclude: self.exclude_labels.clone(),
        }
    }
}

/// Parse `--receiver-pos` / `receiver-pos` ("lat,lon").
pub fn parse_receiver_pos(s: &Option<String>) -> anyhow::Result<Option<(f64, f64)>> {
    match s {
        None => Ok(None),
        Some(v) => {
            let (a, b) = v
                .split_once(',')
                .ok_or_else(|| anyhow::anyhow!("--receiver-pos wants lat,lon"))?;
            Ok(Some((a.trim().parse()?, b.trim().parse()?)))
        }
    }
}

/// Strict tune parsing for decode/listen: everything must be explicit.
pub fn parse_tune(tune: &Tune) -> anyhow::Result<(xng_types::Mode, f64, u64, Vec<u64>)> {
    let mode: xng_types::Mode = tune.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let rate = tune
        .sample_rate
        .ok_or_else(|| anyhow::anyhow!("missing -r/--sample-rate"))?;
    let center = tune
        .center_freq
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing -c/--center-freq"))?;
    let center = crate::freq::parse_hz(center)?;
    anyhow::ensure!(!tune.channels.is_empty(), "missing --channels");
    let channels = tune
        .channels
        .iter()
        .map(|c| crate::freq::parse_hz(c))
        .collect::<anyhow::Result<Vec<u64>>>()?;
    Ok((mode, rate, center, channels))
}

/// Zero-config tuning for the tui: anything omitted is derived from the
/// mode's built-in frequency plan — sample rate from the plan (checked
/// against the device's advertised rates where a native backend can
/// tell us), channels from the densest capture window that fits it
/// (trimmed to a CPU budget, core channels first), center from the
/// chosen channels.
pub fn resolve_tune_auto(
    tune: &Tune,
    sdr: &str,
) -> anyhow::Result<(xng_types::Mode, f64, u64, Vec<u64>)> {
    let mode: xng_types::Mode = tune.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let (plan_rate, plan_channels) = crate::commands::scan::plan(mode);
    let rate = match tune.sample_rate {
        Some(r) => r,
        None => {
            let caps = crate::sdr_open::probe_rate_caps(sdr);
            let r = crate::commands::scan::choose_rate(&caps, mode, plan_rate);
            if r != plan_rate {
                tracing::info!(
                    "auto-tune: using {} S/s (device does not offer the plan's {} S/s)",
                    r as u64,
                    plan_rate as u64
                );
            }
            if let Some(hint) = crate::commands::scan::rate_choice_hint(mode, r, plan_rate) {
                tracing::info!("auto-tune: {hint}");
            }
            r
        }
    };

    // Explicit channels: only the center may need deriving.
    if !tune.channels.is_empty() {
        let channels = tune
            .channels
            .iter()
            .map(|c| crate::freq::parse_hz(c))
            .collect::<anyhow::Result<Vec<u64>>>()?;
        let center = match &tune.center_freq {
            Some(c) => crate::freq::parse_hz(c)?,
            None => centroid_off_dc(&channels, crate::commands::scan::passband(mode) > 0.0),
        };
        return Ok((mode, rate, center, channels));
    }

    // Explicit center, channels from the plan: keep plan channels that
    // fit the capture around that center.
    if let Some(c) = &tune.center_freq {
        let center = crate::freq::parse_hz(c)?;
        let half = (rate * 0.4) as i64; // 80% usable width, half each side
        let channels: Vec<u64> = plan_channels
            .iter()
            .copied()
            .filter(|f| (*f as i64 - center as i64).abs() <= half)
            .collect();
        anyhow::ensure!(
            !channels.is_empty(),
            "no {mode} plan channels within the capture around {:.3} MHz; pass --channels",
            center as f64 / 1e6
        );
        return Ok((mode, rate, center, channels));
    }

    // Fully automatic. DDC-per-channel is cheap, but a small box should
    // not be saddled with a worldwide plan: budget ~4 channels per core.
    let budget = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) * 4;
    let (center, channels) = crate::commands::scan::auto_window(mode, rate, budget)
        .ok_or_else(|| {
            anyhow::anyhow!("mode {mode} has no built-in frequency plan; pass --channels")
        })?;
    tracing::info!(
        "auto-tune: {} channel(s) around {:.3} MHz at {} S/s (budget {budget})",
        channels.len(),
        center as f64 / 1e6,
        rate as u64
    );
    Ok((mode, rate, center, channels))
}

/// Midpoint of a channel set. For DDC modes (`avoid_dc`) the midpoint is
/// nudged off any exact channel so no carrier sits at DC; whole-capture
/// modes (Mode S) require the channel AT the center, so they pass
/// `avoid_dc = false`.
pub fn centroid_off_dc(channels: &[u64], avoid_dc: bool) -> u64 {
    let center =
        (channels.iter().min().unwrap() + channels.iter().max().unwrap()) / 2;
    if avoid_dc && channels.contains(&center) { center + 25_000 } else { center }
}
