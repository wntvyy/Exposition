// File-type icons: the one mapping from a project file to a recognizable icon,
// shared by the Files explorer, the Research explorer, editor tabs, and chips.
//
// Resolution order (UI standard V2, package Exposition_File_Type_Icons,
// manifest schema 1):
//   1. node kind: folders keep the existing folder treatment (callers draw it);
//   2. exact workspace-relative paths (`.exposition/project.json` only);
//   3. exact file names (`lean-toolchain`, `lakefile.lean`, `.Rprofile`, ...);
//   4. extension, case-insensitive (`.R` = `.r`);
//   5. otherwise the lucide glyph of the file's type below; unknown files keep
//      the generic file glyph (approved 2026-11-19).
// Steps 2-4 draw the supplied Material Icon Theme SVGs (MIT, unmodified; see
// `file-icons/THIRD_PARTY_NOTICES.md`) as bundled assets. Brand marks are never
// recolored; TOML uses its light variant on light themes.
//
// An icon is presentation only: it grants no format support and says nothing
// about a file's status, verification, or role in the project graph. The file
// name stays the accessible name of the row or tab; icons are decorative there.
import {
  BookMarked,
  File,
  FileArchive,
  FileAudio,
  FileCode2,
  FileCog,
  FileDatabase,
  FileImage,
  FileJson,
  FileOutput,
  FilePenLine,
  FileSpreadsheet,
  FileText,
  FileType,
  FileType2,
  FileVideo,
  FunctionSquare,
  NotebookText,
} from "lucide-react";
import type { ComponentType } from "react";

import { useTheme } from "../theme";
import { themeKinds } from "../tokens";
import type { IconComponent } from "./Button";

export type FileTypeId =
  | "tex"
  | "typst"
  | "markdown"
  | "text"
  | "bibliography"
  | "lean"
  | "python"
  | "r"
  | "julia"
  | "notebook"
  | "table"
  | "json"
  | "config"
  | "data"
  | "pdf"
  | "image"
  | "archive"
  | "audio"
  | "video"
  | "generic";

export interface FileTypeInfo {
  id: FileTypeId;
  /** Accessible description of the type ("TeX source"). */
  label: string;
  /** The icon, ready for `Tree`/`EditorTabList` `icon` props. */
  icon: IconComponent;
}

type LucideIcon = ComponentType<{ size?: number; "aria-hidden"?: boolean; className?: string; "data-file-type"?: string }>;

const TYPES: Record<FileTypeId, { label: string; glyph: LucideIcon; extensions: string[] }> = {
  tex: { label: "TeX source", glyph: FileType, extensions: ["tex", "ltx", "latex", "sty", "cls", "dtx", "ins", "bbx", "cbx"] },
  typst: { label: "Typst source", glyph: FileType2, extensions: ["typ"] },
  markdown: { label: "Markdown", glyph: FilePenLine, extensions: ["md", "markdown", "mdx", "qmd", "rmd"] },
  text: { label: "Text", glyph: FileText, extensions: ["txt", "text", "log", "rst", "org", "adoc"] },
  bibliography: { label: "Bibliography", glyph: BookMarked, extensions: ["bib", "bibtex", "ris"] },
  lean: { label: "Lean source", glyph: FunctionSquare, extensions: ["lean"] },
  python: { label: "Python script", glyph: FileCode2, extensions: ["py", "pyw", "ipy", "pyi"] },
  r: { label: "R script", glyph: FileCode2, extensions: ["r"] },
  julia: { label: "Julia script", glyph: FileCode2, extensions: ["jl"] },
  notebook: { label: "Notebook", glyph: NotebookText, extensions: ["ipynb"] },
  table: { label: "Table", glyph: FileSpreadsheet, extensions: ["csv", "tsv", "xlsx", "xls", "ods"] },
  json: { label: "JSON data", glyph: FileJson, extensions: ["json", "jsonl", "ndjson", "geojson"] },
  config: { label: "Configuration", glyph: FileCog, extensions: ["toml", "yaml", "yml", "ini", "cfg", "conf"] },
  data: {
    label: "Data file",
    glyph: FileDatabase,
    extensions: ["parquet", "feather", "arrow", "h5", "hdf5", "nc", "npy", "npz", "rds", "rdata", "mat", "sqlite", "db", "dta", "sav", "pkl", "pickle"],
  },
  pdf: { label: "PDF document", glyph: FileOutput, extensions: ["pdf"] },
  image: {
    label: "Image",
    glyph: FileImage,
    extensions: ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "ico", "tif", "tiff", "heic", "heif", "eps"],
  },
  archive: { label: "Archive", glyph: FileArchive, extensions: ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "zst"] },
  audio: { label: "Audio", glyph: FileAudio, extensions: ["wav", "mp3", "flac", "ogg", "m4a"] },
  video: { label: "Video", glyph: FileVideo, extensions: ["mp4", "mov", "webm", "mkv", "avi"] },
  generic: { label: "File", glyph: File, extensions: [] },
};

