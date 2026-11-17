// The runner-availability epoch (XR-K-03, D1): every source of change
// advances it, and it advances once more after the backend's availability
// answers can no longer predate the change.
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const trustHandlers = vi.hoisted(() => [] as ((change: unknown) => void)[]);
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create(() => ({ info: { root: "/work/a", name: "a", recovery: [] } as { root: string; name: string; recovery: never[] } | null })) };
});
vi.mock("../settings/store", async () => {
  const { create } = await import("zustand");
  return { useSettings: create(() => ({ execution: null as unknown, images: null as unknown })) };
});
vi.mock("../../lib/ipc", () => ({
  events: {
    trustChanged: vi.fn(async (handler: (change: unknown) => void) => {
      trustHandlers.push(handler);
      return () => {};
    }),
  },
}));

const { AVAILABILITY_SETTLE_MS, runnerAvailabilityChanged, useRunnerAvailabilityEpoch } = await import("./availability");
const { useSettings } = await import("../settings/store");
const { useWorkspace } = await import("../workspace/store");

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("runner availability epoch", () => {
  it("advances on execution settings, trust, project, and explicit refreshes, and once more after the settle window", async () => {
    const { result } = renderHook(() => useRunnerAvailabilityEpoch());
    await act(async () => {});
    let last = result.current;
    const expectAdvanced = () => {
      expect(result.current).toBeGreaterThan(last);
      last = result.current;
    };

    act(() => useSettings.setState({ execution: { settings: { docker: { image: "python:3.13-slim" } }, warning: null } as never }));
    expectAdvanced();
    // A later answer cannot predate the change: one more read after the window.
    act(() => vi.advanceTimersByTime(AVAILABILITY_SETTLE_MS - 1));
    expect(result.current).toBe(last);
    act(() => vi.advanceTimersByTime(1));
    expectAdvanced();

    // Unrelated settings state (the image list) does not.
    act(() => useSettings.setState({ images: [] as never }));
    expect(result.current).toBe(last);

    act(() => trustHandlers.forEach((h) => h({ root: "/work/a", granted: true })));
    expectAdvanced();
    act(() => useWorkspace.setState({ info: null }));
    expectAdvanced();
    act(() => runnerAvailabilityChanged());
    expectAdvanced();
    // Changes in a burst share one follow-up read.
    act(() => vi.advanceTimersByTime(AVAILABILITY_SETTLE_MS));
    expectAdvanced();
    act(() => vi.advanceTimersByTime(AVAILABILITY_SETTLE_MS * 2));
    expect(result.current).toBe(last);
  });
});
