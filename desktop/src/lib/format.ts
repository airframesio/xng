export function mhz(hz: number, digits = 3): string {
  return (hz / 1e6).toFixed(digits);
}

export function rate(sps: number): string {
  if (sps >= 1e6) return `${+(sps / 1e6).toFixed(3)} MS/s`;
  if (sps >= 1e3) return `${+(sps / 1e3).toFixed(1)} kS/s`;
  return `${sps} S/s`;
}

export function clock(ts: number): string {
  const d = new Date(ts);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())}`;
}

export function elapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(Math.floor(s / 3600))}:${p(Math.floor(s / 60) % 60)}:${p(s % 60)}`;
}

export function count(n: number): string {
  return n.toLocaleString("en-US");
}

/** "a, b ,c" to ["a","b","c"]. */
export function list(s: string): string[] {
  return s
    .split(/[\s,]+/)
    .map((x) => x.trim())
    .filter(Boolean);
}

export function numList(s: string): number[] {
  return list(s)
    .map(Number)
    .filter((n) => Number.isFinite(n));
}

export function num(s: string): number | null {
  const t = s.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

export function text(s: string): string | null {
  const t = s.trim();
  return t ? t : null;
}
