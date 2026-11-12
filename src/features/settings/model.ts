// Pure settings-view logic: sections, tool rows, image state, and the
// capability summary derived from real service status (never hard-coded).
import type { AppInfo } from "../../bindings/AppInfo";
import type { DockerImage } from "../../bindings/DockerImage";
import type { DockerSettings } from "../../bindings/DockerSettings";
import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { ExecutionSettingsState } from "../../bindings/ExecutionSettingsState";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { RuntimeLanguage } from "../../bindings/RuntimeLanguage";
import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import type { ToolSource } from "../../bindings/ToolSource";

export const SETTINGS_RESOURCE = { key: "settings:", title: "Settings" } as const;

export const SECTIONS = [
  { id: "appearance", title: "Appearance" },
  { id: "editor", title: "Editor" },
  { id: "trust", title: "Trusted folders" },
  { id: "execution", title: "Execution" },
  { id: "agents", title: "Agents" },
  { id: "research", title: "Research" },
  { id: "about", title: "About" },
] as const;

export type SectionId = (typeof SECTIONS)[number]["id"];

export function isSectionId(value: unknown): value is SectionId {
  return SECTIONS.some((s) => s.id === value);
}

// ---------- Tools ----------

export interface ToolRow {
  id: string;
  name: string;
  /** `checking` until discovery has reported this tool. */
  status: ToolchainInfo["status"] | "checking";
  version: string | null;
  path: string | null;
  source: ToolSource | null;
  /** The configured override (may differ from `path` while discovery is stale). */
  override: string | null;
  detail: string | null;
  guidance: string | null;
}

/** One row per overridable tool, in catalog order, joined with discovery and settings. */
export function toolRows(state: ExecutionSettingsState | null, toolchains: ToolchainInfo[] | null): ToolRow[] {
  if (!state) return [];
  return state.overridableTools.map((id) => {
    const info = toolchains?.find((t) => t.id === id);
    return {
      id,
      name: info?.name ?? id,
      status: info?.status ?? "checking",
      version: info?.version ?? null,
      path: info?.path ?? null,
      source: info?.source ?? null,
      override: state.settings.toolPaths[id] ?? null,
      detail: info?.detail ?? null,
      guidance: info?.guidance ?? null,
    };
  });
}

export const SOURCE_LABELS: Record<ToolSource, string> = {
  override: "Set in settings",
  knownLocation: "Found in a standard install location",
  path: "Found on PATH",
};

// ---------- Docker ----------

export type DockerAvailability =
  | { kind: "checking" }
  | { kind: "available"; version: string | null; path: string | null }
  | { kind: "broken"; detail: string; path: string | null }
  | { kind: "missing"; guidance: string };

export function dockerAvailability(toolchains: ToolchainInfo[] | null): DockerAvailability {
  const docker = toolchains?.find((t) => t.id === "docker");
  if (!docker) return { kind: "checking" };
  if (docker.status === "available") return { kind: "available", version: docker.version, path: docker.path };
  if (docker.status === "broken") {
    return { kind: "broken", detail: docker.detail ?? "Docker is installed but not working.", path: docker.path };
  }
  return { kind: "missing", guidance: docker.guidance ?? "Docker was not found." };
}

export type ImageState =
  | { kind: "notConfigured" }
  /** Configured; the local image list is not available (yet). */
  | { kind: "unverified"; image: string }
  | { kind: "present"; image: string; local: DockerImage }
  | { kind: "missing"; image: string };

export function imageState(configured: string | null, images: DockerImage[] | null): ImageState {
  if (!configured) return { kind: "notConfigured" };
  if (!images) return { kind: "unverified", image: configured };
  const local = images.find((i) => i.reference === configured);
  return local ? { kind: "present", image: configured, local } : { kind: "missing", image: configured };
}

/** Select options for local images; the configured image stays selectable even when missing. */
export function imageOptions(images: DockerImage[] | null, configured: string | null) {
  const options = (images ?? []).map((i) => ({
    id: i.reference,
    label: i.reference,
    description: [i.size, i.created].filter(Boolean).join(" · ") || undefined,
  }));
  if (configured && !options.some((o) => o.id === configured)) {
    options.unshift({ id: configured, label: configured, description: "Not present locally" });
  }
  return options;
}

/** What a restricted (container) run enforces, with the configured limits. Mirrors `docker::run_args`. */
export function restrictedGuarantees(docker: DockerSettings): string[] {
  return [
    "No network access (--network=none)",
    "Project folder mounted read-only at /workspace; writes go only to a per-run output folder and a 256 MB /tmp",
    `Runs as a non-root user (${docker.user}) with all capabilities dropped and no privilege escalation`,
    "No host environment: only variables the run sets explicitly; credentials are never passed",
    `Limits: ${docker.memory} memory, ${docker.cpus} CPUs, ${docker.pidsLimit} processes`,
    "Images are never pulled; a missing image or engine refuses the run instead of running it on this computer",
  ];
}

// ---------- Trust ----------

export function formatGrantedAt(ms: number | null, locale?: string): string {
  if (ms === null) return "Date not recorded";
  return new Date(ms).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
}

// ---------- Capabilities ----------

export type CapabilityState = "available" | "unavailable" | "checking";

export interface Capability {
  id: string;
  label: string;
  state: CapabilityState;
  detail: string;
}

