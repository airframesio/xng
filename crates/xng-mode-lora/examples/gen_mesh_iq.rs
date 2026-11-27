//! Write a synthetic capture (cf32, little endian) with Meshtastic or
//! MeshCore traffic, for trying the decoder without a radio.
//!
//!   cargo run -p xng-mode-lora --release --example gen_mesh_iq -- meshtastic mt.cf32
//!   xng decode mt.cf32 -m meshtastic -r 1000000 -c 906.875M --channels 906.875
//!
//! MeshCore: `meshcore mc.cf32`, then `-m meshcore -r 1000000 -c 910.525M --channels 910.525`.

use num_complex::Complex;
use std::io::Write;
use xng_mode_lora::coding::CodingParams;
use xng_mode_lora::meshcore;
use xng_mode_lora::meshtastic::{self, Header, BROADCAST, DEFAULT_KEY};
use xng_mode_lora::modulate::{modulate, TxParams};

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn g(&mut self) -> f64 {
        (-2.0 * self.f().max(1e-12).ln()).sqrt() * (2.0 * std::f64::consts::PI * self.f()).cos()
    }
}

fn pb_string(field: u8, s: &str, out: &mut Vec<u8>) {
    out.push((field << 3) | 2);
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let proto = args.get(1).map(String::as_str).unwrap_or("meshtastic");
    let path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| format!("{proto}.cf32"));
    let out_rate = 1_000_000.0;
    let (frames, tx): (Vec<Vec<u8>>, TxParams) = if proto == "meshcore" {
        let ch = meshcore::Channel::public();
        let frames = vec![
            meshcore::encode_advert(
                &[3u8; 32],
                1_764_200_000,
                2,
                "Cameron Park Rptr",
                Some((38.6688, -120.9872)),
            ),
            meshcore::encode_advert(
                &[5u8; 32],
                1_764_200_050,
                1,
                "KJ6ABC Pocket",
                Some((38.5816, -121.4944)),
            ),
            meshcore::encode_advert(
                &[9u8; 32],
                1_764_200_070,
                1,
                "Folsom Base",
                Some((38.6780, -121.1761)),
            ),
            meshcore::encode_group_text(
                &ch,
                1_764_200_100,
                "KJ6ABC Pocket",
                "Anyone on from Placerville?",
                &[0x3c],
            ),
            meshcore::encode_group_text(
                &ch,
                1_764_200_160,
                "Folsom Base",
                "Copy, 2 hops here",
                &[0x3c, 0x91],
            ),
        ];
        (
            frames,
            TxParams {
                coding: CodingParams {
                    sf: 7,
                    cr: 1,
                    ldro: false,
                },
                bw_hz: 62_500.0,
                preamble: 16,
                sync_word: 0x12,
            },
        )
    } else {
        let mut user = Vec::new();
        pb_string(1, "!c0ffee01", &mut user);
        pb_string(2, "Sierra Foothills Router", &mut user);
        pb_string(3, "SFR1", &mut user);
        user.extend([5 << 3, 43, 7 << 3, 2]); // HELTEC_V3, ROUTER
        let mut pos = vec![0x0d];
        pos.extend(386_688_000i32.to_le_bytes());
        pos.push(0x15);
        pos.extend((-1_209_872_000i32).to_le_bytes());
        pos.extend([0x18, 0xd4, 0x03]); // 468 m
        let dm = [0x08, 87, 0x15, 0x9a, 0x99, 0x81, 0x40]; // 87 %, 4.05 V
        let mut tel = vec![0x12, dm.len() as u8];
        tel.extend(dm);
        let h = |from: u32, id: u32| Header {
            to: BROADCAST,
            from,
            id,
            flags: 0x63,
            channel: 8,
            next_hop: 0,
            relay_node: from as u8,
        };
        let node = |id: &str, long: &str, short: &str, hw: u8, lat: f64, lon: f64| {
            let mut u = Vec::new();
            pb_string(1, id, &mut u);
            pb_string(2, long, &mut u);
            pb_string(3, short, &mut u);
            u.extend([5 << 3, hw]);
            let mut p = vec![0x0d];
            p.extend(((lat * 1e7) as i32).to_le_bytes());
            p.push(0x15);
            p.extend(((lon * 1e7) as i32).to_le_bytes());
            (u, p)
        };
        let (u2, p2) = node("!5a17e2b4", "Auburn Hilltop", "AUBN", 9, 38.8966, -121.0769); // RAK4631
        let (u3, p3) = node("!0b5e1a77", "Placerville Mobile", "PVM", 7, 38.7296, -120.7985); // T_ECHO
        let mut tel3 = vec![0x12, 7, 0x08, 64, 0x15];
        tel3.extend(3.86f32.to_le_bytes());
        let frames = vec![
            meshtastic::encode(&h(0xc0ffee01, 0x1001), 4, &user, &DEFAULT_KEY),
            meshtastic::encode(&h(0x5a17e2b4, 0x2002), 4, &u2, &DEFAULT_KEY),
            meshtastic::encode(&h(0x5a17e2b4, 0x2003), 3, &p2, &DEFAULT_KEY),
            meshtastic::encode(&h(0x0b5e1a77, 0x3001), 4, &u3, &DEFAULT_KEY),
            meshtastic::encode(&h(0x0b5e1a77, 0x3002), 3, &p3, &DEFAULT_KEY),
            meshtastic::encode(&h(0x0b5e1a77, 0x3003), 67, &tel3, &DEFAULT_KEY),
            meshtastic::encode(
                &h(0x0b5e1a77, 0x3004),
                1,
                b"Heading up 50, will check in from Pollock Pines",
                &DEFAULT_KEY,
            ),
            meshtastic::encode(&h(0xc0ffee01, 0x1002), 3, &pos, &DEFAULT_KEY),
            meshtastic::encode(
                &h(0x5a17e2b4, 0x2001),
                1,
                b"Good morning mesh, anyone near Auburn?",
                &DEFAULT_KEY,
            ),
            meshtastic::encode(&h(0xc0ffee01, 0x1003), 67, &tel, &DEFAULT_KEY),
        ];
        (
            frames,
            TxParams {
                coding: CodingParams {
                    sf: 11,
                    cr: 1,
                    ldro: false,
                },
                bw_hz: 250e3,
                preamble: 16,
                sync_word: 0x2B,
            },
        )
    };
    let os = (out_rate / tx.bw_hz).round() as usize;
    let mut rng = Rng(0xfeed_beef_1234_5678);
    let noise = 0.05;
    let mut iq: Vec<Complex<f32>> = Vec::new();
    let gap = |rng: &mut Rng, iq: &mut Vec<Complex<f32>>, n: usize| {
        for _ in 0..n {
            iq.push(Complex::new(
                (noise * rng.g()) as f32,
                (noise * rng.g()) as f32,
            ));
        }
    };
    gap(&mut rng, &mut iq, 200_000);
    for (i, f) in frames.iter().enumerate() {
        let cfo = [1800.0, -2500.0, 900.0, -400.0][i % 4];
        for s in modulate(f, &tx, os, cfo, cfo / 906e6 * 1e6) {
            iq.push(s * 0.2 + Complex::new((noise * rng.g()) as f32, (noise * rng.g()) as f32));
        }
        gap(&mut rng, &mut iq, 150_000 + i * 7_777);
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    for s in &iq {
        w.write_all(&s.re.to_le_bytes()).unwrap();
        w.write_all(&s.im.to_le_bytes()).unwrap();
    }
    w.flush().expect("write the capture");
    eprintln!(
        "wrote {} samples ({:.2} s at {out_rate} S/s) to {path}",
        iq.len(),
        iq.len() as f64 / out_rate
    );
}
