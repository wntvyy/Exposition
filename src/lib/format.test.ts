import { describe, expect, it } from "vitest";

import { formatBytes, formatClock, formatDate, formatDateTime, formatDuration, formatRelativeTime, formatSeconds } from "./format";

describe("formatBytes", () => {
  it("uses binary units with one decimal", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(812)).toBe("812 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(49_357)).toBe("48.2 KiB");
    expect(formatBytes(1_200_000)).toBe("1.1 MiB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MiB");
    expect(formatBytes(1.5 * 1024 ** 3)).toBe("1.5 GiB");
    expect(formatBytes(2048 * 1024 ** 3)).toBe("2048.0 GiB");
  });
});

describe("durations", () => {
  it("formats whole seconds", () => {
    expect(formatSeconds(0)).toBe("0 s");
    expect(formatSeconds(45)).toBe("45 s");
    expect(formatSeconds(60)).toBe("1 min");
    expect(formatSeconds(75)).toBe("1 min 15 s");
    expect(formatSeconds(7200)).toBe("2 h");
    expect(formatSeconds(7500)).toBe("2 h 5 min");
  });

  it("formats elapsed milliseconds in the same notation", () => {
    expect(formatDuration(-5)).toBe("0 ms");
    expect(formatDuration(250)).toBe("250 ms");
    expect(formatDuration(1_500)).toBe("1.5 s");
    expect(formatDuration(9_940)).toBe("9.9 s");
    expect(formatDuration(12_400)).toBe("12 s");
    expect(formatDuration(75_000)).toBe("1 min 15 s");
    expect(formatDuration(3_900_000)).toBe("1 h 5 min");
  });
});

describe("times", () => {
  // Local time, so the expected text does not depend on the machine's zone.
  const at = new Date(2026, 10, 20, 14, 3, 22).getTime();

  it("formats the time of day with seconds", () => {
    expect(formatClock(at, "en-GB")).toBe("14:03:22");
  });

  it("formats a date, and a date with the time", () => {
    expect(formatDate(at, "en-US")).toBe("Nov 20, 2026");
    expect(formatDateTime(at, { locale: "en-US" }).replace(/\s/g, " ")).toBe("Nov 20, 2026, 2:03 PM");
    expect(formatDateTime(at, { locale: "en-US", seconds: true }).replace(/\s/g, " ")).toBe("Nov 20, 2026, 2:03:22 PM");
  });

  it("formats how long ago, then the date", () => {
    expect(formatRelativeTime(at, at + 20_000)).toBe("just now");
    expect(formatRelativeTime(at, at + 5 * 60_000)).toBe("5 min ago");
    expect(formatRelativeTime(at, at + 3 * 3_600_000)).toBe("3 h ago");
    expect(formatRelativeTime(at, at + 3 * 86_400_000)).toBe(formatDate(at));
  });
});
