//! Write a synthetic navaid capture for tests and benchmarks.
//!
//! ```text
//! navaid_gen OUT.cu8 --kind vor --rate 2000000 --offset 50000 --seconds 30 \
//!     --cn0 45 --radial 123 --ident SAC [--format cf32] [--carrier-err 1500] \
//!     [--subcarrier-err 30] [--voice 0.3] [--dvor 0.05]
//! navaid_gen OUT.cu8 --kind loc --ddm 0.05 --ident ISAC [--two-freq 8000:-6:0.3]
//! navaid_gen OUT.cu8 --kind gs --ddm -0.08
//! ```
//!
//! `--two-freq SEP:DB:DDM` adds a clearance carrier SEP Hz away, DB below the
//! course carrier, with its own DDM (the course moves +SEP/2, the clearance
//! −SEP/2).

use std::io::Write;
use xng_mode_navaid::modulate::{
    to_cu8, CarrierSpec, Generator, IdentSpec, IlsParams, Scene, Signal, VorParams,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).expect("output file").clone();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let f = |k: &str, d: f64| get(k).map(|v| v.parse::<f64>().unwrap()).unwrap_or(d);
    let rate = f("--rate", 2_000_000.0);
    let offset = f("--offset", 0.0) + f("--carrier-err", 0.0);
    let seconds = f("--seconds", 30.0);
    let cn0 = get("--cn0").map(|v| v.parse::<f64>().unwrap());
    let kind = get("--kind").unwrap_or_else(|| "vor".into());
    let ident = get("--ident").map(|t| {
        let mut s = IdentSpec::standard(&t);
        s.tone_hz += f("--ident-err", 0.0);
        s.start_s = f("--ident-start", 0.5);
        s
    });
    let seed = f("--seed", 1.0) as u64;
    let format = get("--format").unwrap_or_else(|| "cu8".into());
    let mut carriers = Vec::new();
    match kind.as_str() {
        "vor" => {
            let v = VorParams {
                radial_deg: f("--radial", 0.0),
                subcarrier_hz: 9_960.0 + f("--subcarrier-err", 0.0),
                voice_depth: f("--voice", 0.0),
                dvor_asymmetry: f("--dvor", 0.0),
                ident,
                ..Default::default()
            };
            carriers.push(CarrierSpec {
                offset_hz: offset,
                amplitude: 0.25,
                phase_rad: 0.7,
                signal: Signal::Vor(v),
            });
        }
        "loc" | "gs" => {
            let mut p = if kind == "loc" {
                IlsParams::localizer(f("--ddm", 0.0))
            } else {
                IlsParams::glideslope(f("--ddm", 0.0))
            };
            p.ident = ident.clone();
            if let Some(tf) = get("--two-freq") {
                let v: Vec<f64> = tf.split(':').map(|x| x.parse().unwrap()).collect();
                let (sep, db, ddm2) = (v[0], v[1], v[2]);
                carriers.push(CarrierSpec {
                    offset_hz: offset + sep / 2.0,
                    amplitude: 0.25,
                    phase_rad: 0.7,
                    signal: Signal::Ils(p.clone()),
                });
                let mut c = if kind == "loc" {
                    IlsParams::localizer(ddm2)
                } else {
                    IlsParams::glideslope(ddm2)
                };
                c.ident = ident;
                carriers.push(CarrierSpec {
                    offset_hz: offset - sep / 2.0,
                    amplitude: 0.25 * 10f64.powf(db / 20.0),
                    phase_rad: 2.1,
                    signal: Signal::Ils(c),
                });
            } else {
                carriers.push(CarrierSpec {
                    offset_hz: offset,
                    amplitude: 0.25,
                    phase_rad: 0.7,
                    signal: Signal::Ils(p),
                });
            }
        }
        k => panic!("unknown kind {k}"),
    }
    // Like a receiver's gain setting: total RMS (carriers + wideband noise) at
    // about a quarter of full scale, so 8-bit samples neither clip nor bury
    // the noise in quantization.
    let power: f64 = carriers
        .iter()
        .map(|c| c.amplitude * c.amplitude)
        .sum::<f64>()
        + cn0
            .map(|c| carriers[0].amplitude.powi(2) / 10f64.powf(c / 10.0) * rate)
            .unwrap_or(0.0);
    let scale = (0.25 / power.sqrt()) as f32;
    let mut g = Generator::new(Scene {
        sample_rate: rate,
        carriers,
        cn0_dbhz: cn0,
        seed,
    });
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
    let total = (seconds * rate) as usize;
    let block = (rate as usize / 10).max(1);
    let mut done = 0;
    let mut bytes = Vec::new();
    while done < total {
        let n = block.min(total - done);
        let x = g.next_block(n);
        bytes.clear();
        if format == "cu8" {
            to_cu8(&x, scale, &mut bytes);
        } else {
            for c in &x {
                bytes.extend_from_slice(&(c.re * scale).to_le_bytes());
                bytes.extend_from_slice(&(c.im * scale).to_le_bytes());
            }
        }
        w.write_all(&bytes).unwrap();
        done += n;
    }
    w.flush().expect("flush the capture file");
}
