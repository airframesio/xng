// Shapes shared with the Rust side (src-tauri/src/{lib,request,session}.rs).

export interface ModeInfo {
  id: string;
  planRate: number;
  planChannels: number[];
  channelRate: number;
}

export interface AppInfo {
  version: string;
  backends: string[];
  cores: number;
  modes: ModeInfo[];
  os: string;
}

export interface DeviceEntry {
  selector: string;
  label: string;
  backend: string;
}

export type Source =
  | { kind: "device"; selector: string; gain: number | null }
  | { kind: "file"; path: string; format: string | null }
  /** A station file: several radios or recordings run together. */
  | { kind: "station"; path: string }
  /** Another decoder's JSON output (dumphfdl, dumpvdl2, acarsdec). */
  | { kind: "extern"; format: string; command: string };

/** What a station file would run. */
export interface StationPreview {
  stationId: string;
  sessions: { mode: string; source: string; live: boolean; channels: string[] }[];
  /** Outputs the file turns on, by config key. */
  outputs: string[];
}

export interface SessionRequest {
  source: Source;
  mode: string;
  sampleRate: number | null;
  centerFreq: string | null;
  channels: string[];
  receiverPos: string | null;
  filterLabels: string[];
  excludeLabels: string[];
  demodEffort: string | null;
  maxPpm: number | null;
  ais: {
    includeTypes: number[];
    excludeTypes: number[];
    includeMmsi: number[];
    excludeMmsi: number[];
    minIntervalS: number | null;
    dedupWindowS: number | null;
  };
  stationId: string | null;
  outputs: {
    jsonl: string | null;
    udp: string[];
    feedAirframes: boolean;
    asf2Grpc: string | null;
    asf2Quic: string | null;
    asf2QuicCa: string | null;
    asf2QuicInsecure: boolean;
    metrics: string | null;
    sbs: string | null;
    beast: string | null;
    nmeaTcp: string | null;
    nmeaUdp: string | null;
    nmeaTagBlocks: boolean;
    gsmtap: string | null;
    iridiumSatmap: string | null;
    gsFile: string | null;
    aircraftDb: string | null;
    http: string | null;
    mqtt: string | null;
    mqttTopic: string | null;
    zmq: string[];
    ownShipMmsi: number | null;
  };
}

export interface ResolvedTune {
  mode: string;
  sampleRate: number;
  centerHz: number;
  channelsHz: number[];
}

export interface MessageItem {
  id: number;
  ts: number;
  mode: string;
  freq: number;
  crcOk: boolean;
  rssi: number | null;
  snr: number | null;
  summary: string;
  detail: unknown;
}

export interface ChannelStat {
  freq: number;
  frames: number;
  crcOk: number;
  levelDb: number;
}

export interface SessionLive {
  samples: number;
  channels: ChannelStat[];
  spectrum: number[] | null;
}

export interface SessionSummary {
  mode: string;
  centerHz: number;
  sampleRate: number;
  channelsHz: number[];
  source: string;
  /** False for an external decoder: no spectrum or channel stats. */
  signal: boolean;
}

/** One thing on the map or in an entity list (the dashboard's JSON). */
export type Entity = Record<string, unknown> & {
  lat?: number;
  lon?: number;
  seen?: number;
  trail?: [number, number][];
};

export interface EntityState {
  station: string;
  sessions: Record<string, unknown>[];
  aircraft: Entity[];
  vessels: Entity[];
  beacons: Entity[];
  trains: Entity[];
  pagers: Entity[];
  alerts: Entity[];
  iridium_sats: Entity[];
  iridium_rings: Entity[];
  iridium_devices: Entity[];
  iridium_terminals: Entity[];
  totals: Record<string, number>;
  now: number;
}

export type BackendEvent =
  | { type: "started"; station: string; sessions: SessionSummary[] }
  | { type: "messages"; items: MessageItem[]; skipped: number; lagged?: number }
  | { type: "live"; sessions: SessionLive[]; elapsedMs: number }
  | { type: "entities"; state: EntityState }
  | { type: "ended"; error: string | null };

/** `xng scan` / `xng survey`, with the CLI's flags as fields. */
export type ToolRequest =
  | { kind: "scan"; sdr: string; gain: number | null; modes: string[]; dwellSecs: number }
  | {
      kind: "survey";
      sdr: string;
      gain: number | null;
      mode: string;
      sampleRate: number | null;
      channels: string[];
      durationSecs: number;
      interimSecs: number;
      scanFirst: boolean;
      scanDwellSecs: number;
      tuneGain: boolean;
      tuneDwellSecs: number;
      rotateDwellSecs: number;
      showMessages: boolean;
    }
  | { kind: "ingest"; grpc: string | null; quic: string | null; quicCertOut: string | null };

/** What `xng ingest` received by the time it was stopped. */
export interface IngestReport {
  feeders: number;
  messages: number;
}

/** A decode setup worked out from what a tool heard. */
export interface ProposedConfig {
  mode: string;
  sample_rate: number;
  center_hz: number;
  channels_hz: number[];
  gain_db?: number;
  command: string;
}

export interface ScanResult {
  mode: string;
  frequency_hz: number;
  frames: number;
  crc_ok: number;
  level_dbfs: number;
  verdict: string;
}

/** The report `xng scan --out` writes. */
export interface ScanReport {
  dwell_secs: number;
  results: ScanResult[];
  proposals: string[];
  configs: ProposedConfig[];
  systables?: unknown[];
}

export interface SurveyChannel {
  frequency_hz: number;
  listen_secs: number;
  frames: number;
  crc_ok: number;
  crc_bad: number;
  ok_pct: number;
  frames_per_min: number;
  level_dbfs: number;
  verdict: string;
}

/** The report `xng survey --out` writes. */
export interface SurveyReport {
  mode: string;
  duration_secs: number;
  sample_rate: number;
  gain: string;
  sweep?: { gain_db: number; frames: number; crc_ok: number; mean_level_dbfs: number }[];
  channels: SurveyChannel[];
  advice: string[];
  proposal?: string;
  config?: ProposedConfig;
}

export type ToolEvent =
  | { type: "line"; text: string }
  | { type: "done"; report: ScanReport | SurveyReport | IngestReport; stopped: boolean }
  | { type: "failed"; message: string };

export interface Backend {
  /** False when running in a plain browser with simulated data. */
  native: boolean;
  appInfo(): Promise<AppInfo>;
  listDevices(): Promise<DeviceEntry[]>;
  resolveTune(req: SessionRequest): Promise<ResolvedTune>;
  /** Read and check a station file without starting it. */
  stationPreview(path: string): Promise<StationPreview>;
  start(req: SessionRequest, onEvent: (e: BackendEvent) => void): Promise<void>;
  stop(): Promise<void>;
  /** Run a scan or survey; a running session is stopped first. */
  startTool(req: ToolRequest, onEvent: (e: ToolEvent) => void): Promise<void>;
  /** Ask the running tool to finish early; its report still arrives. */
  stopTool(): Promise<void>;
  writeText(path: string, contents: string): Promise<void>;
  exportEntities(format: string, path: string): Promise<void>;
  setDecodeThreads(n: number): Promise<boolean>;
  pickFile(title: string, filters?: { name: string; extensions: string[] }[]): Promise<string | null>;
  pickSave(title: string, defaultName: string): Promise<string | null>;
  openUrl(url: string): Promise<void>;
}
