import { describe, expect, it } from "vitest";

import { EXTENSIONS, FILE_NAMES, RELATIVE_PATHS, resolveFileIcon, type IconAsset } from "./fileIcons";

// Vitest stubs asset imports, so check the copied package files on disk.
const nodeFs = "node:fs";
const { readdirSync } = (await import(/* @vite-ignore */ nodeFs)) as { readdirSync: (path: string) => string[] };
const ICON_DIR = "src/ui/reference/v2/icons";

const asset = (path: string, kind: "file" | "folder" = "file", theme: "light" | "dark" = "dark") =>
  resolveFileIcon(path, kind, theme);

describe("sample file-icon resolver precedence", () => {
  it("checks node kind first: folders keep the folder treatment, whatever their name", () => {
    expect(asset("analysis/legacy.py", "folder")).toEqual({ rule: "folder" });
    expect(asset(".exposition", "folder")).toEqual({ rule: "folder" });
    expect(asset("proofs/lean-toolchain", "folder").rule).toBe("folder");
  });

  it("applies the project-relative path override before file names and extensions", () => {
    expect(asset(".exposition/project.json")).toMatchObject({ rule: "path", asset: "settings" });
    expect(asset(".exposition\\project.json")).toMatchObject({ rule: "path", asset: "settings" });
    expect(asset("./.exposition/project.json")).toMatchObject({ rule: "path", asset: "settings" });
  });

  it("does not give every project.json the settings icon", () => {
    expect(asset("data/project.json")).toMatchObject({ rule: "extension", asset: "json" });
    expect(asset("project.json")).toMatchObject({ rule: "extension", asset: "json" });
    expect(asset("data/.exposition/project.json")).toMatchObject({ rule: "extension", asset: "json" });
  });

  it("applies exact file names before extensions, including extensionless ones", () => {
    expect(asset("proofs/lean-toolchain")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(asset("proofs/lakefile.lean")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(asset("proofs/lakefile.toml")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(asset("proofs/lake-manifest.json")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(asset(".Rprofile")).toMatchObject({ rule: "filename", asset: "r" });
    expect(asset(".Renviron")).toMatchObject({ rule: "filename", asset: "r" });
    expect(asset(".venv/pyvenv.cfg")).toMatchObject({ rule: "filename", asset: "python" });
    expect(asset("other.cfg")).toMatchObject({ rule: "extension", asset: "settings" });
  });

  it("matches extensions case-insensitively", () => {
    expect(asset("analysis/fit.R")).toMatchObject({ rule: "extension", asset: "r" });
    expect(asset("analysis/fit.r")).toMatchObject({ rule: "extension", asset: "r" });
    expect(asset("figures/PHOTO.JPG")).toMatchObject({ asset: "image" });
    expect(asset("results.RData")).toMatchObject({ asset: "r" });
  });

  it("uses the last extension only and ignores a leading dot", () => {
    expect(asset("data/archive.tar.csv")).toMatchObject({ asset: "table" });
    expect(asset(".gitignore")).toMatchObject({ rule: "generic", asset: "document" });
  });

  it("uses the TOML light variant on light themes only", () => {
    expect(resolveFileIcon("pyproject.toml", "file", "light").asset).toBe("toml_light");
    expect(resolveFileIcon("pyproject.toml", "file", "dark").asset).toBe("toml");
    expect(resolveFileIcon("pyproject.yaml", "file", "light").asset).toBe("yaml");
  });

  it("keeps the existing glyph for types production knows but the package lacks", () => {
    expect(asset("raw/spectra.zip")).toMatchObject({ rule: "existing-glyph", legacyType: "archive" });
    expect(asset("notes/summary.qmd")).toMatchObject({ rule: "existing-glyph", legacyType: "markdown" });
  });

  it("falls back to the generic icon for unknown extensions and extensionless names", () => {
    expect(asset("data/measurement.xyz")).toMatchObject({ rule: "generic", asset: "document" });
    expect(asset("CHANGELOG")).toMatchObject({ rule: "generic", asset: "document" });
  });

  it("maps the manifest's 53 extensions, 7 file names, and 1 path", () => {
    expect(Object.keys(EXTENSIONS)).toHaveLength(53);
    expect(Object.keys(FILE_NAMES)).toHaveLength(7);
    expect(Object.keys(RELATIVE_PATHS)).toEqual([".exposition/project.json"]);
  });

  it("ships exactly the assets the mapping uses", () => {
    const used = new Set<IconAsset>([...Object.values(EXTENSIONS), ...Object.values(FILE_NAMES), ...Object.values(RELATIVE_PATHS), "toml_light"]);
    const shipped = readdirSync(ICON_DIR)
      .filter((f) => f.endsWith(".svg"))
      .map((f) => f.slice(0, -4))
      .sort();
    expect(shipped).toEqual([...used].sort());
    expect(readdirSync(ICON_DIR)).toContain("THIRD_PARTY_NOTICES.md");
    expect(readdirSync(`${ICON_DIR}/licenses`)).toContain("Material-Icon-Theme-MIT.txt");
  });
});
