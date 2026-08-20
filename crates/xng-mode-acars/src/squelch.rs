//! Envelope squelch: skip the expensive demod chain while a channel is idle.
//!
//! ACARS channels carry nothing 87–99% of the time (a 50-character message is
//! ~254 ms of RF; even 30 messages/minute on one channel is 12.7% busy), yet
//! the demod runs its 121-tap audio lowpass, NCO, discriminator, timing loop
//! and the deframer's both-polarity sync hunt on every sample forever. Those
//! stages are ~1.3% of one core across 16 channels; the AM envelope and its
//! EMAs, which are enough to tell whether anything is there, are ~0.13%.
//!
//! This gate sits between the two: it consumes the envelope stream the demod
//! already computes and decides, per sample, whether the rest of the chain
//! runs. Closed samples are simply not appended to the mix buffer, so the
//! demod downstream of it is unchanged — the squelch is a stream editor, not
//! a new signal path.
//!
//! **Deliberately not public, and deliberately not in `xng-dsp`.** "Is this
//! channel busy" means something different for every waveform: VDL2 acquires
//! on a coherent unique-word correlation that an envelope gate in front of it
//! would only damage, AIS already has a power gate of its own, and STD-C's NCS
//! common channels are continuous carriers where a gate that ever closed would
//! drop carrier and timing lock. Presence detection belongs to each mode's
//! demod. Do not lift this into the shared front end.
//!
//! ## Why the gate cannot truncate a burst
//!
//! Four independent margins, in the order they apply:
//!
//! 1. **A large detection window.** A burst opens with 128 bits of all-ones
//!    pre-key at 2400 bd — ~53 ms of continuous 2400 Hz carrier before any
//!    frame content. The level EMA crosses the open threshold ~1.3 ms into
//!    that, leaving ~51 ms of pre-key still to come.
//! 2. **Pre-roll.** Opening replays the buffered samples that preceded the
//!    decision, so the lowpass and the timing loop are fed from *before* the
//!    gate opened rather than from a step.
//! 3. **Hysteresis + hangover.** Closing needs a lower level than opening did,
//!    and then [`HANGOVER`] further samples below it, so a fade cannot chop a
//!    burst into pieces.
//! 4. **The deframer interlock.** While the deframer is mid-frame the caller
//!    holds the gate open outright (see [`Squelch::hold_open`]), so even a
//!    signal that genuinely collapses is followed to the end of the block.
//!
//! ## The one shape this gate cannot hold open
//!
//! A signal that never stops will eventually be learned as the floor. The
//! creep in [`FLOOR_CREEP`] is deliberately slow, but it is not zero, so a
//! *continuous* carrier closes the gate after roughly seven seconds.
//!
//! That is out of reach for ACARS by construction: the longest legal block is
//! ~250 characters, ~0.83 s on air, an order of magnitude short of it — and
//! the deframer interlock holds the gate open across a block regardless. It is
//! recorded here because it is exactly the property that makes this module
//! wrong for other modes. STD-C's NCS common channels *are* continuous
//! carriers; a gate with this behaviour in front of one would drop carrier
//! lock a few seconds after startup and never recover. See the per-mode table
//! in the PR description: the presence test does not generalise.

