// File-type icons: the one mapping from a file name to a recognizable icon,
// shared by the Files explorer, the Research explorer, and editor tabs.
//
// Icons come from lucide-react (bundled; no remote assets). They are drawn in
// the surrounding text color, so the shape — never color alone — tells types
// apart, and the file name (with its extension) stays the accessible name of
// the row or tab. An icon is presentation only: it says nothing about a file's
// status, verification, or role in the project graph.
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

/** Stable icon components per type (a new identity per render would remount icons). */
const ICONS = Object.fromEntries(
  (Object.entries(TYPES) as [FileTypeId, (typeof TYPES)[FileTypeId]][]).map(([id, { glyph: Glyph }]) => {
    const Icon: IconComponent = (props) => <Glyph {...props} data-file-type={id} />;
    Icon.displayName = `FileIcon(${id})`;
    return [id, Icon];
  }),
) as Record<FileTypeId, IconComponent>;

/** The extension of a file name, lowercase ("" for none; a leading dot alone is not one). */
export function fileExtension(name: string): string {
  const base = name.slice(name.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/** The file type of a file name or path, by extension; unknown types are generic. */
export function fileTypeOf(name: string): FileTypeInfo {
  const id = BY_EXTENSION.get(fileExtension(name)) ?? "generic";
  return { id, label: TYPES[id].label, icon: ICONS[id] };
}

/** The icon component for a file name (for `Tree`/`EditorTabList` `icon` props). */
export function fileIconFor(name: string): IconComponent {
  return fileTypeOf(name).icon;
}

export interface FileIconProps {
  /** File name or path; the icon follows its extension. */
  name: string;
  /** 14 in trees, tabs, and inline text (the default); 16 in toolbars. */
  size?: 14 | 16;
  /** Decorative (aria-hidden) when the file name is next to it (default true). */
  decorative?: boolean;
  className?: string;
}

/** A standalone file-type icon. Labelled with its type when not decorative. */
export function FileIcon({ name, size = 14, decorative = true, className }: FileIconProps) {
  const type = fileTypeOf(name);
  const Glyph = TYPES[type.id].glyph;
  return decorative ? (
    <Glyph size={size} aria-hidden className={className} data-file-type={type.id} />
  ) : (
    <span role="img" aria-label={type.label} title={type.label} className={className} data-file-type={type.id}>
      <Glyph size={size} aria-hidden />
    </span>
  );
}
