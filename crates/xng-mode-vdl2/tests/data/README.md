# Off-air test fixtures

`vdl2_offair_6s.i16` — a 6-second slice (offset 20 s) of the off-air VDL
Mode 2 IQ recording `VDL-M2_IQ.zip` from sigidwiki's VDL-M2 page
(https://www.sigidwiki.com/wiki/VHF_Data_Link_-_Mode_2_(VDL-M2)),
resampled to 50 kHz interleaved stereo (I/Q) signed 16-bit little-endian,
**with Q negated** — the original capture has an inverted I/Q convention
(dumpvdl2 also decodes nothing until the spectrum is conjugated).
sigidwiki content is CC BY-SA — this fixture is test data under that
license, not under the repository's MIT/Apache terms.

The window contains a downlink ACARS from HB-IJW (an Amsterdam-area
recording; label B9, `/EHAM.TI2/...`) plus an AVLC RR supervisory frame,
both also decoded by dumpvdl2 2.6.0 from the same capture.

Regenerate with:

```
ffmpeg -i "VDL2 IQ.wav" -f f32le -ac 2 -ar 50000 vdl2_50k.f32
python3 -c "import numpy as np; d=np.fromfile('vdl2_50k.f32',dtype=np.float32).reshape(-1,2); d[:,1]*=-1; np.clip(d[20*50000:26*50000]*32767,-32768,32767).astype('<i2').tofile('vdl2_offair_6s.i16')"
```
