import { act, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { setThemePreference } from "../theme";
import {
  FileIcon,
  fileExtension,
  fileIconFor,
  fileIconUrl,
  fileTypeOf,
  ICON_EXTENSIONS,
  ICON_FILE_NAMES,
  ICON_RELATIVE_PATHS,
  resolveFileIcon,
} from "./FileIcon";

// Vitest stubs asset imports, so check the package files on disk.
const nodeFs = "node:fs";
const { readdirSync } = (await import(/* @vite-ignore */ nodeFs)) as { readdirSync: (path: string) => string[] };
const ICON_DIR = "src/ui/components/file-icons";

describe("file-type icons", () => {
  it("maps the app's common types by extension, case-insensitively", () => {
    const cases: [string, string][] = [
      ["main.tex", "tex"],
      ["preamble.STY", "tex"],
      ["paper.typ", "typst"],
      ["README.md", "markdown"],
      ["notes.markdown", "markdown"],
      ["log.txt", "text"],
      ["refs.bib", "bibliography"],
      ["Basic.lean", "lean"],
      ["analysis.py", "python"],
      ["model.R", "r"],
      ["solve.jl", "julia"],
      ["explore.ipynb", "notebook"],
      ["data.csv", "table"],
      ["data.tsv", "table"],
      ["records.json", "json"],
      ["table.parquet", "data"],
      ["arrays.npz", "data"],
      ["config.yaml", "config"],
      ["paper.pdf", "pdf"],
      ["figure.PNG", "image"],
      ["photo.jpeg", "image"],
      ["diagram.svg", "image"],
      ["scan.tiff", "image"],
      ["bundle.zip", "archive"],
    ];
    for (const [name, id] of cases) expect(fileTypeOf(name).id, name).toBe(id);
  });

  it("uses the generic icon for unknown, extensionless, and leading-dot names", () => {
    for (const name of ["Makefile", "LICENSE", ".gitignore", ".env", "archive.unknownext", "trailing."]) {
      expect(fileTypeOf(name).id, name).toBe("generic");
    }
    expect(fileExtension(".gitignore")).toBe("");
    expect(fileExtension("dir.v2/file")).toBe("");
    expect(fileExtension("a/b/c.Tar.GZ")).toBe("gz");
    expect(fileTypeOf("figs/plot.final.png").id).toBe("image");
  });

  it("gives every type an accessible label and a stable component", () => {
    expect(fileTypeOf("main.tex").label).toBe("TeX source");
    expect(fileTypeOf("x.unknown").label).toBe("File");
    expect(fileIconFor("a.tex")).toBe(fileIconFor("b.tex"));
    expect(fileIconFor("a.tex")).not.toBe(fileIconFor("a.typ"));
  });

  it("follows a changed extension immediately", () => {
    const { container, rerender } = render(<FileIcon name="Untitled.txt" />);
    expect(container.querySelector("[data-file-type]")?.getAttribute("data-file-type")).toBe("text");
    rerender(<FileIcon name="Untitled.tex" />);
    expect(container.querySelector("[data-file-type]")?.getAttribute("data-file-type")).toBe("tex");
  });

  it("is decorative next to a file name and labelled when standalone", () => {
    const { container } = render(
      <>
        <FileIcon name="a.lean" />
        <FileIcon name="a.lean" decorative={false} />
      </>,
    );
    const [decorative, labelled] = container.querySelectorAll("[data-file-type]");
    expect(decorative?.getAttribute("aria-hidden")).toBe("true");
    expect(labelled?.getAttribute("role")).toBe("img");
    expect(labelled?.getAttribute("aria-label")).toBe("Lean source");
  });

  it("renders through the Tree/tab icon contract with the type attached", () => {
    const Glyph = fileIconFor("bundle.zip");
    const { container } = render(<Glyph size={14} aria-hidden className="ui-tree-item__icon" />);
    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("data-file-type")).toBe("archive");
    expect(svg?.getAttribute("width")).toBe("14");
    // No badge, pill, or text: an icon only.
    expect(container.textContent).toBe("");
  });

  it("draws a supplied asset as a fixed-size decorative image", () => {
    const Icon = fileIconFor("data.csv");
    const { container } = render(<Icon size={16} aria-hidden className="ui-tree-item__icon" />);
    const img = container.querySelector("img");
    expect(img?.getAttribute("data-file-type")).toBe("table");
    expect(img?.getAttribute("data-file-icon")).toBe("table");
    expect([img?.getAttribute("width"), img?.getAttribute("height")]).toEqual(["16", "16"]);
    expect(img?.getAttribute("alt")).toBe("");
    expect(img?.getAttribute("aria-hidden")).toBe("true");
    expect(img?.className).toContain("ui-tree-item__icon");
    expect(container.textContent).toBe("");
  });

  it("uses the TOML light variant on light themes and follows a theme change", () => {
    setThemePreference("light");
    const { container } = render(<FileIcon name="pyproject.toml" />);
    const src = () => container.querySelector("img")?.getAttribute("src") ?? "";
    const light = fileIconUrl("toml", "light");
    const dark = fileIconUrl("toml", "dark");
    expect(light).not.toBe(dark);
    expect(src()).toBe(light);
    act(() => setThemePreference("ocean"));
    expect(src()).toBe(dark);
    act(() => setThemePreference("sage"));
    expect(src()).toBe(light);
    act(() => setThemePreference("system"));
  });
});

