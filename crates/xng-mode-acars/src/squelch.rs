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
const PREROLL: usize = 512;
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
/// Level must exceed the floor by this factor to open (2.0 dB).
///
/// The margin is small on purpose. The weakest bursts that still decode are
/// only a few dB above the floor — the sensitivity sweep still recovers frames
/// at σ = 0.25, where the burst carries just ~3.7× the noise power — so a
/// threshold chosen for a comfortable-looking dB figure would gate away
/// exactly the marginal frames the demod works hardest to keep. It is safe to
/// sit this close to the floor only because the test runs on the *smoothed*
/// level, not the instantaneous envelope: an EMA at α = 0.005 has a standard
/// deviation of ~5% of its mean on noise, so 1.6× is ~10 sigma away and a
/// false open is a non-event. Testing instantaneous power here instead would
/// open on 13% of pure-noise samples.
const OPEN_RATIO: f32 = 1.6;
/// Level must fall below this multiple of the floor before the hangover starts
/// (0.8 dB). Lower than [`OPEN_RATIO`] — that gap is the hysteresis.
const CLOSE_RATIO: f32 = 1.2;
/// Noise-floor EMA factor, applied only while the channel reads idle.
const FLOOR_ALPHA: f32 = 0.002;
/// A sample this far above the floor is an impulse, not the floor, and is kept
/// out of the EMA even when the channel otherwise reads idle.
const FLOOR_SPIKE_GATE: f32 = 8.0;
/// Per-non-idle-sample multiplicative up-creep, so a floor that settled too
/// low can still recover. Sized like the demod's own `NOISE_RECOVER`: at 2e-5
/// on the 24 kHz channel rate a ~250 ms burst lifts the floor ~0.5 dB
/// (harmless), while a genuinely stuck floor re-converges over a few seconds.
const FLOOR_RECOVER: f32 = 2.0e-5;

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
    /// Envelope-power noise floor, tracked over idle samples only.
    floor: f32,
    /// Samples folded into the floor seed so far, up to [`SEED`].
    seeded: usize,
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
            floor: 0.0,
            seeded: 0,
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

    /// Offer one sample. `p` is its instantaneous envelope power, `level` the
    /// demod's smoothed envelope power (reused rather than recomputed — it is
    /// already maintained for RSSI and has exactly the right time constant),
    /// and `value` the DC-blocked envelope sample to buffer for pre-roll.
    ///
    /// Detection deliberately runs on the *carrier* power `p`, not on the
    /// DC-blocked `value`: the DC blocker exists to strip the carrier and
    /// leave the audio, which throws away the very thing that most cleanly
    /// distinguishes a burst from silence.
    pub(crate) fn step(&mut self, p: f32, level: f32, value: f32) -> Gate {
        self.hist[self.pos] = value;
        self.hist[self.pos + RING] = value;
        self.pos = if self.pos + 1 == RING { 0 } else { self.pos + 1 };

        if self.seeded < SEED {
            // Still measuring the floor: hold open, and accumulate the mean.
            self.seeded += 1;
            self.floor += (p - self.floor) / self.seeded as f32;
            self.open = true;
            self.hangover = HANGOVER;
            return Gate::Open;
        }

        // The floor is what the channel reads when nothing is transmitting, so
        // it must not be allowed to learn from a transmission. Freezing it on
        // "not idle" rather than on a fixed spike threshold matters for weak
        // bursts specifically: a burst only a few dB up would otherwise sit
        // below any sane spike gate and drag the floor towards itself, closing
        // the squelch part-way through its own signal.
        let idle = level < self.floor * CLOSE_RATIO;
        if idle {
            if p < self.floor * FLOOR_SPIKE_GATE {
                self.floor += FLOOR_ALPHA * (p - self.floor);
            }
        } else {
            self.floor *= 1.0 + FLOOR_RECOVER;
        }

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

    /// Drive the gate the way the demod does, with a level EMA matching the
    /// demod's, over a power sequence. Returns the per-sample open decision
    /// and the total number of samples the demod would have processed
    /// (pre-roll included), which is what the CPU saving is measured in.
    fn run(powers: &[f32]) -> (Vec<bool>, usize) {
        let mut sq = Squelch::new();
        let mut level = 0.0f32;
        let mut open = Vec::with_capacity(powers.len());
        let mut processed = 0usize;
        for (i, &p) in powers.iter().enumerate() {
            level += 0.005 * (p - level);
            match sq.step(p, level, i as f32) {
                Gate::Closed => open.push(false),
                Gate::Open => {
                    processed += 1;
                    open.push(true);
                }
                Gate::Opening(n) => {
                    // The replayed samples must be the ones that immediately
                    // preceded this one, in order — that is the whole point of
                    // the pre-roll, so check it rather than assuming it.
                    let pre = sq.preroll(n);
                    for (k, &v) in pre.iter().enumerate() {
                        assert_eq!(v as usize, i - n + k, "pre-roll out of order");
                    }
                    processed += n + 1;
                    open.push(true);
                }
            }
        }
        (open, processed)
    }

    /// Silence, one burst, silence. The gate must be shut over the bulk of the
    /// silence, must be open before the burst's own samples arrive, and must
    /// shut again afterwards.
    #[test]
    fn opens_on_a_burst_after_long_silence_and_closes_after_it() {
        let (quiet, loud) = (0.01f32, 0.04f32); // 6 dB — a weak burst
        let mut p = vec![quiet; 24_000]; // 1 s of silence
        p.extend(vec![loud; 6_000]); // 250 ms burst
        p.extend(vec![quiet; 24_000]);

        let (open, _) = run(&p);

        // Shut through the back half of the lead-in silence.
        assert!(open[12_000..24_000].iter().all(|&o| !o), "gate open during silence");
        // Open well inside the 53 ms (1272-sample) pre-key.
        let opened = 24_000 + open[24_000..].iter().position(|&o| o).expect("never opened");
        assert!(opened - 24_000 < 1_272, "took {} samples to open", opened - 24_000);
        // Continuously open for the whole burst.
        assert!(open[opened..30_000].iter().all(|&o| o), "gate closed mid-burst");
        // Shut again once the hangover expires.
        assert!(!open[p.len() - 1], "gate never closed after the burst");
    }

    /// The saving is the point: on a channel that is quiet apart from one
    /// burst, the overwhelming majority of samples must never reach the chain.
    #[test]
    fn idle_channel_skips_almost_everything() {
        let mut p = vec![0.01f32; 24_000 * 10];
        for s in p.iter_mut().skip(24_000).take(6_000) {
            *s = 0.04;
        }
        let (_, processed) = run(&p);
        let duty = processed as f64 / p.len() as f64;
        // 250 ms burst in 10 s is 2.5%; seed, pre-roll and hangover add a
        // little. Anything near 1.0 means the gate is not gating.
        assert!(duty < 0.10, "processed {:.1}% of an idle channel", duty * 100.0);
    }

    /// A dip in the middle of a burst must not split it: hysteresis plus
    /// hangover has to ride through a fade far longer than any envelope null
    /// the modulation itself produces.
    #[test]
    fn hangover_rides_through_a_mid_burst_fade() {
        let (quiet, loud) = (0.01f32, 0.04f32);
        let mut p = vec![quiet; 24_000];
        p.extend(vec![loud; 3_000]);
        p.extend(vec![quiet; 1_200]); // 50 ms fade to the noise floor
        p.extend(vec![loud; 3_000]);
        p.extend(vec![quiet; 12_000]);

        let (open, _) = run(&p);
        assert!(open[25_000..31_200].iter().all(|&o| o), "fade split the burst");
    }

    /// Back-to-back bursts closer together than the pre-roll window. Every
    /// sample must be handed to the demod at most once — a pre-roll that
    /// reached back past the previous burst would replay samples that were
    /// already processed, feeding the deframer a duplicated bit stream.
    #[test]
    fn back_to_back_bursts_never_replay_processed_samples() {
        let (quiet, loud) = (0.01f32, 0.04f32);
        let mut p = vec![quiet; 24_000];
        for _ in 0..3 {
            p.extend(vec![loud; 6_000]);
            p.extend(vec![quiet; 3_000]); // shorter than HANGOVER + decay
        }
        p.extend(vec![quiet; 12_000]);

        // `run` asserts pre-roll ordering; here we additionally check that the
        // total processed count cannot exceed the number of samples that exist.
        let (_, processed) = run(&p);
        assert!(processed <= p.len(), "processed {processed} of {} samples", p.len());
    }

    /// The deframer interlock outranks the level test outright.
    #[test]
    fn hold_open_overrides_the_level_test() {
        let mut sq = Squelch::new();
        let mut level = 0.0f32;
        // Settle the floor on silence first.
        for i in 0..24_000 {
            level += 0.005 * (0.01 - level);
            sq.step(0.01, level, i as f32);
        }
        assert!(matches!(sq.step(0.01, level, 0.0), Gate::Closed), "should be shut on silence");
        sq.hold_open(true);
        for _ in 0..24_000 {
            assert!(
                !matches!(sq.step(0.01, level, 0.0), Gate::Closed),
                "held gate closed anyway"
            );
        }
    }

    /// Held open from the first sample, the gate is a no-op: every sample is
    /// passed exactly once, in order, with no pre-roll replay. This is what
    /// makes "gating disabled" identical to the pre-squelch demod rather than
    /// merely similar to it.
    #[test]
    fn held_open_passes_every_sample_exactly_once() {
        let mut sq = Squelch::new();
        sq.hold_open(true);
        let mut level = 0.0f32;
        for i in 0..50_000 {
            let p = if (24_000..30_000).contains(&i) { 0.04 } else { 0.01 };
            level += 0.005 * (p - level);
            assert!(matches!(sq.step(p, level, i as f32), Gate::Open), "sample {i} not passed");
        }
    }
}
