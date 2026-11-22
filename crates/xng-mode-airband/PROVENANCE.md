# Provenance — xng-mode-airband

Clean-room airband voice receiver (AM and NFM channels, squelch, scanner
selection, Ogg/WAV framing). No code was copied from any other receiver.

## Fact references (no code)

- **RTLSDR-Airband** (GPL-2.0): used only to list the features people expect
  from an airband receiver: many channels from one capture, scan mode, mixers,
  carrier squelch with hang time, CTCSS, a notch filter, AFC, NFM, per
  transmission and continuous recordings, Icecast and UDP outputs. Nothing
  from its source (DSP, squelch, output code) was ported. xng's front end
  reuses xng-dsp's channelizer, and the squelch, AGC, AFC and scan logic were
  written from first principles for this crate.
- **airband-hub** (GPL-2.0-or-later): read for how a hub gathers streams from
  several receivers; it informed which metadata each transmission carries. No
  code was used.

## Standards and public facts

- **Airband channelling**: 25 kHz and 8.33 kHz channel spacing (ICAO Annex 10
  Vol V; EUROCONTROL 8.33 kHz guidance), double-sideband AM voice, audio
  roughly 300 Hz to 3.4 kHz.
- **CTCSS**: the EIA/TIA-603 tone list (67.0 to 254.1 Hz). Tone detection is a
  Goertzel bank written for this crate.
- **NFM de-emphasis**: the usual first-order de-emphasis (time constant
  configurable, default 200 us for land-mobile style audio).
- **Audio biquads**: the RBJ "Audio EQ Cookbook" formulas (high-pass, low-pass,
  notch), which are public-domain reference formulas.
- **Ogg / Opus framing**: RFC 3533 (Ogg pages and the page CRC), RFC 7845
  (OpusHead / OpusTags headers, 48 kHz granule positions, pre-skip).
- **WAV**: the RIFF/WAVE PCM header layout.

## Validation

- Unit tests cover the squelch (auto floor tracking, manual threshold,
  hysteresis, hang, open delay), CTCSS gating, AFC pull-in, adjacent-channel
  rejection (a strong carrier 25 kHz away, and 8.33 kHz neighbours through the
  multichannel front end), the key-up fade-in, scan priority, hold,
  preemption, pin and lockout, and Ogg/WAV framing.
- End to end, synthetic AM IQ (four speech transmissions over four channels,
  carrier offsets up to 300 Hz, AWGN) decodes into recordings whose waveform
  correlates 0.77 to 0.88 with the source speech after the 150 Hz to 3.4 kHz
  voice band-limiting. No off-air airband capture is vendored yet.
