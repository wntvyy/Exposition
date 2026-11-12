// Pure project-settings logic (`.exposition/project.json`): LaTeX targets,
// the engine a .tex file builds with, and edits that keep the file valid for
// the workspace service's checks (unique 1–64 character names, a defined
// default target, latex + pdfLaTeX/LuaLaTeX). Other targets are kept as they are.
import type { ProjectSettings } from "../../bindings/ProjectSettings";
import type { ProjectTarget } from "../../bindings/ProjectTarget";

export type LatexEngine = "pdfLatex" | "luaLatex";

export const LATEX_ENGINES: { id: LatexEngine; label: string }[] = [
  { id: "pdfLatex", label: "pdfLaTeX" },
  { id: "luaLatex", label: "LuaLaTeX" },
];

export function engineLabel(engine: string): string {
  return LATEX_ENGINES.find((e) => e.id === engine)?.label ?? engine;
}

export const isTexPath = (path: string | null | undefined): path is string => !!path && /\.tex$/i.test(path);

export function latexTargets(settings: ProjectSettings): ProjectTarget[] {
  return settings.targets.filter((t) => t.format === "latex" && (t.engine === "pdfLatex" || t.engine === "luaLatex"));
}

/**
 * The target the TeX service builds `entry` with (as `TexService::target_for_entry`):
 * the default target when it builds this entry, else the first LaTeX target
 * for it; with none, pdfLaTeX.
 */
export function targetForEntry(
  settings: ProjectSettings,
  entry: string,
): { target: ProjectTarget | null; engine: LatexEngine } {
  const candidates = latexTargets(settings).filter((t) => t.entry === entry);
  const target = candidates.find((t) => t.name === settings.defaultTarget) ?? candidates[0] ?? null;
  return { target, engine: (target?.engine as LatexEngine | undefined) ?? "pdfLatex" };
}

/** `base`, or `base 2`, `base 3`, … — a name no other target uses. */
export function uniqueName(base: string, targets: ProjectTarget[]): string {
  const taken = new Set(targets.map((t) => t.name.trim()));
  const stem = base.trim().slice(0, 60) || "target";
  if (!taken.has(stem)) return stem;
  for (let n = 2; ; n++) if (!taken.has(`${stem} ${n}`)) return `${stem} ${n}`;
}

/** A workspace-relative path as the service stores it (forward slashes, no `./`). */
export function normalizeEntry(raw: string): string {
  return raw
    .trim()
    .replace(/\\/g, "/")
    .split("/")
    .filter((part) => part !== "" && part !== ".")
    .join("/");
}

const INVALID_CHARS = /[<>:"|?*\u0000-\u001f]/;

export interface TargetDraft {
  name: string;
  entry: string;
  engine: LatexEngine;
}

/** Why a draft cannot be saved (null: it can). `editing` is the name being replaced. */
export function validateDraft(draft: TargetDraft, targets: ProjectTarget[], editing: string | null): string | null {
  const name = draft.name.trim();
  if (!name) return "Enter a name.";
  if (name.length > 64) return "Names are at most 64 characters.";
  if (targets.some((t) => t.name.trim() === name && t.name !== editing)) return `A target named “${name}” exists already.`;
  const raw = draft.entry.trim();
  if (!raw) return "Enter the main .tex file, relative to the project folder.";
  if (/^([a-z]:|[\\/])/i.test(raw)) return "Use a path relative to the project folder, such as paper/main.tex.";
  const entry = normalizeEntry(raw);
  const parts = entry.split("/");
  if (parts.includes("..")) return "The file must be inside the project folder.";
  if (parts.some((p) => INVALID_CHARS.test(p) || /[. ]$/.test(p))) return "That is not a valid file path.";
  if (!isTexPath(entry)) return "The main file of a LaTeX target is a .tex file.";
  return null;
}

/** Replace the target named `editing` (or add one), keeping the default pointing at it. */
export function upsertTarget(settings: ProjectSettings, editing: string | null, draft: TargetDraft): ProjectSettings {
  const target: ProjectTarget = {
    name: draft.name.trim(),
    format: "latex",
    engine: draft.engine,
    entry: normalizeEntry(draft.entry),
  };
  const index = editing === null ? -1 : settings.targets.findIndex((t) => t.name === editing);
  const targets =
    index < 0 ? [...settings.targets, target] : settings.targets.map((t, i) => (i === index ? target : t));
  const defaultTarget = editing !== null && settings.defaultTarget === editing ? target.name : settings.defaultTarget;
  return { ...settings, targets, defaultTarget };
}

export function removeTarget(settings: ProjectSettings, name: string): ProjectSettings {
  return {
    ...settings,
    targets: settings.targets.filter((t) => t.name !== name),
    defaultTarget: settings.defaultTarget === name ? null : settings.defaultTarget,
  };
}

export function setDefaultTarget(settings: ProjectSettings, name: string | null): ProjectSettings {
  return { ...settings, defaultTarget: name };
}

/**
 * Build `entry` with `engine`: change the target the service uses for it, or
 * add a target named after the file when there is none.
 */
export function withEntryEngine(settings: ProjectSettings, entry: string, engine: LatexEngine): ProjectSettings {
  const { target } = targetForEntry(settings, entry);
  if (target) {
    return { ...settings, targets: settings.targets.map((t) => (t === target ? { ...t, engine } : t)) };
  }
  const stem = entry.split("/").pop()?.replace(/\.tex$/i, "") ?? "main";
  const name = uniqueName(stem, settings.targets);
  return { ...settings, targets: [...settings.targets, { name, format: "latex", engine, entry }] };
}
