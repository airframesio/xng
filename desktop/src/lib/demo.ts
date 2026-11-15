// Simulated backend for working on the UI in a plain browser. Nothing here
// ships a decode path: inside the app, backend.ts talks to the Rust engine.

import type {
  AppInfo,
  Backend,
  BackendEvent,
  ChannelStat,
  Entity,
  EntityState,
  MessageItem,
  ResolvedTune,
  ScanReport,
  SessionRequest,
  SurveyReport,
  ToolEvent,
  ToolRequest,
} from "./types";

const PLANS: Record<string, { rate: number; channels: number[] }> = {
  acars: { rate: 2_400_000, channels: [130.025, 130.425, 130.45, 131.125, 131.55, 131.725].map((m) => Math.round(m * 1e6)) },
  vdl2: { rate: 2_400_000, channels: [136.65, 136.7, 136.8, 136.975].map((m) => Math.round(m * 1e6)) },
  ais: { rate: 2_400_000, channels: [161.975e6, 162.025e6] },
  adsb: { rate: 2_400_000, channels: [1090e6] },
  hfdl: { rate: 912_000, channels: [8.927e6, 8.912e6, 8.894e6, 8.834e6] },
  iridium: { rate: 10_000_000, channels: [1626e6] },
};
const MODE_IDS = [
  "acars", "ais", "adsb", "vdl2", "std-c", "aero-l", "aero-c", "hfdl", "iridium", "uat", "sarsat",
  "dsc", "navtex", "sonde", "ads-l", "atcs", "aprs", "pocsag", "eot", "flex", "vdes", "time",
];

function plan(mode: string) {
  return PLANS[mode] ?? { rate: 2_400_000, channels: [162.4e6] };
}

const AIRLINES = ["UAL", "DAL", "AAL", "SWA", "ASA", "FDX", "UPS", "JBU", "ACA", "BAW"];
const LABELS = ["H1", "Q0", "5Z", "80", "SA", "B6", "16", "_d"];
const TEXTS = [
  "POS N3742.1W12218.4,KSFO,0412,350,ORRCA,0431,-52,26072",
  "OFF 0358 FOB 0412 ETA 0915",
  "REQ WX KSEA KPDX",
  "ETA 0915 GATE B12 PAX 164",
  "/PIKCLYA.DC1/CLD 0402 KSFO PDC 221",
  "FUEL 12.4 TEMP -52 WIND 260/72",
];

interface Plane {
  icao: string;
  flight: string;
  reg: string;
  lat: number;
  lon: number;
  trk: number;
  spd: number;
  alt: number;
  msgs: number;
  trail: [number, number][];
}

function rnd(a: number, b: number) {
  return a + Math.random() * (b - a);
}
function pick<T>(xs: T[]): T {
  return xs[Math.floor(Math.random() * xs.length)];
}
function hex(n: number) {
  return Array.from({ length: n }, () => "0123456789ABCDEF"[Math.floor(Math.random() * 16)]).join("");
}