/// Samples of channel history kept so the chain can be fed from *before* the
/// gate opened. The lowpass needs its 121 taps primed and the timing loop
/// wants a lead-in; 512 samples (21 ms at the 24 kHz channel rate) covers both
/// with room to spare, and is paid only once per burst.
///
/// **This is insurance, not a measured necessity — do not cite it as tested.**
/// Nothing in the suite currently detects its removal: setting it to 0 leaves
/// every test passing and the sensitivity A/B unchanged. That was checked
/// deliberately, including the case it exists for — truncating the pre-key to
/// 4 of its 128 bits, where a missing lead-in should hurt most — and 0 and 512
/// were still indistinguishable there.
///
/// The reason is structural: the gate opens ~1.3 ms into a ~53 ms all-ones
/// pre-key, so the demod already receives ~51 ms of constant carrier before
/// any frame content. The pre-roll re-supplies lead-in the waveform provides
/// anyway. It is kept because it costs ~512 samples per burst (nothing at a
/// realistic duty cycle) and synthetic bursts from `modulate.rs` cannot
/// represent a real capture where the pre-key is lost to a collision or a
/// fade. If a future change needs to justify this code, it must first build a
/// case that fails without it.
///
/// Sized to exceed one channel-rate chunk. The decode loop hands the demod
/// `READ_CHUNK` (65 536) wideband samples at a time, which at 2.4 MS/s is 655
/// samples of channel rate; the deframer interlock is only re-evaluated at
/// those boundaries, so if the gate ever does close wrongly mid-burst the
/// replay has to be able to cover a whole chunk to recover it. 512 was smaller
/// than that and so could not, which made it the one size that was neither
/// useful nor honest.
const PREROLL: usize = 2_048;
/// Ring capacity. One more slot than [`PREROLL`] so a full-length pre-roll can
/// be taken from strictly *before* the sample being decided.
const RING: usize = PREROLL + 1;
/// Samples the gate stays open after the level falls below the close
/// threshold. 100 ms — comfortably longer than the ~27 ms chunk at which the
/// deframer interlock is re-evaluated, and short enough that the tail costs
/// little duty cycle (30 msg/min adds ~5 percentage points of open time).
const HANGOVER: usize = 2_400;
/// Samples averaged into the initial noise floor before the gate is trusted.
/// The floor is seeded from an arithmetic mean rather than a single sample:
/// envelope power is exponentially distributed, so one draw is a terrible
/// estimator (it can land a factor of ten either way), and a floor seeded far
/// too low leaves the gate stuck open — safe, but paying full CPU forever.
/// Over 256 samples the mean is within ~6% of the truth, and the 10.7 ms cost
/// of holding the gate open to collect them is irrelevant.
///
/// Note for anyone writing a test against this module: the gate is held OPEN
/// for these samples and then for a full [`HANGOVER`] after them, so a decode
/// test whose lead-in silence is shorter than ~2700 samples never exercises
/// the gate at all. The sensitivity harness originally had a 400-sample
/// lead-in and consequently scored a deliberately-deaf gate as a clean pass.
/// Give any such test at least a second of lead-in.
const SEED: usize = 256;
/// How long the floor seed will wait for samples carrying energy before giving
/// up and accepting whatever it has. Bounds the gate-held-open period on a
/// channel that is digitally silent from the start: without it, a source that
/// never produces a nonzero sample would never finish seeding. One second.
/// (On such a channel the level is zero too, so the gate simply reads closed.)
const SEED_TIMEOUT: usize = 24_000;
/// Level must exceed the floor by this factor to open (2.0 dB).
///
/// The margin is small on purpose. The weakest bursts that still decode sit
/// only a few dB above the floor, so a threshold chosen for a
/// comfortable-looking dB figure would gate away exactly the marginal frames
/// the demod works hardest to keep. It is safe to sit this close only because
/// the test runs on the *smoothed* level, not the instantaneous envelope: an
/// EMA at α = 0.005 has a standard deviation of ~5% of its mean on noise, so
/// 1.6× is ~12 sigma out and a false open is a non-event. Testing
/// instantaneous power here would instead open on ~20% of pure-noise samples.
const OPEN_RATIO: f32 = 1.6;
/// Level must fall below this multiple of the floor before the hangover starts
/// (0.8 dB). Lower than [`OPEN_RATIO`] — that gap is the hysteresis.
const CLOSE_RATIO: f32 = 1.2;
/// Smoothing for the detection level. Same time constant the demod uses for
/// RSSI (tau ~= 8.3 ms at the 24 kHz channel rate) — fast enough to cross the
/// open threshold a few ms into a ~53 ms pre-key, slow enough that its
/// standard deviation on noise is ~5% of its mean.
const LEVEL_ALPHA: f32 = 0.005;
/// Noise-floor tracking rate while the channel reads idle (tau ~= 21 ms).
/// Symmetric, so the floor is an unbiased estimate of the idle level.
const FLOOR_TRACK: f32 = 0.002;
/// Noise-floor tracking rate while the channel does NOT read idle, i.e. while
/// something is transmitting. Not a freeze, because a hard freeze has no way
/// back from a floor that settled too low — in particular a floor of exactly
/// zero, which the previous multiplicative creep (`floor *= 1 + eps`) could
/// never escape, pinning the gate open forever and silently costing the entire
/// optimisation. Tracking toward the level additively always escapes.
///
/// Sized so a transmission cannot pull the floor up onto itself: at 1e-5 on
/// the 24 kHz channel rate a 250 ms burst closes only 6% of the gap and the
/// longest possible ACARS block (~0.83 s) only 18%, both far short of the
/// [`CLOSE_RATIO`] margin. A *continuous* carrier would eventually win — see
/// the note on sustained signals in the module docs.
const FLOOR_CREEP: f32 = 1.0e-5;

