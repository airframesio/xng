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
  | { kind: "file"; path: string; format: string | null };

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
  | { type: "messages"; items: MessageItem[]; skipped: number }
  | { type: "live"; sessions: SessionLive[]; elapsedMs: number }
  | { type: "entities"; state: EntityState }
  | { type: "ended"; error: string | null };

export interface Backend {
  /** False when running in a plain browser with simulated data. */
  native: boolean;
  appInfo(): Promise<AppInfo>;
  listDevices(): Promise<DeviceEntry[]>;
  resolveTune(req: SessionRequest): Promise<ResolvedTune>;
  start(req: SessionRequest, onEvent: (e: BackendEvent) => void): Promise<void>;
  stop(): Promise<void>;
  exportEntities(format: string, path: string): Promise<void>;
  setDecodeThreads(n: number): Promise<boolean>;
  pickFile(title: string, filters?: { name: string; extensions: string[] }[]): Promise<string | null>;
  pickSave(title: string, defaultName: string): Promise<string | null>;
  openUrl(url: string): Promise<void>;
}
