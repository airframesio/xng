//! IQ file replay source.

use crate::{IqSource, SdrError};
use num_complex::Complex;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// On-disk IQ sample formats (interleaved I/Q).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IqFormat {
    /// 32-bit float I/Q (`.cf32`, `.fc32`, GNU Radio default).
    Cf32,
    /// 16-bit signed I/Q (`.cs16`, `.sc16`).
    Cs16,
    /// 8-bit signed I/Q (`.cs8`, HackRF).
    Cs8,
    /// 8-bit unsigned offset-127.5 I/Q (`.cu8`, RTL-SDR).
    Cu8,
}

impl IqFormat {
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            IqFormat::Cf32 => 8,
            IqFormat::Cs16 => 4,
            IqFormat::Cs8 | IqFormat::Cu8 => 2,
        }
    }

    /// Guess the format from a file extension.
    pub fn from_extension(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "cf32" | "fc32" | "raw" | "iq" => Some(IqFormat::Cf32),
            "cs16" | "sc16" => Some(IqFormat::Cs16),
            "cs8" | "sc8" => Some(IqFormat::Cs8),
            "cu8" => Some(IqFormat::Cu8),
            _ => None,
        }
    }
}

impl std::str::FromStr for IqFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cf32" | "fc32" => Ok(IqFormat::Cf32),
            "cs16" | "sc16" => Ok(IqFormat::Cs16),
            "cs8" | "sc8" => Ok(IqFormat::Cs8),
            "cu8" => Ok(IqFormat::Cu8),
            other => Err(format!("unknown IQ format: {other}")),
        }
    }
}

/// Replays a recorded IQ file (or a piped IQ stream) as an [`IqSource`].
pub struct FileIqSource {
    reader: BufReader<Box<dyn Read + Send>>,
    format: IqFormat,
    sample_rate: f64,
    center_freq_hz: u64,
    byte_buf: Vec<u8>,
}

impl FileIqSource {
    pub fn open(
        path: impl AsRef<Path>,
        format: IqFormat,
        sample_rate: f64,
        center_freq_hz: u64,
    ) -> Result<Self, SdrError> {
        let file = File::open(path)?;
        Ok(Self::from_reader(Box::new(file), format, sample_rate, center_freq_hz))
    }

    /// Read IQ samples from standard input (ECO-15) — interop with KiwiSDR
    /// `--kiwi-wav` extraction, GNU Radio, and `rx_sdr`/`soapy` pipes. The
    /// format cannot be guessed from a stdin "extension", so the caller must
    /// pass it explicitly.
    pub fn open_stdin(
        format: IqFormat,
        sample_rate: f64,
        center_freq_hz: u64,
    ) -> Self {
        Self::from_reader(Box::new(std::io::stdin()), format, sample_rate, center_freq_hz)
    }

    fn from_reader(
        rd: Box<dyn Read + Send>,
        format: IqFormat,
        sample_rate: f64,
        center_freq_hz: u64,
    ) -> Self {
        Self {
            reader: BufReader::with_capacity(1 << 20, rd),
            format,
            sample_rate,
            center_freq_hz,
            byte_buf: Vec::new(),
        }
    }
}

impl IqSource for FileIqSource {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq_hz(&self) -> u64 {
        self.center_freq_hz
    }

    fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
        let bps = self.format.bytes_per_sample();
        let want = buf.len() * bps;
        self.byte_buf.resize(want, 0);

        // Fill as much as possible (BufReader::read may return short counts).
        let mut filled = 0;
        while filled < want {
            match self.reader.read(&mut self.byte_buf[filled..want])? {
                0 => break,
                n => filled += n,
            }
        }
        let samples = filled / bps;
        if samples == 0 {
            return Err(SdrError::EndOfStream);
        }

        let bytes = &self.byte_buf[..samples * bps];
        match self.format {
            IqFormat::Cf32 => {
                for (i, chunk) in bytes.chunks_exact(8).enumerate() {
                    let re = f32::from_le_bytes(chunk[0..4].try_into().unwrap());
                    let im = f32::from_le_bytes(chunk[4..8].try_into().unwrap());
                    buf[i] = Complex::new(re, im);
                }
            }
            IqFormat::Cs16 => {
                for (i, chunk) in bytes.chunks_exact(4).enumerate() {
                    let re = i16::from_le_bytes(chunk[0..2].try_into().unwrap());
                    let im = i16::from_le_bytes(chunk[2..4].try_into().unwrap());
                    buf[i] = Complex::new(re as f32 / 32768.0, im as f32 / 32768.0);
                }
            }
            IqFormat::Cs8 => {
                for (i, chunk) in bytes.chunks_exact(2).enumerate() {
                    buf[i] = Complex::new(chunk[0] as i8 as f32 / 128.0, chunk[1] as i8 as f32 / 128.0);
                }
            }
            IqFormat::Cu8 => {
                for (i, chunk) in bytes.chunks_exact(2).enumerate() {
                    buf[i] = Complex::new(
                        (chunk[0] as f32 - 127.5) / 127.5,
                        (chunk[1] as f32 - 127.5) / 127.5,
                    );
                }
            }
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_cf32() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let samples = [(1.0f32, -0.5f32), (0.25, 0.75)];
        for (re, im) in samples {
            tmp.write_all(&re.to_le_bytes()).unwrap();
            tmp.write_all(&im.to_le_bytes()).unwrap();
        }
        tmp.flush().unwrap();

        let mut src = FileIqSource::open(tmp.path(), IqFormat::Cf32, 48000.0, 0).unwrap();
        let mut buf = vec![Complex::new(0.0, 0.0); 16];
        let n = src.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(buf[0], Complex::new(1.0, -0.5));
        assert_eq!(buf[1], Complex::new(0.25, 0.75));
        assert!(matches!(src.read(&mut buf), Err(SdrError::EndOfStream)));
    }

    #[test]
    fn reads_cu8_rtlsdr_style() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&[255u8, 0u8]).unwrap(); // ~(+1, -1)
        tmp.flush().unwrap();

        let mut src = FileIqSource::open(tmp.path(), IqFormat::Cu8, 2_400_000.0, 0).unwrap();
        let mut buf = vec![Complex::new(0.0, 0.0); 4];
        assert_eq!(src.read(&mut buf).unwrap(), 1);
        assert!((buf[0].re - 1.0).abs() < 0.01);
        assert!((buf[0].im + 1.0).abs() < 0.01);
    }

    #[test]
    fn format_from_extension() {
        assert_eq!(IqFormat::from_extension(Path::new("x.cf32")), Some(IqFormat::Cf32));
        assert_eq!(IqFormat::from_extension(Path::new("x.cu8")), Some(IqFormat::Cu8));
        assert_eq!(IqFormat::from_extension(Path::new("x.wav")), None);
    }
}