describe("file-icon precedence (supplied package rules)", () => {
  const r = (path: string, kind: "file" | "folder" = "file") => resolveFileIcon(path, kind);

  it("checks node kind first: folders keep the folder treatment, whatever their name", () => {
    expect(r("analysis/legacy.py", "folder").rule).toBe("folder");
    expect(r(".exposition", "folder").rule).toBe("folder");
    expect(r("proofs/lean-toolchain", "folder").rule).toBe("folder");
  });

  it("applies the workspace-relative path override before file names and extensions", () => {
    expect(r(".exposition/project.json")).toMatchObject({ rule: "path", asset: "settings" });
    expect(r(".exposition\\project.json")).toMatchObject({ rule: "path", asset: "settings" });
    expect(r("./.exposition/project.json")).toMatchObject({ rule: "path", asset: "settings" });
  });

  it("does not give every project.json the settings icon", () => {
    expect(r("data/project.json")).toMatchObject({ rule: "extension", asset: "json" });
    expect(r("project.json")).toMatchObject({ rule: "extension", asset: "json" });
    expect(r("data/.exposition/project.json")).toMatchObject({ rule: "extension", asset: "json" });
  });

  it("applies exact file names before extensions, including extensionless ones", () => {
    expect(r("proofs/lean-toolchain")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(r("proofs/lakefile.lean")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(r("proofs/lakefile.toml")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(r("proofs/lake-manifest.json")).toMatchObject({ rule: "filename", asset: "lean" });
    expect(r(".Rprofile")).toMatchObject({ rule: "filename", asset: "r" });
    expect(r(".Renviron")).toMatchObject({ rule: "filename", asset: "r" });
    expect(r(".venv/pyvenv.cfg")).toMatchObject({ rule: "filename", asset: "python" });
    expect(r("other.cfg")).toMatchObject({ rule: "extension", asset: "settings" });
  });

  it("matches extensions case-insensitively and uses the last extension only", () => {
    expect(r("analysis/fit.R")).toMatchObject({ rule: "extension", asset: "r" });
    expect(r("figures/PHOTO.JPG")).toMatchObject({ asset: "image" });
    expect(r("results.RData")).toMatchObject({ asset: "r" });
    expect(r("data/archive.tar.csv")).toMatchObject({ asset: "table" });
  });

  it("keeps the existing glyph for known types the package lacks", () => {
    expect(r("raw/spectra.zip")).toEqual({ rule: "glyph", type: "archive" });
    expect(r("notes/summary.qmd")).toEqual({ rule: "glyph", type: "markdown" });
  });

  it("keeps today's generic glyph for unknown, extensionless, and leading-dot names (approved 2026-11-19)", () => {
    for (const name of ["data/measurement.xyz", "CHANGELOG", ".gitignore"]) expect(r(name), name).toEqual({ rule: "glyph", type: "generic" });
    const { container } = render(<FileIcon name="CHANGELOG" />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")?.getAttribute("data-file-type")).toBe("generic");
  });

  it("maps the manifest's 53 extensions, 7 file names, and 1 path", () => {
    expect(Object.keys(ICON_EXTENSIONS)).toHaveLength(53);
    expect(Object.keys(ICON_FILE_NAMES)).toHaveLength(7);
    expect(Object.keys(ICON_RELATIVE_PATHS)).toEqual([".exposition/project.json"]);
  });

  it("ships exactly the assets the mapping uses, with the notices and license", () => {
    const used = new Set<string>([...Object.values(ICON_EXTENSIONS), ...Object.values(ICON_FILE_NAMES), ...Object.values(ICON_RELATIVE_PATHS), "toml_light"]);
    const shipped = readdirSync(ICON_DIR)
      .filter((f) => f.endsWith(".svg"))
      .map((f) => f.slice(0, -4))
      .sort();
    expect(shipped).toEqual([...used].sort());
    expect(readdirSync(ICON_DIR)).toContain("THIRD_PARTY_NOTICES.md");
    expect(readdirSync(`${ICON_DIR}/licenses`)).toContain("Material-Icon-Theme-MIT.txt");
  });
});
