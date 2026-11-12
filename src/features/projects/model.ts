// Pure helpers for the New Project page (unit-tested; no IPC here).
import type { CapabilityReport } from "../../bindings/CapabilityReport";
import type { CapabilityStatus } from "../../bindings/CapabilityStatus";
import type { ManuscriptFormat } from "../../bindings/ManuscriptFormat";
import type { ProjectBlueprint } from "../../bindings/ProjectBlueprint";
import type { ProjectPresetInfo } from "../../bindings/ProjectPresetInfo";
import type { ProjectPresetRequest } from "../../bindings/ProjectPresetRequest";
import type { ResearchRuntime } from "../../bindings/ResearchRuntime";

export const NEW_PROJECT_KEY = "new-project:";
export const SETUP_KEY_PREFIX = "project-setup:";
/** The minimal preset the objective path starts from. */
export const EMPTY_PRESET = "empty";

export const FORMAT_LABELS: Record<ManuscriptFormat, string> = {
  typst: "Typst",
  latexPdf: "LaTeX (pdfLaTeX)",
  latexLua: "LaTeX (LuaLaTeX)",
  markdown: "Markdown (print profile)",
};

export const RUNTIME_LABELS: Record<ResearchRuntime, string> = {
  python: "Python",
  r: "R",
  julia: "Julia",
};

const RESERVED = /^(con|prn|aux|nul|com[0-9¹²³]|lpt[0-9¹²³]|conin\$|conout\$|clock\$)$/i;

/** A safe folder name derived from a project title. */
export function folderNameFor(title: string): string {
  const cleaned = title
    .normalize("NFC")
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, " ")
    .trim()
    .toLowerCase()
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^[.-]+|[.\s-]+$/g, "")
    .slice(0, 60)
    .replace(/[.\s-]+$/g, "");
  if (!cleaned) return "new-project";
  const stem = cleaned.split(".")[0] ?? cleaned;
  return RESERVED.test(stem) ? `${cleaned}-project` : cleaned;
}

/** Join a parent folder and a child name with the parent's separator style. */
export function joinPath(parent: string, name: string): string {
  const trimmed = parent.replace(/[\\/]+$/, "");
  const sep = parent.includes("\\") || /^[A-Za-z]:$/.test(trimmed) ? "\\" : "/";
  return `${trimmed}${sep}${name}`;
}

/** The folder that contains `path` (for re-deriving a location from a new title). */
export function parentOf(path: string): string | null {
  const m = /^(.*)[\\/][^\\/]+[\\/]?$/.exec(path.trim());
  return m && m[1] ? m[1] : null;
}

/** The request for a preset with the page's options. */
export function presetRequest(
  preset: ProjectPresetInfo,
  options: {
    title: string;
    format: ManuscriptFormat;
    runtime: ResearchRuntime;
    leanToolchain: string | null;
    includeWorkflow: boolean;
  },
): ProjectPresetRequest {
  return {
    preset: preset.id,
    title: options.title.trim() || null,
    format: preset.manuscript ? options.format : null,
    runtime: preset.computation ? options.runtime : null,
    leanToolchain: preset.lean ? options.leanToolchain : null,
    includeWorkflow: options.includeWorkflow && preset.workflow !== null,
    purpose: null,
  };
}

/**
 * The agent task that organizes a project created from an objective. The
 * task runs in the new project only; it asks for a proposal, allows a
 * research-only project, and excludes installs and code execution.
 */
export function objectivePrompt(objective: string): string {
  return [
    `New project objective: ${objective.trim()}`,
    "",
    "This folder is a new, empty Exposition project created for this objective. Propose a suitable organization and then create it with your file and project-graph tools:",
    "- the files and folders it needs (a manuscript only if the objective calls for one; a research-only project is fine),",
    "- a research space with sources, data, and notes when the work is computational or exploratory,",
    "- meaningful entities and typed relations (questions, claims, definitions, experiments, collections) and workflow drafts where useful.",
    "Explain the organization briefly before creating it. Do not install software and do not run code.",
  ].join("\n");
}

/**
 * Availability of what a blueprint declares, on this computer (null while
 * discovery has not answered). Lean is checked against the exact toolchain
 * the blueprint's `lean-toolchain` names.
 */
export function declaredCapabilities(blueprint: ProjectBlueprint, report: CapabilityReport | null): CapabilityStatus[] | null {
  if (!report) return null;
  const out: CapabilityStatus[] = [];
  for (const declared of blueprint.capabilities) {
    const host = report.capabilities.find((c) => c.capability === declared.capability);
    if (!host) continue;
    const pin = blueprint.files.find((f) => f.path === "lean-toolchain")?.content.trim();
    if (declared.capability === "lean" && pin) {
      const installed = report.leanToolchains.includes(pin);
      out.push({
        ...host,
        available: installed,
        detail: installed ? `${pin} is installed` : `${pin} is not installed; checking Lean files will report it as missing`,
        guidance: installed
          ? null
          : `Install it with \`elan toolchain install ${pin}\` (or from the Lean panel). Exposition never installs it implicitly.`,
      });
    } else {
      out.push(host);
    }
  }
  return out;
}

export function setupKey(operation: string): string {
  return `${SETUP_KEY_PREFIX}${operation}`;
}