/// What the demod should do with the sample just offered to [`Squelch::step`].
pub(crate) enum Gate {
    /// Channel idle — skip the expensive chain for this sample.
    Closed,
    /// Channel busy — process this sample.
    Open,
    /// Opening edge: process the `n` buffered samples from
    /// [`Squelch::preroll`] first, then this sample.
    Opening(usize),
}

/// Per-channel envelope gate. One per [`crate::demod::MskDemod`].
pub(crate) struct Squelch {
    /// Pre-roll history, stored doubled so the live window is always one
    /// contiguous slice (the same trick `xng_dsp::Fir` uses for its delay
    /// line). The newest sample sits at `pos + RING - 1`.
    hist: Vec<f32>,
    /// Next write position, always `< RING`; the live window is
    /// `hist[pos .. pos + RING]`, oldest first.
    pos: usize,
    /// Smoothed DC-blocked power — the detection level. Private to the gate:
    /// the demod's own `level` is carrier-inclusive and is what RSSI reports,
    /// which is a different quantity and must stay that way.
    level: f32,
    /// Noise floor in the same units as [`Self::level`].
    floor: f32,
    /// Samples with actual energy folded into the floor seed, up to [`SEED`].
    seeded: usize,
    /// Samples spent waiting for those, capped by [`SEED_TIMEOUT`].
    seed_wait: usize,
    open: bool,
    /// Samples of open time remaining once the level has dropped below
    /// [`CLOSE_RATIO`]; refreshed whenever it rises back above it.
    hangover: usize,
    /// Consecutive closed samples, saturating at [`PREROLL`]. Doubles as the
    /// pre-roll length, which is what stops back-to-back bursts separated by
    /// less than [`PREROLL`] from replaying samples that were already
    /// processed on the way into the previous burst.
    closed_run: usize,
    /// Caller-driven override: hold open regardless of level.
    hold: bool,
}

impl Squelch {
    pub(crate) fn new() -> Self {
        Self {
            hist: vec![0.0; 2 * RING],
            pos: 0,
            level: 0.0,
            floor: 0.0,
            seeded: 0,
            seed_wait: 0,
            open: false,
            hangover: 0,
            closed_run: 0,
            hold: false,
        }
    }

    /// Force the gate open (or release it). The decoder sets this while its
    /// deframer is mid-frame so a block is never truncated, and it is also how
    /// a caller disables gating entirely: held open from the first sample, the
    /// demod sees exactly the stream it saw before this module existed.
    pub(crate) fn hold_open(&mut self, hold: bool) {
        self.hold = hold;
    }

