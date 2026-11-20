// One notation for sizes, durations, and times in the interface. Features
// keep their own null handling and inputs (a run, a job, two timestamps) and
// render through these functions, so the same value reads the same everywhere.

const KIB = 1024;
const MIB = KIB * 1024;
const GIB = MIB * 1024;

/** A size in binary units, one decimal: "812 B", "48.2 KiB", "3.4 MiB", "1.5 GiB". */
export function formatBytes(bytes: number): string {
  if (bytes < KIB) return `${bytes} B`;
  if (bytes < MIB) return `${(bytes / KIB).toFixed(1)} KiB`;
  if (bytes < GIB) return `${(bytes / MIB).toFixed(1)} MiB`;
  return `${(bytes / GIB).toFixed(1)} GiB`;
}

/** Whole seconds: "45 s", "1 min", "1 min 15 s", "2 h", "2 h 5 min". */
export function formatSeconds(seconds: number): string {
  if (seconds < 60) return `${seconds} s`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  if (m < 60) return s ? `${m} min ${s} s` : `${m} min`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
}

/** An elapsed time: "850 ms", "1.5 s", "45 s", "1 min 15 s", "2 h 5 min". */
export function formatDuration(ms: number): string {
  const elapsed = Math.max(0, ms);
  if (elapsed < 1000) return `${Math.round(elapsed)} ms`;
  const seconds = elapsed / 1000;
  return seconds < 10 ? `${seconds.toFixed(1)} s` : formatSeconds(Math.round(seconds));
}

/** The time of day with seconds, in the user's locale ("14:03:22"). */
export function formatClock(ms: number, locale?: string): string {
  return new Date(ms).toLocaleTimeString(locale ?? [], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** A date ("Nov 20, 2026" in en-US). */
export function formatDate(ms: number, locale?: string): string {
  return new Date(ms).toLocaleDateString(locale, { dateStyle: "medium" });
}

/**
 * A date and time ("Nov 20, 2026, 2:03 PM" in en-US). `seconds` adds them
 * ("…, 2:03:22 PM") for records where several events share a minute.
 */
export function formatDateTime(ms: number, options: { seconds?: boolean; locale?: string } = {}): string {
  return new Date(ms).toLocaleString(options.locale, { dateStyle: "medium", timeStyle: options.seconds ? "medium" : "short" });
}

/** How long ago: "just now", "5 min ago", "3 h ago", then the date. */
export function formatRelativeTime(ms: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "just now";
  const mins = Math.round(seconds / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} h ago`;
  return formatDate(ms);
}
