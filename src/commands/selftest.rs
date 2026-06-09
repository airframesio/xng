//! `xng selftest` — prove the M0 plumbing end-to-end without hardware:
//! DSP sanity (tone through the polyphase channelizer) and bus → outputs
//! (synthetic ACARS message to console and optional JSONL).

use crate::bus::MessageBus;
use crate::outputs::console::{self, ConsoleFormat};
use crate::outputs::jsonl;
use chrono::Utc;
use num_complex::Complex;
use std::path::Path;
use xng_dsp::PfbChannelizer;
use xng_types::{
    AcarsCore, AppInfo, DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality,
    StationIdentity,
};

fn dsp_check() -> anyhow::Result<()> {
    let nch = 8;
    let fs = 2_000_000.0;
    let mut pfb = PfbChannelizer::new(nch, 12);
    let target = 3;
    let f = pfb.channel_offset_hz(target, fs);
    let input: Vec<Complex<f32>> = (0..nch * 256)
        .map(|i| {
            let ph = std::f64::consts::TAU * f * i as f64 / fs;
            Complex::new(ph.cos() as f32, ph.sin() as f32)
        })
        .collect();
    let mut out = vec![Vec::new(); nch];
    pfb.process(&input, &mut out);
    let power: Vec<f32> = out
        .iter()
        .map(|c| c[24..].iter().map(|s| s.norm_sqr()).sum::<f32>() / (c.len() - 24) as f32)
        .collect();
    let best = power
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    anyhow::ensure!(
        best == target,
        "channelizer check failed: tone at {f} Hz landed in channel {best}, expected {target}"
    );
    println!("[ok] channelizer: {:.0} kHz tone → channel {target} of {nch} (2 MS/s → {} kS/s/ch)", f / 1e3, fs / nch as f64 / 1e3);
    Ok(())
}

fn synthetic_message(seq: usize) -> Message {
    Message {
        mode: Mode::AcarsPoa,
        timestamp: Utc::now(),
        frequency_hz: 131_550_000,
        signal: SignalQuality { rssi_db: Some(-18.4), snr_db: Some(14.2), ..Default::default() },
        decode: DecodeQuality { crc_ok: true, ..Default::default() },
        body: MessageBody::Acars(AcarsCore {
            mode: '2',
            tail: Some("N471XG".into()),
            label: "Q0".into(),
            block_id: Some('5'),
            flight: Some("XG0042".into()),
            msg_num: Some(format!("M{seq:02}A")),
            text: format!("XNG SELFTEST {seq}"),
            ..Default::default()
        }),
        raw: Some(vec![0x2b, 0x2a, 0x16, 0x16, 0x01]),
        source: Provenance {
            station: StationIdentity::new("XX-SELFTEST-XNG"),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        },
    }
}

pub fn run(jsonl_path: Option<&Path>, json: bool) -> anyhow::Result<()> {
    dsp_check()?;

    let fmt = if json { ConsoleFormat::Json } else { ConsoleFormat::Pretty };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        let console_task = tokio::spawn(console::run(bus.subscribe(), fmt));
        let jsonl_task = jsonl_path.map(|p| {
            let rx = bus.subscribe();
            let p = p.to_owned();
            tokio::spawn(async move { jsonl::run(rx, &p).await })
        });

        for seq in 0..3 {
            let subs = bus.publish(synthetic_message(seq));
            anyhow::ensure!(subs >= 1, "bus published to {subs} subscribers");
        }
        drop(bus); // close the channel so outputs drain and exit

        console_task.await?;
        if let Some(t) = jsonl_task {
            t.await??;
            println!("[ok] jsonl output: 3 messages written to {}", jsonl_path.unwrap().display());
        }
        println!("[ok] bus + console output: 3 messages");
        println!("selftest passed");
        Ok(())
    })
}
