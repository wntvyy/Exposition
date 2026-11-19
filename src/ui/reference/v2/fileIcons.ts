// V2 review samples: the file-icon view the samples draw with. Since the
// selection (2026-11-19) the mapping and assets are production's
// (`src/ui/components/FileIcon.tsx`, `file-icons/`); this adapter only adds the
// sample's explicit theme parameter and rule names for the icon table.
import {
  fileIconUrl,
  ICON_EXTENSIONS,
  ICON_FILE_NAMES,
  ICON_RELATIVE_PATHS,
  resolveFileIcon as resolveProduction,
  type FileIconAsset,
  type FileTypeId,
} from "../../components/FileIcon";

export type IconAsset = FileIconAsset;
export type NodeKind = "file" | "folder";
export type ThemeKind = "light" | "dark";
export type IconRule = "folder" | "path" | "filename" | "extension" | "existing-glyph" | "generic";

export const EXTENSIONS = ICON_EXTENSIONS;
export const FILE_NAMES = ICON_FILE_NAMES;
export const RELATIVE_PATHS = ICON_RELATIVE_PATHS;

export interface IconResolution {
  rule: IconRule;
  /** The package asset to draw, light variant applied (absent for folders and glyphs). */
  asset?: IconAsset;
  /** For glyph rules: the production file type whose glyph is drawn. */
  legacyType?: FileTypeId;
  /** The matched key (path, file name, or extension) for the sample's table. */
  match?: string;
}

export function resolveFileIcon(path: string, kind: NodeKind, theme: ThemeKind): IconResolution {
  const r = resolveProduction(path, kind);
  if (r.rule === "folder") return { rule: "folder" };
  if (r.rule === "glyph") return { rule: r.type === "generic" ? "generic" : "existing-glyph", legacyType: r.type };
  const asset: IconAsset = r.asset === "toml" && theme === "light" ? "toml_light" : r.asset!;
  return { rule: r.rule, asset, match: r.match };
}

export function iconUrl(asset: IconAsset): string {
  return asset === "toml_light" ? fileIconUrl("toml", "light") : fileIconUrl(asset, "dark");
}