    /// Offer one sample. `value` is the DC-blocked envelope sample — exactly
    /// what the demod feeds its mixer, and what gets buffered for pre-roll.
    ///
    /// Detection runs on the DC-blocked signal, **not** on raw envelope power.
    /// An earlier version used the carrier-inclusive power on the reasoning
    /// that the carrier is the largest thing distinguishing a burst from
    /// silence. That is true of thermal noise and false of everything else: a
    /// steady DC offset, LO leakage at the capture centre, or a co-channel
    /// carrier inflates the level and the floor *equally*, so the ratio test
    /// goes deaf and the burst is dropped — even though the demod's own DC
    /// blocker removes that interferer completely and decodes the burst fine
    /// when the gate is pinned open. Measured before the change, three bursts
    /// through the real decoder with a pedestal 9.5 dB above the burst: 0/3
    /// gated against 3/3 pinned open, at every amplitude tried.
    ///
    /// Detecting on the DC-blocked signal makes the gate's notion of "present"
    /// the same as the demod's, and costs nothing: it is *more* sensitive, not
    /// less, because blocking DC removes more from the silence floor than it
    /// removes from the burst. Measured ~3.6 dB better across amplitudes.
    pub(crate) fn step(&mut self, value: f32) -> Gate {
        // A non-finite sample must not reach the EMAs. `level` is never reset,
        // so one NaN would poison it permanently and every later comparison
        // against the floor would be false — the gate would latch shut for the
        // life of the process. cf32 input is a supported format, so this is
        // reachable from a malformed file, not just from hardware.
        if !value.is_finite() {
            return if self.open { Gate::Open } else { Gate::Closed };
        }

        self.hist[self.pos] = value;
        self.hist[self.pos + RING] = value;
        self.pos = if self.pos + 1 == RING { 0 } else { self.pos + 1 };

        let p = value * value;
        self.level += LEVEL_ALPHA * (p - self.level);
        let level = self.level;

        if self.seeded < SEED && self.seed_wait < SEED_TIMEOUT {
            // Still measuring the floor: hold open, and accumulate the mean.
            //
            // Only samples carrying energy count. Digital silence — a muted
            // source, a zero-padded IQ file, a driver's first buffer — is not
            // a measurement of the noise floor, and averaging it in yields a
            // floor of exactly zero that then takes seconds of creep to
            // escape. Skipping those samples means the estimate starts the
            // moment real data arrives and completes 256 samples later.
            self.seed_wait += 1;
            if p > 0.0 {
                self.seeded += 1;
                self.floor += (p - self.floor) / self.seeded as f32;
            }
            self.open = true;
            self.hangover = HANGOVER;
            return Gate::Open;
        }

        // The floor is what the channel reads when nothing is transmitting, so
        // it must not learn from a transmission — but it must never be unable
        // to learn at all, which is what a hard freeze gets wrong. Track the
        // smoothed level always, quickly while the channel reads idle and
        // ~200x slower while it does not. Tracking the *smoothed* level rather
        // than per-sample power matters: per-sample envelope power is
        // exponentially distributed, and an asymmetric tracker on it settles
        // roughly 10 dB below the true mean, which would hold the gate open on
        // pure noise and cost the entire saving.
        let idle = level < self.floor * CLOSE_RATIO;
        let alpha = if idle { FLOOR_TRACK } else { FLOOR_CREEP };
        self.floor += alpha * (level - self.floor);

        if self.hold {
            self.open = true;
            self.hangover = HANGOVER;
        } else if self.open {
            if level > self.floor * CLOSE_RATIO {
                self.hangover = HANGOVER;
            } else if self.hangover > 0 {
                self.hangover -= 1;
            } else {
                self.open = false;
            }
        } else if level > self.floor * OPEN_RATIO {
            self.open = true;
            self.hangover = HANGOVER;
        }

        if !self.open {
            if self.closed_run < PREROLL {
                self.closed_run += 1;
            }
            Gate::Closed
        } else if self.closed_run > 0 {
            let n = self.closed_run;
            self.closed_run = 0;
            Gate::Opening(n)
        } else {
            Gate::Open
        }
    }

