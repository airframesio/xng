// What each decode mode is, in plain words, grouped the way people look
// for them. Ids match `xng --mode`.

export type Family = "air" | "sea" | "sat" | "ground";

export interface ModeMeta {
  id: string;
  name: string;
  what: string;
  family: Family;
}

export const FAMILIES: { id: Family; name: string }[] = [
  { id: "air", name: "Aviation" },
  { id: "sea", name: "Maritime" },
  { id: "sat", name: "Satellite" },
  { id: "ground", name: "Ground and other" },
];

export const MODES: ModeMeta[] = [
  { id: "acars", name: "ACARS", what: "Airline text messages on VHF", family: "air" },
  { id: "vdl2", name: "VDL Mode 2", what: "Digital airline datalink on VHF", family: "air" },
  { id: "adsb", name: "ADS-B / Mode S", what: "Aircraft positions on 1090 MHz", family: "air" },
  { id: "hfdl", name: "HFDL", what: "Long-range airline datalink on shortwave", family: "air" },
  { id: "uat", name: "UAT 978", what: "US light-aircraft positions and weather", family: "air" },
  { id: "ads-l", name: "ADS-L", what: "Gliders, drones and light aircraft", family: "air" },
  { id: "vor", name: "VOR", what: "VOR radials, idents and station health", family: "air" },
  { id: "ils", name: "ILS", what: "Localizer and glideslope course readings and idents", family: "air" },
  { id: "ais", name: "AIS", what: "Ship positions and details", family: "sea" },
  { id: "vdes", name: "VDES", what: "Application messages beside AIS", family: "sea" },
  { id: "dsc", name: "DSC", what: "Maritime distress and calling", family: "sea" },
  { id: "navtex", name: "NAVTEX", what: "Maritime safety broadcasts", family: "sea" },
  { id: "aero-l", name: "Inmarsat Aero L", what: "Aircraft satcom, L-band", family: "sat" },
  { id: "aero-c", name: "Inmarsat Aero C", what: "Aircraft satcom, C-band feeder", family: "sat" },
  { id: "std-c", name: "Inmarsat STD-C", what: "SafetyNET and FleetNET broadcasts", family: "sat" },
  { id: "iridium", name: "Iridium", what: "Ring alerts, broadcasts and ACARS over SBD", family: "sat" },
  { id: "sarsat", name: "COSPAS-SARSAT", what: "406 MHz distress beacons", family: "sat" },
  { id: "sonde", name: "Radiosondes", what: "Weather balloon telemetry", family: "ground" },
  { id: "aprs", name: "APRS", what: "Amateur packet radio positions", family: "ground" },
  { id: "pocsag", name: "POCSAG", what: "Radio paging", family: "ground" },
  { id: "flex", name: "FLEX", what: "Radio paging", family: "ground" },
  { id: "eot", name: "EOT / HOT", what: "End-of-train telemetry", family: "ground" },
  { id: "atcs", name: "ATCS", what: "Rail control data radio", family: "ground" },
  { id: "time", name: "Time signals", what: "WWV, CHU, DCF77 and others", family: "ground" },
  { id: "meshtastic", name: "Meshtastic", what: "LoRa mesh messages and node positions", family: "ground" },
  { id: "meshcore", name: "MeshCore", what: "LoRa mesh adverts and channel messages", family: "ground" },
];

const BY_ID = new Map(MODES.map((m) => [m.id, m]));

export function modeMeta(id: string): ModeMeta {
  return BY_ID.get(id) ?? { id, name: id.toUpperCase(), what: "", family: "ground" };
}
