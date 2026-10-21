const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 1.2 GB, 340 MB, 12 KB (1024-based, like Explorer). */
export function formatBytes(bytes: number): string {
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = value >= 100 || unit === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** A compact rate for tight spots: 950, 12K, 1.2M, 3.4G (bytes per second). */
export function formatRateShort(bytesPerSecond: number): string {
  const b = Math.max(0, bytesPerSecond);
  if (b < 1000) return `${Math.round(b)}`;
  if (b < 1000 * 1024) return `${Math.round(b / 1024)}K`;
  if (b < 1000 * 1024 * 1024) {
    const m = b / 1024 / 1024;
    return `${m < 10 ? m.toFixed(1) : Math.round(m)}M`;
  }
  return `${(b / 1024 / 1024 / 1024).toFixed(1)}G`;
}

/** 1.2 MB/s. */
export function formatRate(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

export function formatPercent(value: number): string {
  return `${Math.round(Math.max(0, Math.min(100, value)))}%`;
}

/** How loaded a 0–100 value is: drives the meter colour (with the number beside it). */
export function level(value: number): "ok" | "warn" | "hot" {
  if (value >= 90) return "hot";
  if (value >= 75) return "warn";
  return "ok";
}

/** "just now", "12s ago", "3m ago". */
export function ago(at: number, now: number): string {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 3) return "just now";
  if (s < 60) return `${s}s ago`;
  return `${Math.round(s / 60)}m ago`;
}
