// App state. Heavy, fast-changing data (messages, entities) is kept in
// raw state and replaced wholesale per batch, so Svelte never deep-proxies
// thousands of objects. Spectrum frames bypass reactivity entirely.

import { backend } from "./backend";
import { list, num, numList, text } from "./format";
import type {
  AppInfo,
  BackendEvent,
  ChannelStat,
  DeviceEntry,
  EntityState,
  MessageItem,
  ResolvedTune,
  SessionRequest,
  SessionSummary,
} from "./types";

const MAX_MESSAGES = 5000;
const SETTINGS_KEY = "xng.settings.v1";

/** Everything the setup form holds, as the strings the user typed. */
export interface Settings {
  sourceKind: "device" | "file";
  device: string;
  gain: string;
  filePath: string;
  fileFormat: string;
  mode: string;
  sampleRate: string;
  centerFreq: string;
  channels: string;
  receiverPos: string;
  demodEffort: string;
  maxPpm: string;
  decodeThreads: string;
  filterLabels: string;
  excludeLabels: string;
  aisIncludeTypes: string;
  aisExcludeTypes: string;
  aisIncludeMmsi: string;
  aisExcludeMmsi: string;
  aisMinInterval: string;
  aisDedupWindow: string;
  stationId: string;
  feedAirframes: boolean;
  jsonl: string;
  udp: string;
  asf2Grpc: string;
  asf2Quic: string;
  asf2QuicCa: string;
  asf2QuicInsecure: boolean;
  metrics: string;
  http: string;
  sbs: string;
  beast: string;
  nmeaTcp: string;
  nmeaUdp: string;
  nmeaTagBlocks: boolean;
  gsmtap: string;
  iridiumSatmap: string;
  gsFile: string;
  aircraftDb: string;
  mqtt: string;
  mqttTopic: string;
  zmq: string;
  ownShipMmsi: string;
}

const DEFAULTS: Settings = {
  sourceKind: "device", device: "", gain: "", filePath: "", fileFormat: "",
  mode: "acars", sampleRate: "", centerFreq: "", channels: "", receiverPos: "",
  demodEffort: "", maxPpm: "", decodeThreads: "", filterLabels: "", excludeLabels: "",
  aisIncludeTypes: "", aisExcludeTypes: "", aisIncludeMmsi: "", aisExcludeMmsi: "",
  aisMinInterval: "", aisDedupWindow: "", stationId: "", feedAirframes: false,
  jsonl: "", udp: "", asf2Grpc: "", asf2Quic: "", asf2QuicCa: "", asf2QuicInsecure: false,
  metrics: "", http: "", sbs: "", beast: "", nmeaTcp: "", nmeaUdp: "", nmeaTagBlocks: false,
  gsmtap: "", iridiumSatmap: "", gsFile: "", aircraftDb: "", mqtt: "", mqttTopic: "",
  zmq: "", ownShipMmsi: "",
};

function loadSettings(): Settings {
  try {
    const saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "{}");
    return { ...DEFAULTS, ...saved };
  } catch {
    return { ...DEFAULTS };
  }
}

export const settings = $state<Settings>(loadSettings());

export function saveSettings() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify($state.snapshot(settings)));
  } catch {
    // Storage unavailable: settings just don't persist.
  }
}

export function resetAdvanced() {
  const keep: Partial<Settings> = {
    sourceKind: settings.sourceKind, device: settings.device, filePath: settings.filePath, mode: settings.mode,
  };
  // For a recording these describe the capture and live on the Setup
  // screen, not in the Advanced panel.
  if (settings.sourceKind === "file") {
    Object.assign(keep, {
      sampleRate: settings.sampleRate, centerFreq: settings.centerFreq, fileFormat: settings.fileFormat,
    });
  }
  Object.assign(settings, DEFAULTS, keep);
  saveSettings();
}

/** How many advanced fields differ from their defaults. */
export function advancedCount(): number {
  const skip = new Set(["sourceKind", "device", "filePath", "mode"]);
  if (settings.sourceKind === "file") ["sampleRate", "centerFreq", "fileFormat"].forEach((k) => skip.add(k));
  return (Object.keys(DEFAULTS) as (keyof Settings)[]).filter((k) => !skip.has(k) && settings[k] !== DEFAULTS[k]).length;
}

export function buildRequest(): SessionRequest {
  const s = settings;
  return {
    source:
      s.sourceKind === "device"
        ? { kind: "device", selector: s.device, gain: num(s.gain) }
        : { kind: "file", path: s.filePath, format: text(s.fileFormat) },
    mode: s.mode,
    sampleRate: num(s.sampleRate),
    centerFreq: text(s.centerFreq),
    channels: list(s.channels),
    receiverPos: text(s.receiverPos),
    filterLabels: list(s.filterLabels),
    excludeLabels: list(s.excludeLabels),
    demodEffort: text(s.demodEffort),
    maxPpm: num(s.maxPpm),
    ais: {
      includeTypes: numList(s.aisIncludeTypes),
      excludeTypes: numList(s.aisExcludeTypes),
      includeMmsi: numList(s.aisIncludeMmsi),
      excludeMmsi: numList(s.aisExcludeMmsi),
      minIntervalS: num(s.aisMinInterval),
      dedupWindowS: num(s.aisDedupWindow),
    },
    stationId: text(s.stationId),
    outputs: {
      jsonl: text(s.jsonl),
      udp: list(s.udp),
      feedAirframes: s.feedAirframes,
      asf2Grpc: text(s.asf2Grpc),
      asf2Quic: text(s.asf2Quic),
      asf2QuicCa: text(s.asf2QuicCa),
      asf2QuicInsecure: s.asf2QuicInsecure,
      metrics: text(s.metrics),
      sbs: text(s.sbs),
      beast: text(s.beast),
      nmeaTcp: text(s.nmeaTcp),
      nmeaUdp: text(s.nmeaUdp),
      nmeaTagBlocks: s.nmeaTagBlocks,
      gsmtap: text(s.gsmtap),
      iridiumSatmap: text(s.iridiumSatmap),
      gsFile: text(s.gsFile),
      aircraftDb: text(s.aircraftDb),
      http: text(s.http),
      mqtt: text(s.mqtt),
      mqttTopic: text(s.mqttTopic),
      zmq: list(s.zmq),
      ownShipMmsi: num(s.ownShipMmsi),
    },
  };
}

