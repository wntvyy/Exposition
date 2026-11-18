// V2 review samples (phase 1): a sample-local file-icon resolver that follows
// the supplied package rules (Exposition_File_Type_Icons, manifest schema 1).
//
// Sample-only: production `src/ui/components/FileIcon.tsx` is unchanged. The
// later production change extends that resolver with this precedence:
//   1. node kind (folders keep the existing folder treatment, even `legacy.py/`);
//   2. exact project-relative path overrides (`.exposition/project.json`);
//   3. exact file names (`lean-toolchain`, `lakefile.lean`, `.Rprofile`, …);
//   4. extension, case-insensitive for presentation (`.R` = `.r`);
//   5. fallback: a type production already knows keeps its current glyph (the
//      package has no asset for it); anything else gets the package's generic
//      `document` icon.
// Icons are presentation only: the mapping grants no format support and never
// reads file contents. Paths and names are compared without being rewritten.
import { fileTypeOf, type FileTypeId } from "../../components/FileIcon";

/** The 23 package assets (file names without `.svg`). */
export type IconAsset =
  | "bibliography"
  | "bibtex-style"
  | "database"
  | "document"
  | "html"
  | "image"
  | "json"
  | "julia"
  | "jupyter"
  | "lean"
  | "markdown"
  | "matlab"
  | "pdf"
  | "python"
  | "r"
  | "settings"
  | "svg"
  | "table"
  | "tex"
  | "toml"
  | "toml_light"
  | "typst"
  | "yaml";

type MappedAsset = Exclude<IconAsset, "toml_light">;

/** Extension → asset (keys lowercase, no dot). The manifest's 53 associations. */
export const EXTENSIONS: Readonly<Record<string, MappedAsset>> = {
  arrow: "database",
  avif: "image",
  bib: "bibliography",
  bmp: "image",
  bst: "bibtex-style",
  cfg: "settings",
  cls: "tex",
  csv: "table",
  eps: "image",
  feather: "database",
  gif: "image",
  h5: "database",
  hdf5: "database",
  html: "html",
  ico: "image",
  ini: "settings",
  ipy: "python",
  ipynb: "jupyter",
  jl: "julia",
  jpeg: "image",
  jpg: "image",
  json: "json",
  jsonl: "json",
  lean: "lean",
  ltx: "tex",
  markdown: "markdown",
  mat: "matlab",
  md: "markdown",
  nc: "database",
  npy: "database",
  npz: "database",
  org: "document",
  parquet: "database",
  pdf: "pdf",
  png: "image",
  py: "python",
  pyw: "python",
  r: "r",
  rdata: "r",
  rds: "r",
  rst: "document",
  sty: "tex",
  svg: "svg",
  tex: "tex",
  toml: "toml",
  tsv: "table",
  txt: "document",
  typ: "typst",
  webp: "image",
  xls: "table",
  xlsx: "table",
  yaml: "yaml",
  yml: "yaml",
};

/** Exact file names (any folder). */
export const FILE_NAMES: Readonly<Record<string, MappedAsset>> = {
  "lakefile.lean": "lean",
  "lakefile.toml": "lean",
  "lean-toolchain": "lean",
  "lake-manifest.json": "lean",
  ".Rprofile": "r",
  ".Renviron": "r",
  "pyvenv.cfg": "python",
};

/** Exact project-relative paths. */
export const RELATIVE_PATHS: Readonly<Record<string, MappedAsset>> = {
  ".exposition/project.json": "settings",
};

export const UNKNOWN_ASSET: MappedAsset = "document";

export type NodeKind = "file" | "folder";
export type ThemeKind = "light" | "dark";
export type IconRule = "folder" | "path" | "filename" | "extension" | "existing-glyph" | "generic";

export interface IconResolution {
  rule: IconRule;
  /** The package asset to draw (absent for folders and existing glyphs). */
  asset?: IconAsset;
  /** For `existing-glyph`: the production FileIcon type that keeps its glyph. */
  legacyType?: FileTypeId;
  /** The matched key (path, file name, or extension) for the sample's table. */
  match?: string;
}

/** Normalize separators and a leading "./" for comparison only. */
function comparablePath(path: string): string {
  return path.replaceAll("\\", "/").replace(/^\.\//, "");
}

function baseName(path: string): string {
  const p = comparablePath(path);
  return p.slice(p.lastIndexOf("/") + 1);
}

/** Lowercase extension of a base name ("" when none; a leading dot alone is not one). */
export function iconExtension(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

function themed(asset: MappedAsset, theme: ThemeKind): IconAsset {
  return asset === "toml" && theme === "light" ? "toml_light" : asset;
}

function lookupInsensitive(table: Readonly<Record<string, MappedAsset>>, key: string): [string, MappedAsset] | undefined {
  // Exact first; then ASCII case-insensitive, as graph admission compares
  // these names (and Windows file names are case-insensitive).
  const exact = table[key];
  if (exact) return [key, exact];
  const lower = key.toLowerCase();
  for (const [k, v] of Object.entries(table)) if (k.toLowerCase() === lower) return [k, v];
  return undefined;
}

/**
 * Resolve the icon for a project node. `path` is project-relative (any
 * separator). Folders always resolve to the folder treatment.
 */
export function resolveFileIcon(path: string, kind: NodeKind, theme: ThemeKind): IconResolution {
  if (kind === "folder") return { rule: "folder" };
  const rel = comparablePath(path);
  const byPath = lookupInsensitive(RELATIVE_PATHS, rel);
  if (byPath) return { rule: "path", asset: themed(byPath[1], theme), match: byPath[0] };
  const name = baseName(rel);
  const byName = lookupInsensitive(FILE_NAMES, name);
  if (byName) return { rule: "filename", asset: themed(byName[1], theme), match: byName[0] };
  const ext = iconExtension(name);
  const byExt = ext ? EXTENSIONS[ext] : undefined;
  if (byExt) return { rule: "extension", asset: themed(byExt, theme), match: `.${ext}` };
  const legacy = fileTypeOf(name).id;
  if (legacy !== "generic") return { rule: "existing-glyph", legacyType: legacy, match: ext ? `.${ext}` : name };
  return { rule: "generic", asset: UNKNOWN_ASSET };
}
