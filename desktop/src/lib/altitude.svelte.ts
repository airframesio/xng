// Altitude display unit, shared by every view that shows an altitude.
// "auto" follows the system region: feet for US, GB, CA, LR, MM, PR, GU, VI,
// AS or no region, meters elsewhere. The choice is kept in localStorage.

export type AltitudeUnit = "auto" | "ft" | "m" | "fl";

export const ALTITUDE_UNITS: AltitudeUnit[] = ["auto", "ft", "m", "fl"];

const KEY = "xng.altitudeUnit";
const FEET_REGIONS = new Set(["US", "GB", "CA", "LR", "MM", "PR", "GU", "VI", "AS"]);
const FT_PER_M = 3.28084;

/** The unit "auto" stands for here. */
export function regionAltitudeUnit(): "ft" | "m" {
  try {
    const tags = navigator.languages?.length ? navigator.languages : [navigator.language];
    for (const tag of tags) {
      if (!tag) continue;
      const region = new Intl.Locale(tag).region;
      if (region) return FEET_REGIONS.has(region) ? "ft" : "m";
    }
  } catch {
    // No Intl.Locale: feet.
  }
  return "ft";
}

function load(): AltitudeUnit {
  try {
    const v = localStorage.getItem(KEY);
    if (ALTITUDE_UNITS.includes(v as AltitudeUnit)) return v as AltitudeUnit;
  } catch {
    // Storage blocked.
  }
  return "auto";
}

export const altitude = $state<{ unit: AltitudeUnit }>({ unit: load() });

export function setAltitudeUnit(unit: AltitudeUnit) {
  altitude.unit = unit;
  try {
    localStorage.setItem(KEY, unit);
  } catch {
    // Storage blocked: the choice lasts until the app closes.
  }
}

export function altitudeUnitLabel(unit: AltitudeUnit): string {
  if (unit === "auto") return `Automatic (${regionAltitudeUnit() === "m" ? "Meters" : "Feet"})`;
  return { ft: "Feet", m: "Meters", fl: "Flight levels" }[unit];
}

export function metersToFeet(m: number): number {
  return m * FT_PER_M;
}

/**
 * An altitude in feet, in the chosen unit: "36,000 ft", "10,973 m" or
 * "FL360" (flight levels from 18,000 ft, feet below). `elevation` marks a
 * height or elevation, where flight levels mean feet. Non-numbers come back
 * as they are.
 */
export function formatAltitude(ft: unknown, elevation = false): string {
  if (ft == null || ft === "") return "";
  const v = Number(ft);
  if (!Number.isFinite(v)) return String(ft);
  const unit = altitude.unit === "auto" ? regionAltitudeUnit() : altitude.unit;
  if (unit === "m") return `${Math.round(v / FT_PER_M).toLocaleString()} m`;
  if (unit === "fl" && !elevation && v >= 18000) return `FL${String(Math.round(v / 100)).padStart(3, "0")}`;
  return `${Math.round(v).toLocaleString()} ft`;
}
