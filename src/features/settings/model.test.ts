import { describe, expect, it } from "vitest";

import type { AppInfo } from "../../bindings/AppInfo";
import type { DockerImage } from "../../bindings/DockerImage";
import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { ExecutionSettingsState } from "../../bindings/ExecutionSettingsState";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import {
  deriveCapabilities,
  dockerAvailability,
  formatGrantedAt,
  imageOptions,
  imageState,
  isSectionId,
  restrictedGuarantees,
  SECTIONS,
  toolRows,
  type CapabilityInput,
} from "./model";

function tool(id: string, overrides: Partial<ToolchainInfo> = {}): ToolchainInfo {
  return {
    id,
    name: id,
    category: "tex",
    path: `C:/tools/${id}.exe`,
    source: "path",
    version: `${id} 1.0`,
    status: "available",
    detail: null,
    guidance: null,
    ...overrides,
  };
}

function execution(image: string | null = null, toolPaths: Record<string, string> = {}): ExecutionSettingsState {
  return {
    settings: {
      toolPaths,
      docker: { image, memory: "2g", cpus: "2", pidsLimit: 256, user: "65534:65534" },
      maxConcurrentJobs: null,
    },
    overridableTools: ["pdflatex", "python", "docker"],
  };
}

const image = (reference: string): DockerImage => ({ reference, id: "e48f59cdba69", size: "178MB", created: "6 days ago" });

function runtime(language: RuntimeEnvironment["language"], available: boolean): RuntimeEnvironment {
  return {
    id: `${language}:x`,
    language,
    displayName: `${language} 3`,
    executable: "C:/x.exe",
    version: "3.14.0",
    kernel: null,
    hostInterpreter: null,
    packages: [],
    kernelEnvironment: {},
    availability: available
      ? { kind: "available" }
      : { kind: "unavailable", reason: `${language} not found`, guidance: `Install ${language}` },
  };
}

const app: AppInfo = {
  name: "Exposition",
  version: "0.1.0",
  platform: "windows",
  arch: "x86_64",
  debugBuild: true,
  dataDir: "C:/data",
  typstCompiler: "typst 0.15.1",
  markdownProfile: "exposition-markdown-print/1",
};

const provider = (configured: boolean): ProviderStatus => ({
  settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
  configured,
  credentialStore: "Windows Credential Manager",
  credentialError: null,
  scriptedAvailable: false,
});

describe("sections", () => {
  it("lists every required section once, with About last", () => {
    expect(SECTIONS.map((s) => s.id)).toEqual(["appearance", "editor", "trust", "execution", "agents", "research", "about"]);
    expect(isSectionId("trust")).toBe(true);
    expect(isSectionId("nope")).toBe(false);
  });
});

describe("tool rows", () => {
  it("joins discovery and overrides in the service's order", () => {
    const rows = toolRows(execution(null, { python: "D:/py/python.exe" }), [
      tool("python", { source: "override", path: "D:/py/python.exe", version: "Python 3.12.9" }),
      tool("pdflatex"),
      tool("python-jupyter"),
    ]);
    expect(rows.map((r) => r.id)).toEqual(["pdflatex", "python", "docker"]);
    expect(rows[1]).toMatchObject({ override: "D:/py/python.exe", source: "override", version: "Python 3.12.9" });
    // Discovery has not reported docker yet.
    expect(rows[2]).toMatchObject({ status: "checking", override: null, path: null });
  });

  it("is empty until execution settings load", () => {
    expect(toolRows(null, [tool("python")])).toEqual([]);
  });
});

describe("docker", () => {
  it("reports availability from discovery", () => {
    expect(dockerAvailability(null)).toEqual({ kind: "checking" });
    expect(dockerAvailability([tool("docker", { version: "client 29.9.2, engine 29.9.2" })])).toMatchObject({
      kind: "available",
      version: "client 29.9.2, engine 29.9.2",
    });
    expect(dockerAvailability([tool("docker", { status: "broken", detail: "engine not reachable" })])).toMatchObject({
      kind: "broken",
      detail: "engine not reachable",
    });
    expect(dockerAvailability([tool("docker", { status: "missing", path: null, guidance: "Install Docker" })])).toEqual({
      kind: "missing",
      guidance: "Install Docker",
    });
  });

  it("checks the configured image against local images only", () => {
    expect(imageState(null, [image("python:3.13-slim")])).toEqual({ kind: "notConfigured" });
    expect(imageState("python:3.13-slim", null)).toEqual({ kind: "unverified", image: "python:3.13-slim" });
    expect(imageState("python:3.13-slim", [image("python:3.13-slim")]).kind).toBe("present");
    expect(imageState("gone:1", [image("python:3.13-slim")])).toEqual({ kind: "missing", image: "gone:1" });
  });

  it("offers local images and keeps a missing configured image visible", () => {
    const options = imageOptions([image("node:26"), image("python:3.13-slim")], "gone:1");
    expect(options.map((o) => o.id)).toEqual(["gone:1", "node:26", "python:3.13-slim"]);
    expect(options[0]!.description).toBe("Not present locally");
    expect(options[2]!.description).toBe("178MB · 6 days ago");
    expect(imageOptions(null, null)).toEqual([]);
  });

  it("describes enforcement with the configured limits", () => {
    const lines = restrictedGuarantees(execution().settings.docker).join("\n");
    expect(lines).toContain("--network=none");
    expect(lines).toContain("read-only");
    expect(lines).toContain("65534:65534");
    expect(lines).toContain("2g memory, 2 CPUs, 256 processes");
    expect(lines).toMatch(/never pulled/);
  });
});