export function demoBackend(): Backend {
  let timers: number[] = [];
  const stopAll = () => {
    timers.forEach((t) => clearInterval(t));
    timers = [];
  };

  const resolve = (req: SessionRequest): ResolvedTune => {
    const p = plan(req.mode);
    const channelsHz = req.channels.length ? req.channels.map((c) => parseFloat(c) * 1e6) : p.channels;
    const centerHz = Math.round((Math.min(...channelsHz) + Math.max(...channelsHz)) / 2);
    return { mode: req.mode, sampleRate: req.sampleRate ?? p.rate, centerHz, channelsHz };
  };

  return {
    native: false,
    async appInfo(): Promise<AppInfo> {
      return {
        version: "0.21.0",
        backends: ["soapysdr", "airspy", "airspyhf"],
        cores: navigator.hardwareConcurrency || 8,
        os: "browser",
        modes: MODE_IDS.map((id) => ({
          id,
          planRate: plan(id).rate,
          planChannels: plan(id).channels,
          channelRate: 24_000,
        })),
      };
    },
    async listDevices() {
      return [
        { selector: "driver=rtlsdr,serial=00000001", label: "Generic RTL2832U OEM :: 00000001", backend: "soapysdr" },
        { selector: "driver=airspy,serial=A74068C82F531693", label: "Airspy A74068C82F531693", backend: "airspy" },
      ];
    },
    async resolveTune(req) {
      return resolve(req);
    },
    async start(req, onEvent: (e: BackendEvent) => void) {
      stopAll();
      const tune = resolve(req);
      const t0 = performance.now();
      const home = { lat: 37.62, lon: -122.38 };
      const planes: Plane[] = Array.from({ length: 28 }, () => ({
        icao: hex(6),
        flight: pick(AIRLINES) + Math.floor(rnd(10, 2400)),
        reg: "N" + Math.floor(rnd(100, 999)) + pick(["UA", "DL", "AA", "SW", "AS"]),
        lat: home.lat + rnd(-1.6, 1.6),
        lon: home.lon + rnd(-2.2, 2.2),
        trk: rnd(0, 360),
        spd: rnd(180, 480),
        alt: Math.round(rnd(3, 39)) * 1000,
        msgs: 0,
        trail: [],
      }));
      const ships: Entity[] = Array.from({ length: 9 }, (_, i) => ({
        mmsi: 366000000 + Math.floor(rnd(1000, 999999)),
        name: ["PACIFIC STAR", "GOLDEN BEAR", "MATSONIA", "BAY CLIPPER", "SEA LION", "OLEUM", "DELTA PILOT", "ZELINSKY", "HORIZON"][i],
        lat: 37.8 + rnd(-0.12, 0.12),
        lon: -122.42 + rnd(-0.14, 0.1),
        cog: Math.round(rnd(0, 360)),
        sog: Math.round(rnd(0, 14) * 10) / 10,
        seen: Date.now() / 1000,
        msgs: 1,
      }));
      const stats: ChannelStat[] = tune.channelsHz.map((freq) => ({ freq, frames: 0, crcOk: 0, levelDb: -42 }));
      let id = 0;
      const totals: Record<string, number> = {};

      onEvent({
        type: "started",
        station: req.stationId || "XNG-DESKTOP",
        sessions: [
          {
            mode: tune.mode,
            centerHz: tune.centerHz,
            sampleRate: tune.sampleRate,
            channelsHz: tune.channelsHz,
            source: req.source.kind === "device" ? req.source.selector : req.source.path,
          },
        ],
      });

      const message = (): MessageItem => {
        const p = pick(planes);
        const ch = pick(stats);
        const ok = Math.random() > 0.04;
        ch.frames++;
        if (ok) ch.crcOk++;
        p.msgs++;
        totals[tune.mode] = (totals[tune.mode] ?? 0) + 1;
        const label = pick(LABELS);
        const text = pick(TEXTS);
        const rssi = Math.round(rnd(-38, -8) * 10) / 10;
        return {
          id: ++id,
          ts: Date.now(),
          mode: tune.mode,
          freq: ch.freq,
          crcOk: ok,
          rssi,
          snr: Math.round(rnd(6, 28) * 10) / 10,
          summary: `ACARS ${p.reg} ${p.flight} lbl=${label} ${ok ? "ok" : "BAD"} | ${text}`,
          detail: {
            mode: tune.mode,
            timestamp: new Date().toISOString(),
            frequency_hz: ch.freq,
            signal: { rssi_db: rssi },
            decode: { crc_ok: ok, fec_corrected: 0, errors: 0 },
            body: { type: "acars", mode: "2", tail: p.reg, label, block_id: "4", flight: p.flight, text, more_to_come: false },
            source: {
              station: { ident: req.stationId || "XNG-DESKTOP" },
              app: { name: "xng", version: "0.21.0" },
              channel: { frequency_hz: ch.freq, sample_rate: 24000 },
            },
          },
        };
      };

      timers.push(
        window.setInterval(() => {
          const n = Math.random() < 0.7 ? Math.floor(rnd(0, 4)) : 0;
          if (n) onEvent({ type: "messages", items: Array.from({ length: n }, message), skipped: 0 });
        }, 140),
      );

      timers.push(
        window.setInterval(() => {
          const span = tune.sampleRate;
          const t = performance.now() / 1000;
          const spectrum = Array.from({ length: 512 }, (_, k) => {
            const f = tune.centerHz - span / 2 + (k / 512) * span;
            let db = -78 + rnd(-3, 3) + 2 * Math.sin(k / 40 + t / 3);
            for (const c of stats) {
              const d = Math.abs(f - c.freq) / 9000;
              const burst = 0.5 + 0.5 * Math.sin(t * 1.7 + c.freq / 1e5);
              db = Math.max(db, -30 - d * d * 14 - (1 - burst) * 30 + rnd(-2, 2));
            }
            return db;
          });
          for (const c of stats) c.levelDb = -40 + rnd(-6, 12);
          onEvent({
            type: "live",
            elapsedMs: performance.now() - t0,
            sessions: [{ samples: Math.round(((performance.now() - t0) / 1000) * span), channels: stats.map((s) => ({ ...s })), spectrum }],
          });
        }, 100),
      );

      const entities = () => {
        const now = Date.now() / 1000;
        for (const p of planes) {
          const d = (p.spd * 1.852) / 3600 / 111;
          p.lat += d * Math.cos((p.trk * Math.PI) / 180);
          p.lon += (d * Math.sin((p.trk * Math.PI) / 180)) / Math.cos((p.lat * Math.PI) / 180);
          p.trk = (p.trk + rnd(-1.5, 1.5) + 360) % 360;
          p.trail.push([p.lat, p.lon]);
          if (p.trail.length > 60) p.trail.shift();
        }
        const state: EntityState = {
          station: req.stationId || "XNG-DESKTOP",
          sessions: [{ receiver_pos: [home.lat, home.lon] }],
          aircraft: planes.map((p) => ({
            id: p.icao, icao: p.icao, flight: p.flight, reg: p.reg, lat: p.lat, lon: p.lon,
            trk: Math.round(p.trk), spd: Math.round(p.spd), alt: p.alt, msgs: p.msgs, seen: now,
            trail: [...p.trail], actype: "B738",
          })),
          vessels: ships.map((s) => ({ ...s, seen: now })),
          beacons: [], trains: [], pagers: [], alerts: [],
          iridium_sats: [], iridium_rings: [], iridium_devices: [], iridium_terminals: [],
          totals: { ...totals },
          now,
        };
        onEvent({ type: "entities", state });
      };
      entities();
      timers.push(window.setInterval(entities, 1000));
    },
    async stop() {
      stopAll();
    },
    async startTool(req, onEvent) {
      stopAll();
      toolStop?.();
      toolStop = simulateTool(req, onEvent);
    },
    async stopTool() {
      toolStop?.();
      toolStop = null;
    },
    async writeText() {},
    async exportEntities() {},
    async setDecodeThreads() {
      return true;
    },
    async pickFile() {
      return "/recordings/acars-131M-2400k.cf32";
    },
    async pickSave(_t, name) {
      return "/exports/" + name;
    },
    async openUrl(url) {
      window.open(url, "_blank", "noopener");
    },
  };
}

