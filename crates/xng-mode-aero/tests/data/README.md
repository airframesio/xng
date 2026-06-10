# Off-air test fixtures

`600bps_offair_12s.i16` — the first 12 seconds of `samples/600bps_sample.ogg`
from the JAERO repository (https://github.com/jontio/JAERO, MIT license,
Copyright (c) Jonathan Olds), decoded to mono 48 kHz signed 16-bit
little-endian PCM. A real off-air Inmarsat Classic Aero recording; the
P-channel carrier of interest sits at ~1066 Hz in the audio band and the
window contains ACARS traffic from HL8217 (Asiana).

Regenerate with:

```
ffmpeg -i 600bps_sample.ogg -f s16le -ac 1 -ar 48000 -t 12 600bps_offair_12s.i16
```
