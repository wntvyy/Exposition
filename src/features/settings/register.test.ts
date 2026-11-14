// Palette wording matches the Settings section ("Storage & History", one form everywhere).
import { describe, expect, it, vi } from "vitest";

vi.mock("../editor/settings", () => ({ loadAppSettings: vi.fn(async () => {}) }));
vi.mock("./store", () => ({ loadExecutionSettings: vi.fn(async () => {}), showSection: vi.fn() }));
vi.mock("./SettingsEditor", () => ({ SettingsEditor: () => null }));
vi.mock("./engine", () => ({ canSwitchEngine: () => false, switchEngine: vi.fn() }));
vi.mock("../../app/editors", () => ({ editors: { open: vi.fn() } }));

import { registry } from "../../app/registry";
import { SECTIONS } from "./model";
import "./register";

describe("settings commands", () => {
  it("Storage & History is named the same in the palette and in Settings", () => {
    const section = SECTIONS.find((s) => s.id === "storage")!;
    expect(registry.command("settings.storage")?.title).toBe(section.title);
    expect(section.title).toBe("Storage & History");
  });
});
