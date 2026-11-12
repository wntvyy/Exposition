// Settings files set aside at load are reported once, naming the kept file.
import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ settingsGet: vi.fn(), executionSettingsGet: vi.fn() }));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));

vi.mock("../../lib/ipc", () => ({ commands: ipc, errorMessage: String }));
vi.mock("../../app/notifications", () => notifications);

const { loadAppSettings, resetAppSettingsForTests, useAppSettings } = await import("../editor/settings");
const { loadExecutionSettings } = await import("./store");
const { resetSettingsWarnings } = await import("./warnings");

const KEPT = "C:\\Users\\me\\AppData\\Local\\org.exposition.desktop\\settings.json.invalid-1731400000000";

beforeEach(() => {
  resetAppSettingsForTests();
  resetSettingsWarnings();
  notifications.notify.mockReset();
});

describe("settings warnings", () => {
  it("reports an application settings file that was set aside, naming the kept original", async () => {
    const warning = `Editor font size must be 8–40 (got 400). Defaults are in use; the original file was kept as ${KEPT}.`;
    ipc.settingsGet.mockResolvedValue({
      settings: { version: 1, editor: {}, files: { reopenLastProject: false } },
      warning,
    });
    await loadAppSettings();
    await loadAppSettings();
    expect(useAppSettings.getState().loaded).toBe(true);
    expect(notifications.notify).toHaveBeenCalledTimes(1);
    expect(notifications.notify).toHaveBeenCalledWith(
      expect.objectContaining({ tone: "warning", title: "Invalid application settings were reset", timeoutMs: null }),
    );
    expect(notifications.notify.mock.calls[0]![0].body).toContain(KEPT);
  });

  it("reports invalid execution settings once per session although every read returns the warning", async () => {
    const warning = `Execution settings in C:\\data\\execution\\settings.json: docker.user "0:0" would run restricted containers as root (using 65534:65534). The original file was kept as C:\\data\\execution\\settings.json.invalid-1.`;
    ipc.executionSettingsGet.mockResolvedValue({
      settings: { toolPaths: {}, docker: { image: null, memory: "2g", cpus: "2", pidsLimit: 256, user: "65534:65534" }, maxConcurrentJobs: null },
      overridableTools: [],
      warning,
    });
    await loadExecutionSettings();
    await loadExecutionSettings();
    expect(notifications.notify).toHaveBeenCalledTimes(1);
    expect(notifications.notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Invalid execution settings were reset", body: warning }),
    );
  });

  it("stays quiet when nothing was set aside", async () => {
    ipc.settingsGet.mockResolvedValue({ settings: { version: 1, editor: {}, files: { reopenLastProject: false } }, warning: null });
    await loadAppSettings();
    expect(notifications.notify).not.toHaveBeenCalled();
  });
});