export type Phase = "idle" | "starting" | "running" | "ended";
export type View = "live" | "messages" | "signal";

class AppState {
  info = $state<AppInfo | null>(null);
  native = $state(true);
  devices = $state<DeviceEntry[]>([]);
  devicesLoading = $state(false);
  devicesError = $state<string | null>(null);

  phase = $state<Phase>("idle");
  view = $state<View>("live");
  error = $state<string | null>(null);

  station = $state("");
  sessions = $state.raw<SessionSummary[]>([]);
  messages = $state.raw<MessageItem[]>([]);
  total = $state(0);
  dropped = $state(0);
  /** Of `dropped`, how many the map missed as well. */
  lagged = $state(0);
  channels = $state.raw<ChannelStat[]>([]);
  samples = $state(0);
  elapsedMs = $state(0);
  entities = $state.raw<EntityState | null>(null);
  selectedId = $state<number | null>(null);
  /** Messages per second, smoothed. */
  rate = $state(0);

  private spectrumListeners = new Set<(bins: number[]) => void>();
  private rateWindow: { t: number; n: number }[] = [];

  onSpectrum(fn: (bins: number[]) => void): () => void {
    this.spectrumListeners.add(fn);
    return () => this.spectrumListeners.delete(fn);
  }

  get session(): SessionSummary | null {
    return this.sessions[0] ?? null;
  }

  get selected(): MessageItem | null {
    const id = this.selectedId;
    return id == null ? null : (this.messages.find((m) => m.id === id) ?? null);
  }

  async init() {
    const b = await backend();
    this.native = b.native;
    this.info = await b.appInfo();
    await this.refreshDevices();
  }

  async refreshDevices() {
    this.devicesLoading = true;
    this.devicesError = null;
    try {
      const b = await backend();
      this.devices = await b.listDevices();
      if (!this.devices.some((d) => d.selector === settings.device)) {
        settings.device = this.devices[0]?.selector ?? "";
      }
    } catch (e) {
      this.devicesError = String(e);
      this.devices = [];
    } finally {
      this.devicesLoading = false;
    }
  }

  async resolve(): Promise<ResolvedTune> {
    return (await backend()).resolveTune(buildRequest());
  }

  async start() {
    if (this.phase === "starting" || this.phase === "running") return;
    saveSettings();
    this.error = null;
    this.phase = "starting";
    this.messages = [];
    this.total = 0;
    this.dropped = 0;
    this.lagged = 0;
    this.channels = [];
    this.samples = 0;
    this.elapsedMs = 0;
    this.entities = null;
    this.selectedId = null;
    this.rate = 0;
    this.rateWindow = [];
    try {
      const b = await backend();
      const threads = num(settings.decodeThreads);
      if (threads) await b.setDecodeThreads(threads);
      await b.start(buildRequest(), (e) => this.handle(e));
    } catch (e) {
      this.error = String(e);
      this.phase = "idle";
    }
  }

  async stop() {
    try {
      await (await backend()).stop();
    } finally {
      if (this.phase === "running" || this.phase === "starting") this.phase = "ended";
    }
  }

  /** Back to the setup screen. */
  reset() {
    this.phase = "idle";
    this.error = null;
  }

  private handle(e: BackendEvent) {
    switch (e.type) {
      case "started":
        this.station = e.station;
        this.sessions = e.sessions;
        this.phase = "running";
        this.view = "live";
        break;
      case "messages": {
        const n = e.items.length + e.skipped;
        this.total += n;
        this.dropped += e.skipped;
        this.lagged += e.lagged ?? 0;
        const now = performance.now();
        this.rateWindow.push({ t: now, n });
        while (this.rateWindow.length && now - this.rateWindow[0].t > 5000) this.rateWindow.shift();
        this.rate = this.rateWindow.reduce((a, w) => a + w.n, 0) / 5;
        if (e.items.length) {
          const next = this.messages.concat(e.items);
          this.messages = next.length > MAX_MESSAGES ? next.slice(next.length - MAX_MESSAGES) : next;
        }
        break;
      }
      case "live": {
        const s = e.sessions[0];
        if (!s) break;
        this.elapsedMs = e.elapsedMs;
        this.samples = s.samples;
        this.channels = s.channels;
        if (s.spectrum) for (const fn of this.spectrumListeners) fn(s.spectrum);
        break;
      }
      case "entities":
        this.entities = e.state;
        break;
      case "ended":
        if (e.error) this.error = e.error;
        if (this.phase !== "idle") this.phase = "ended";
        break;
    }
  }
}

export const app = new AppState();