let toolStop: (() => void) | null = null;

/** Play a scan or survey at about 20x speed. Returns a stop function that
 *  ends it early and still delivers the report, as the real tools do. */
function simulateTool(req: ToolRequest, onEvent: (e: ToolEvent) => void): () => void {
  const modes = req.kind === "scan" ? req.modes : [req.mode];
  const heard = modes.flatMap((mode) =>
    plan(mode).channels.map((f, i) => {
      const active = i % 3 !== 2;
      const frames = active ? Math.round(rnd(4, 60)) : 0;
      return { mode, f, frames, ok: Math.round(frames * rnd(0.8, 1)), level: active ? rnd(-45, -30) : rnd(-72, -64) };
    }),
  );
  const config = (mode: string) => {
    const chans = heard.filter((h) => h.mode === mode && h.ok > 0).map((h) => h.f);
    if (!chans.length) return null;
    const center = Math.round((Math.min(...chans) + Math.max(...chans)) / 2) + 25_000;
    const rate = plan(mode).rate;
    const command = `xng listen --sdr '${req.sdr}' --mode ${mode} -r ${rate} -c ${(center / 1e6).toFixed(3)}M --channels ${chans.map((c) => (c / 1e6).toFixed(3)).join(",")}`;
    return { mode, sample_rate: rate, center_hz: center, channels_hz: chans, command };
  };
  const report = (): ScanReport | SurveyReport => {
    if (req.kind === "scan") {
      const configs = modes.map(config).filter((c) => c !== null);
      return {
        dwell_secs: req.dwellSecs,
        results: heard.map((h) => ({
          mode: h.mode, frequency_hz: h.f, frames: h.frames, crc_ok: h.ok, level_dbfs: h.level,
          verdict: h.ok > 0 ? "ACTIVE" : "quiet",
        })),
        proposals: configs.map((c) => c.command),
        configs,
      };
    }
    const c = config(req.mode);
    return {
      mode: req.mode,
      duration_secs: req.durationSecs,
      sample_rate: plan(req.mode).rate,
      gain: req.tuneGain ? "30 dB (swept)" : req.gain == null ? "hardware AGC" : `${req.gain} dB (fixed)`,
      sweep: req.tuneGain
        ? [12, 21, 30, 39, 48].map((g) => ({ gain_db: g, frames: Math.round(rnd(0, 20)), crc_ok: Math.round(rnd(0, 15)), mean_level_dbfs: -70 + g }))
        : undefined,
      channels: heard.map((h) => ({
        frequency_hz: h.f, listen_secs: req.durationSecs, frames: h.frames, crc_ok: h.ok, crc_bad: h.frames - h.ok,
        ok_pct: h.frames ? (100 * h.ok) / h.frames : 0, frames_per_min: (60 * h.frames) / req.durationSecs,
        level_dbfs: h.level, verdict: h.ok > 0 ? "ACTIVE" : "quiet",
      })),
      advice: ["Levels look healthy. A fixed gain usually beats hardware AGC for bursty signals."],
      proposal: c?.command,
      config: c ?? undefined,
    };
  };
  const steps = heard.map((h) => `→ ${h.mode} @ ${(h.f / 1e6).toFixed(3)} MHz: ${h.frames} frames, ${h.ok} ok, ${h.level.toFixed(1)} dBFS`);
  steps.unshift(req.kind === "scan" ? `scanning ${heard.length} channel(s) across ${modes.length} mode(s)` : `surveying ${heard.length} channel(s), ${req.durationSecs}s total`);
  let i = 0;
  let finished = false;
  const finish = (stopped: boolean) => {
    if (finished) return;
    finished = true;
    clearInterval(timer);
    onEvent({ type: "done", report: report(), stopped });
  };
  const timer = window.setInterval(() => {
    if (i < steps.length) onEvent({ type: "line", text: steps[i++] });
    else finish(false);
  }, 450);
  return () => finish(true);
}