export interface CapabilityInput {
  app: AppInfo | null;
  toolchains: ToolchainInfo[] | null;
  environments: EnvironmentReport | null;
  provider: ProviderStatus | null;
  execution: ExecutionSettingsState | null;
  images: DockerImage[] | null;
}

const checking = (id: string, label: string): Capability => ({ id, label, state: "checking", detail: "Checking…" });

function toolCapability(id: string, label: string, toolchains: ToolchainInfo[] | null, toolIds: string[]): Capability {
  if (!toolchains) return checking(id, label);
  const tools = toolIds.map((t) => ({ id: t, info: toolchains.find((x) => x.id === t) }));
  const missing = tools.find((t) => t.info?.status !== "available");
  if (!missing) {
    return { id, label, state: "available", detail: tools.map((t) => t.info!.version ?? t.info!.name).join("; ") };
  }
  const info = missing.info;
  const reason = info ? (info.detail ?? info.guidance ?? "not found") : "not found";
  return { id, label, state: "unavailable", detail: `${info?.name ?? missing.id}: ${reason}` };
}

const RUNTIME_LABELS: Record<RuntimeLanguage, string> = { python: "Python", r: "R", julia: "Julia" };

function runtimeCapability(language: RuntimeLanguage, environments: EnvironmentReport | null): Capability {
  const id = `research-${language}`;
  const label = `${RUNTIME_LABELS[language]} research (kernels)`;
  if (!environments) return checking(id, label);
  const runtimes = environments.runtimes.filter((r) => r.language === language);
  const usable = runtimes.filter((r) => r.availability.kind === "available");
  if (usable.length) {
    return {
      id,
      label,
      state: "available",
      detail: usable
        .map((r) => (r.version && !r.displayName.includes(r.version) ? `${r.displayName} ${r.version}` : r.displayName))
        .join("; "),
    };
  }
  const first = runtimes[0]?.availability;
  const detail =
    first && first.kind === "unavailable" ? `${first.reason}. ${first.guidance}` : `No ${RUNTIME_LABELS[language]} runtime found`;
  return { id, label, state: "unavailable", detail };
}

function restrictedCapability(input: CapabilityInput): Capability {
  const id = "restricted";
  const label = "Restricted execution (Docker)";
  if (!input.toolchains || !input.execution) return checking(id, label);
  const docker = dockerAvailability(input.toolchains);
  if (docker.kind === "missing") return { id, label, state: "unavailable", detail: "Docker was not found" };
  if (docker.kind === "broken") return { id, label, state: "unavailable", detail: docker.detail };
  const image = imageState(input.execution.settings.docker.image, input.images);
  switch (image.kind) {
    case "notConfigured":
      return { id, label, state: "unavailable", detail: "No container image is configured" };
    case "missing":
      return { id, label, state: "unavailable", detail: `Image ${image.image} is not present locally` };
    case "unverified":
    case "present":
      return { id, label, state: "available", detail: `Image ${image.image}` };
    default:
      return checking(id, label);
  }
}

function agentCapability(provider: ProviderStatus | null): Capability {
  const id = "agents";
  const label = "Agents (model provider)";
  if (!provider) return checking(id, label);
  if (provider.credentialError) return { id, label, state: "unavailable", detail: provider.credentialError };
  if (provider.settings.provider === "scripted") {
    return { id, label, state: "available", detail: "Scripted provider (development only)" };
  }
  return provider.configured
    ? { id, label, state: "available", detail: `${provider.settings.model}; API key in ${provider.credentialStore}` }
    : { id, label, state: "unavailable", detail: "No API key configured" };
}

/** Capability summary for About, derived only from service status. */
export function deriveCapabilities(input: CapabilityInput): Capability[] {
  const { app, toolchains } = input;
  const builtIn = (id: string, label: string, detail: (a: AppInfo) => string): Capability =>
    app ? { id, label, state: "available", detail: detail(app) } : checking(id, label);
  return [
    builtIn("typst", "Typst documents", (a) => `${a.typstCompiler}, built in`),
    builtIn("markdown", "Markdown documents", (a) => `Print profile ${a.markdownProfile}, laid out by ${a.typstCompiler}`),
    toolCapability("pdflatex", "LaTeX (pdfLaTeX)", toolchains, ["pdflatex"]),
    toolCapability("lualatex", "LuaLaTeX", toolchains, ["lualatex"]),
    bibliographyCapability(toolchains),
    runtimeCapability("python", input.environments),
    runtimeCapability("r", input.environments),
    runtimeCapability("julia", input.environments),
    toolCapability("lean", "Lean", toolchains, ["lean", "lake"]),
    restrictedCapability(input),
    agentCapability(input.provider),
  ];
}

function bibliographyCapability(toolchains: ToolchainInfo[] | null): Capability {
  const id = "bibliography";
  const label = "Bibliographies (Biber, BibTeX)";
  if (!toolchains) return checking(id, label);
  const available = ["biber", "bibtex"]
    .map((t) => toolchains.find((x) => x.id === t))
    .filter((t): t is ToolchainInfo => t?.status === "available");
  return available.length
    ? { id, label, state: "available", detail: available.map((t) => t.version ?? t.name).join("; ") }
    : { id, label, state: "unavailable", detail: "Neither Biber nor BibTeX was found" };
}