const BY_EXTENSION = new Map<string, FileTypeId>();
for (const [id, type] of Object.entries(TYPES) as [FileTypeId, (typeof TYPES)[FileTypeId]][]) {
  for (const ext of type.extensions) BY_EXTENSION.set(ext, id);
}

/** The extension of a file name, lowercase ("" for none; a leading dot alone is not one). */
export function fileExtension(name: string): string {
  const base = name.slice(name.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/** The file type of a file name or path, by extension; unknown types are generic. */
export function fileTypeOf(name: string): FileTypeInfo {
  const id = BY_EXTENSION.get(fileExtension(name)) ?? "generic";
  return { id, label: TYPES[id].label, icon: iconFor(resolveFileIcon(name)) };
}

// ---- Supplied assets (Exposition_File_Type_Icons)

/** The package's 23 SVGs, by file name without `.svg`. */
export type FileIconAsset =
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

export type MappedFileIconAsset = Exclude<FileIconAsset, "toml_light">;

/** Extension (lowercase, no dot) -> asset: the manifest's 53 associations. */
export const ICON_EXTENSIONS: Readonly<Record<string, MappedFileIconAsset>> = {
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

/** Exact file names, in any folder. */
export const ICON_FILE_NAMES: Readonly<Record<string, MappedFileIconAsset>> = {
  "lakefile.lean": "lean",
  "lakefile.toml": "lean",
  "lean-toolchain": "lean",
  "lake-manifest.json": "lean",
  ".Rprofile": "r",
  ".Renviron": "r",
  "pyvenv.cfg": "python",
};

/** Exact workspace-relative paths. */
export const ICON_RELATIVE_PATHS: Readonly<Record<string, MappedFileIconAsset>> = {
  ".exposition/project.json": "settings",
};

export type FileNodeKind = "file" | "folder";
export type FileIconRule = "folder" | "path" | "filename" | "extension" | "glyph";

export interface FileIconResolution {
  rule: FileIconRule;
  /** The supplied asset (steps 2-4), before the light-theme TOML variant. */
  asset?: MappedFileIconAsset;
  /** The file type (label, glyph fallback). */
  type: FileTypeId;
  /** The matched key (path, file name, or `.ext`). */
  match?: string;
}

/** Separators and a leading "./" normalized, for comparison only. */
function comparablePath(path: string): string {
  return path.replaceAll("\\", "/").replace(/^\.\//, "");
}

function lookup(table: Readonly<Record<string, MappedFileIconAsset>>, key: string): [string, MappedFileIconAsset] | undefined {
  // Exact first, then ASCII case-insensitive (Windows names are case-insensitive).
  const exact = table[key];
  if (exact) return [key, exact];
  const lower = key.toLowerCase();
  for (const [k, v] of Object.entries(table)) if (k.toLowerCase() === lower) return [k, v];
  return undefined;
}

/**
 * Resolve the icon for a project node. `path` is workspace-relative (any
 * separator) or a bare file name; a bare name never matches a path override.
 */
export function resolveFileIcon(path: string, kind: FileNodeKind = "file"): FileIconResolution {
  const rel = comparablePath(path);
  const name = rel.slice(rel.lastIndexOf("/") + 1);
  const type = BY_EXTENSION.get(fileExtension(name)) ?? "generic";
  if (kind === "folder") return { rule: "folder", type };
  const byPath = lookup(ICON_RELATIVE_PATHS, rel);
  if (byPath) return { rule: "path", asset: byPath[1], type, match: byPath[0] };
  const byName = lookup(ICON_FILE_NAMES, name);
  if (byName) return { rule: "filename", asset: byName[1], type, match: byName[0] };
  const ext = fileExtension(name);
  const byExt = ext ? ICON_EXTENSIONS[ext] : undefined;
  if (byExt) return { rule: "extension", asset: byExt, type, match: `.${ext}` };
  return { rule: "glyph", type };
}

const ASSET_URLS = import.meta.glob<string>("./file-icons/*.svg", { eager: true, query: "?url", import: "default" });

/** The bundled URL of an asset, with TOML's light variant on light themes. */
export function fileIconUrl(asset: MappedFileIconAsset, themeKind: "light" | "dark"): string {
  const file = asset === "toml" && themeKind === "light" ? "toml_light" : asset;
  return ASSET_URLS[`./file-icons/${file}.svg`] ?? "";
}

interface DrawProps {
  size?: number;
  "aria-hidden"?: boolean;
  className?: string;
}

function AssetImage({ asset, type, size = 14, className }: DrawProps & { asset: MappedFileIconAsset; type: FileTypeId }) {
  const kind = useTheme((s) => themeKinds[s.resolved]);
  return (
    <img
      src={fileIconUrl(asset, kind)}
      width={size}
      height={size}
      alt=""
      aria-hidden
      draggable={false}
      className={className ? `ui-file-icon ${className}` : "ui-file-icon"}
      data-file-type={type}
      data-file-icon={asset}
    />
  );
}

/** Stable icon components per drawing (a new identity per render would remount icons). */
const ICONS = new Map<string, IconComponent>();

function iconFor(r: FileIconResolution): IconComponent {
  const key = r.asset ? `asset:${r.asset}:${r.type}` : `glyph:${r.type}`;
  let Icon = ICONS.get(key);
  if (!Icon) {
    const { asset, type } = r;
    if (asset) {
      Icon = (props) => <AssetImage {...props} asset={asset} type={type} />;
    } else {
      const Glyph = TYPES[type].glyph;
      Icon = (props) => <Glyph {...props} data-file-type={type} />;
    }
    Icon.displayName = `FileIcon(${asset ?? type})`;
    ICONS.set(key, Icon);
  }
  return Icon;
}

/** The icon component for a file (workspace-relative path or name), for `Tree`/`EditorTabList` `icon` props. */
export function fileIconFor(path: string): IconComponent {
  return iconFor(resolveFileIcon(path));
}

export interface FileIconProps {
  /** Workspace-relative path or file name; the icon follows it. */
  name: string;
  /** 14 in trees, tabs, and inline text (the default); 16 in toolbars. */
  size?: 14 | 16;
  /** Decorative (aria-hidden) when the file name is next to it (default true). */
  decorative?: boolean;
  className?: string;
}

/** A standalone file-type icon. Labelled with its type when not decorative. */
export function FileIcon({ name, size = 14, decorative = true, className }: FileIconProps) {
  const r = resolveFileIcon(name);
  const Icon = iconFor(r);
  return decorative ? (
    <Icon size={size} aria-hidden className={className} />
  ) : (
    <span role="img" aria-label={TYPES[r.type].label} title={TYPES[r.type].label} className={className} data-file-type={r.type}>
      <Icon size={size} aria-hidden />
    </span>
  );
}
