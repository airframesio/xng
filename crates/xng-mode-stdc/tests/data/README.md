# Off-air test fixtures

`stdc_egc_14s.i16` — a 14-second slice (offset 4 s) of the off-air
Inmarsat-C TDM/EGC IQ recording `Inmarsat-C_TDM_EGC_IQ.zip` from
sigidwiki's Inmarsat-C TDM page
(https://www.sigidwiki.com/wiki/Inmarsat-C_TDM), resampled to 24 kHz
interleaved stereo (I/Q) signed 16-bit little-endian. sigidwiki content
is CC BY-SA — this fixture is test data under that license, not under
the repository's MIT/Apache terms.

The TDM carrier sits at +216 Hz in the capture (AOR-E region). The
window contains one full frame: bulletin board (frame number 5987,
network version 109), a logical-channel announcement (LES 104 AOR-E),
and confirmations.

Regenerate with:

```
ffmpeg -i "Inmarsat-C TDM EGC.wav" -f s16le -ac 2 -ar 24000 -ss 4 -t 14 stdc_egc_14s.i16
```
