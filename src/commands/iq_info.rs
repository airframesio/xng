//! `xng iq-info` — inspect a recorded IQ file: duration, power, and the
//! strongest spectral peaks (Welch-averaged periodogram).

use num_complex::Complex;
use rustfft::FftPlanner;
use std::path::Path;
use xng_sdr::{FileIqSource, IqFormat, IqSource, SdrError};

pub fn run(
    file: &Path,
    sample_rate: f64,
    format: Option<&str>,
    center_freq_hz: u64,
    fft_size: usize,
) -> anyhow::Result<()> {
    let format = match format {
        Some(f) => f.parse().map_err(|e: String| anyhow::anyhow!(e))?,
        None => IqFormat::from_extension(file).ok_or_else(|| {
            anyhow::anyhow!("cannot guess IQ format from extension; pass --format (cf32|cs16|cs8|cu8)")
        })?,
    };
    anyhow::ensure!(fft_size.is_power_of_two() && fft_size >= 64, "--fft-size must be a power of two >= 64");

    let mut src = FileIqSource::open(file, format, sample_rate, center_freq_hz)?;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(fft_size);
    let window: Vec<f32> = xng_dsp::window::hamming(fft_size).iter().map(|w| *w as f32).collect();

    let mut buf = vec![Complex::new(0.0f32, 0.0f32); fft_size];
    let mut psd = vec![0.0f64; fft_size];
    let mut total_power = 0.0f64;
    let mut total_samples: u64 = 0;
    let mut segments: u64 = 0;

    loop {
        // Read one full FFT segment; a short final segment is discarded.
        let mut filled = 0;
        let exhausted = loop {
            match src.read(&mut buf[filled..]) {
                Ok(n) => {
                    filled += n;
                    if filled == fft_size {
                        break false;
                    }
                }
                Err(SdrError::EndOfStream) => break true,
                Err(e) => return Err(e.into()),
            }
        };
        if exhausted {
            break;
        }
        total_samples += fft_size as u64;
        total_power += buf.iter().map(|s| s.norm_sqr() as f64).sum::<f64>();
        for (s, w) in buf.iter_mut().zip(&window) {
            *s *= *w;
        }
        fft.process(&mut buf);
        for (acc, s) in psd.iter_mut().zip(&buf) {
            *acc += s.norm_sqr() as f64;
        }
        segments += 1;
    }
    anyhow::ensure!(segments > 0, "file too short for even one FFT segment of {fft_size} samples");

    let duration = total_samples as f64 / sample_rate;
    let mean_power_db = 10.0 * (total_power / total_samples as f64).log10();

    println!("file:        {}", file.display());
    println!("format:      {format:?}, {sample_rate} S/s");
    println!("samples:     {total_samples} ({duration:.3} s, {segments} segments of {fft_size})");
    println!("mean power:  {mean_power_db:.1} dBFS");

    // Peak search on the averaged PSD, simple local-max with guard bins.
    let norm = (segments * fft_size as u64) as f64 * fft_size as f64;
    let bins: Vec<f64> = psd.iter().map(|p| 10.0 * (p / norm).max(1e-30).log10()).collect();
    let median = {
        let mut sorted = bins.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted[sorted.len() / 2]
    };
    let mut peaks: Vec<(usize, f64)> = (0..fft_size)
        .filter(|&i| {
            let prev = bins[(i + fft_size - 1) % fft_size];
            let next = bins[(i + 1) % fft_size];
            bins[i] > prev && bins[i] >= next && bins[i] > median + 10.0
        })
        .map(|i| (i, bins[i]))
        .collect();
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("noise floor: {median:.1} dBFS/bin (median)");
    if peaks.is_empty() {
        println!("peaks:       none > 10 dB above the noise floor");
    } else {
        println!("peaks (top {}):", peaks.len().min(10));
        for (bin, level) in peaks.iter().take(10) {
            let offset = if *bin <= fft_size / 2 {
                *bin as f64 * sample_rate / fft_size as f64
            } else {
                (*bin as f64 - fft_size as f64) * sample_rate / fft_size as f64
            };
            if center_freq_hz > 0 {
                let abs = center_freq_hz as f64 + offset;
                println!("  {:>12.3} kHz offset  ({:.6} MHz)  {:>6.1} dBFS", offset / 1e3, abs / 1e6, level);
            } else {
                println!("  {:>12.3} kHz offset  {:>6.1} dBFS", offset / 1e3, level);
            }
        }
    }
    Ok(())
}