describe("trust", () => {
  it("formats grant times and admits missing ones", () => {
    expect(formatGrantedAt(null)).toBe("Date not recorded");
    expect(formatGrantedAt(Date.UTC(2026, 10, 11, 12, 0), "en-US")).toContain("2026");
  });
});

describe("capabilities", () => {
  const empty: CapabilityInput = { app: null, toolchains: null, environments: null, provider: null, execution: null, images: null };

  it("claims nothing before services report", () => {
    const capabilities = deriveCapabilities(empty);
    expect(capabilities.length).toBeGreaterThan(8);
    expect(capabilities.every((c) => c.state === "checking")).toBe(true);
  });

  it("derives every entry from service status", () => {
    const environments: EnvironmentReport = {
      runtimes: [runtime("python", true), runtime("r", false), runtime("julia", true)],
      scannedAtMs: 1,
    };
    const toolchains = [
      tool("pdflatex", { version: "pdfTeX 3.14" }),
      tool("lualatex", { status: "missing", version: null, path: null, guidance: "Install TeX" }),
      tool("biber", { status: "missing", version: null }),
      tool("bibtex", { version: "BibTeX 0.99e" }),
      tool("lean", { version: "Lean 4.35.0" }),
      tool("lake", { status: "broken", version: null, detail: "exited 1" }),
      tool("docker", { version: "client 29.9.2, engine 29.9.2" }),
    ];
    const byId = (input: CapabilityInput) => Object.fromEntries(deriveCapabilities(input).map((c) => [c.id, c]));
    const caps = byId({
      app,
      toolchains,
      environments,
      provider: provider(false),
      execution: execution("python:3.13-slim"),
      images: [image("python:3.13-slim")],
    });
    expect(caps.typst).toMatchObject({ state: "available", detail: "typst 0.15.1, built in" });
    expect(caps.markdown!.detail).toContain("exposition-markdown-print/1");
    expect(caps.pdflatex).toMatchObject({ state: "available", detail: "pdfTeX 3.14" });
    expect(caps.lualatex).toMatchObject({ state: "unavailable", detail: "lualatex: Install TeX" });
    expect(caps.bibliography).toMatchObject({ state: "available", detail: "BibTeX 0.99e" });
    expect(caps["research-python"]!.state).toBe("available");
    expect(caps["research-r"]).toMatchObject({ state: "unavailable", detail: "r not found. Install r" });
    expect(caps.lean).toMatchObject({ state: "unavailable", detail: "lake: exited 1" });
    expect(caps.restricted).toMatchObject({ state: "available", detail: "Image python:3.13-slim" });
    expect(caps.agents).toMatchObject({ state: "unavailable", detail: "No API key configured" });

    // A tool absent from discovery is not available.
    expect(byId({ ...empty, toolchains: [] }).pdflatex!.state).toBe("unavailable");
    // Restricted execution needs a configured, locally present image and Docker.
    expect(byId({ ...empty, toolchains, execution: execution(null) }).restricted!.detail).toMatch(/No container image/);
    expect(byId({ ...empty, toolchains, execution: execution("gone:1"), images: [] }).restricted!.state).toBe("unavailable");
    expect(
      byId({ ...empty, toolchains: [tool("docker", { status: "missing" })], execution: execution("python:3.13-slim") })
        .restricted!.state,
    ).toBe("unavailable");
    expect(byId({ ...empty, provider: provider(true) }).agents).toMatchObject({ state: "available" });
    expect(byId({ ...empty, provider: provider(true) }).agents!.detail).not.toMatch(/sk-/);
  });
});