    /// The `n` buffered samples immediately preceding the one just decided,
    /// oldest first. Only valid straight after a [`Gate::Opening`], and only
    /// for the `n` it reported.
    pub(crate) fn preroll(&self, n: usize) -> &[f32] {
        debug_assert!(n <= PREROLL);
        let newest = self.pos + RING - 1;
        &self.hist[newest - n..newest]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quiet and loud DC-blocked amplitudes, 6 dB apart in power.
    const QUIET: f32 = 0.1;
    const LOUD: f32 = 0.2;

    /// Drive the gate the way the demod does and report, per sample, whether
    /// it was open, plus the total number of samples the demod would have
    /// processed (pre-roll included) — which is what the CPU saving is.
    ///
    /// Also asserts the pre-roll contract on every opening edge: the replayed
    /// slice must be exactly the `n` inputs immediately preceding the sample
    /// that opened the gate, in order. That is checked against the input
    /// itself rather than against a reimplementation of the ring.
    fn run(amps: &[f32]) -> (Vec<bool>, usize) {
        let mut sq = Squelch::new();
        let mut open = Vec::with_capacity(amps.len());
        let mut processed = 0usize;
        for (i, &a) in amps.iter().enumerate() {
            match sq.step(a) {
                Gate::Closed => open.push(false),
                Gate::Open => {
                    processed += 1;
                    open.push(true);
                }
                Gate::Opening(n) => {
                    assert_eq!(sq.preroll(n), &amps[i - n..i], "pre-roll at {i} is wrong");
                    processed += n + 1;
                    open.push(true);
                }
            }
        }
        (open, processed)
    }

    /// Silence, one burst, silence. The gate must be shut over the bulk of the
    /// silence, open before the burst's own frame content could arrive, and
    /// shut again afterwards.
    #[test]
    fn opens_on_a_burst_after_long_silence_and_closes_after_it() {
        let mut a = vec![QUIET; 24_000];
        a.extend(vec![LOUD; 6_000]);
        a.extend(vec![QUIET; 24_000]);

        let (open, _) = run(&a);

        assert!(open[12_000..24_000].iter().all(|&o| !o), "gate open during silence");
        let opened = 24_000 + open[24_000..].iter().position(|&o| o).expect("never opened");
        // Well inside the 128-bit (1272-sample) pre-key.
        assert!(opened - 24_000 < 1_272, "took {} samples to open", opened - 24_000);
        assert!(open[opened..30_000].iter().all(|&o| o), "gate closed mid-burst");
        assert!(!open[a.len() - 1], "gate never closed after the burst");
    }

    /// The saving is the point.
    #[test]
    fn idle_channel_skips_almost_everything() {
        let mut a = vec![QUIET; 24_000 * 10];
        for s in a.iter_mut().skip(24_000).take(6_000) {
            *s = LOUD;
        }
        let (_, processed) = run(&a);
        let duty = processed as f64 / a.len() as f64;
        assert!(duty < 0.10, "processed {:.1}% of an idle channel", duty * 100.0);
    }

    /// Hysteresis plus hangover must ride through a fade far longer than any
    /// envelope null the modulation itself produces.
    #[test]
    fn hangover_rides_through_a_mid_burst_fade() {
        let mut a = vec![QUIET; 24_000];
        a.extend(vec![LOUD; 3_000]);
        a.extend(vec![QUIET; 1_200]); // 50 ms fade, shorter than HANGOVER
        a.extend(vec![LOUD; 3_000]);
        a.extend(vec![QUIET; 12_000]);

        let (open, _) = run(&a);
        assert!(open[25_000..31_200].iter().all(|&o| o), "fade split the burst");
    }

    /// Bursts closer together than the pre-roll window: every sample must be
    /// handed to the demod at most once, or the deframer sees duplicated bits.
    #[test]
    fn back_to_back_bursts_never_replay_processed_samples() {
        let mut a = vec![QUIET; 24_000];
        for _ in 0..3 {
            a.extend(vec![LOUD; 6_000]);
            a.extend(vec![QUIET; 3_000]);
        }
        a.extend(vec![QUIET; 12_000]);
        let (_, processed) = run(&a);
        assert!(processed <= a.len(), "processed {processed} of {} samples", a.len());
    }

    /// The deframer interlock outranks the level test outright.
    #[test]
    fn hold_open_overrides_the_level_test() {
        let mut sq = Squelch::new();
        for _ in 0..24_000 {
            sq.step(QUIET);
        }
        assert!(matches!(sq.step(QUIET), Gate::Closed), "should be shut on silence");
        sq.hold_open(true);
        for _ in 0..24_000 {
            assert!(!matches!(sq.step(QUIET), Gate::Closed), "held gate closed anyway");
        }
    }

    /// Held open from the first sample the gate is a no-op: every sample is
    /// passed exactly once, in order, with no pre-roll replay. This is what
    /// makes "gating disabled" identical to the pre-squelch demod rather than
    /// merely similar to it.
    #[test]
    fn held_open_passes_every_sample_exactly_once() {
        let mut sq = Squelch::new();
        sq.hold_open(true);
        for i in 0..50_000 {
            let a = if (24_000..30_000).contains(&i) { LOUD } else { QUIET };
            assert!(matches!(sq.step(a), Gate::Open), "sample {i} not passed");
        }
    }

    /// A floor of exactly zero must not be absorbing.
    ///
    /// Regression test. The floor used to recover by multiplication
    /// (`floor *= 1 + eps`), which cannot escape zero — so a source that
    /// starts with digital silence (a muted input, a zero-padded IQ file, a
    /// driver's first buffer) seeded the floor to 0, made `idle` false for
    /// every subsequent sample, and pinned the gate open **forever**. Silent:
    /// no error, no dropped frame, just the entire CPU saving quietly gone.
    /// Measured at the time: 100.00% gate duty against 1.11% expected.
    #[test]
    fn a_zero_floor_is_not_absorbing() {
        let mut a = vec![0.0f32; 1_024];
        a.extend(vec![QUIET; 24_000 * 15]);
        let (open, _) = run(&a);
        assert!(!open[a.len() - 1], "gate never recovered from a zero floor");
        // And it must actually be saving by the end, not merely flickering.
        let tail = &open[a.len() - 24_000..];
        let duty = tail.iter().filter(|&&o| o).count() as f64 / tail.len() as f64;
        assert!(duty < 0.05, "still {:.0}% open after recovery", duty * 100.0);
    }

    /// One non-finite sample must not permanently latch the gate.
    ///
    /// `level` is an EMA that is never reset, so letting a NaN into it makes
    /// every later `level > floor * RATIO` false and the channel goes deaf for
    /// the life of the process. cf32 is a supported input format, so this is
    /// reachable from a malformed file rather than only from hardware.
    #[test]
    fn a_non_finite_sample_does_not_latch_the_gate() {
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut sq = Squelch::new();
            for _ in 0..24_000 {
                sq.step(QUIET);
            }
            sq.step(poison);
            // A burst after the poison must still open the gate.
            let mut opened = false;
            for _ in 0..6_000 {
                if !matches!(sq.step(LOUD), Gate::Closed) {
                    opened = true;
                    break;
                }
            }
            assert!(opened, "gate latched shut after {poison}");
        }
    }

    /// A transmission must not be learned as the noise floor on any timescale
    /// ACARS can produce. The longest legal block is ~0.83 s on air.
    #[test]
    fn a_full_length_block_does_not_pull_the_floor_up_onto_itself() {
        let mut a = vec![QUIET; 24_000];
        a.extend(vec![LOUD; 20_000]); // ~0.83 s, the longest possible block
        let (open, _) = run(&a);
        assert!(
            open[25_000..44_000].iter().all(|&o| o),
            "floor learned the transmission and closed the gate on it"
        );
    }
}
