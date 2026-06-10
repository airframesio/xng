# Off-air test fixtures

`hfdl_21931khz_8s.i16` — an 8-second slice (offset 1.5 s) of the off-air
HFDL IQ recording `skip.land_2024-11-05T21_18_09Z_21931.00_iq.wav.zip`
from sigidwiki's HFDL page
(https://www.sigidwiki.com/wiki/High_Frequency_Data_Link_(HFDL)),
resampled to 24 kHz interleaved stereo (I/Q) signed 16-bit little-endian.
sigidwiki content is CC BY-SA — this fixture is test data under that
license, not under the repository's MIT/Apache terms.

Capture center = 21 931.0 kHz (the SSB carrier), i.e. carrier offset 0;
the audio subcarrier sits at +1440 Hz. The window contains one 300 bps
single-slot SPDU squitter from ground station 4 (Riverhead), confirmed
field-for-field against dumphfdl 1.7.0 (frame index 2397, offset 1,
system table version 52).

Regenerate with:

```
ffmpeg -i skip.land_2024-11-05T21_18_09Z_21931.00_iq.wav \
  -f s16le -ac 2 -ar 24000 -ss 1.5 -t 8 hfdl_21931khz_8s.i16
```
