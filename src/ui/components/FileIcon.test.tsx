import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FileIcon, fileExtension, fileIconFor, fileTypeOf } from "./FileIcon";

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
    const Icon = fileIconFor("data.csv");
    const { container } = render(<Icon size={14} aria-hidden className="ui-tree-item__icon" />);
    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("data-file-type")).toBe("table");
    expect(svg?.getAttribute("width")).toBe("14");
    // No badge, pill, or text: an icon only.
    expect(container.textContent).toBe("");
  });
});
