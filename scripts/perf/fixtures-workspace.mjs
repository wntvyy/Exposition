// Deterministic fixtures for the editor and explorer measurements
// (scripts/perf/app/editor-typing.mjs). Same inputs give the same files.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { writeProject } from "./fixtures.mjs";

/** Document and folder sizes per scale (EXPOSITION_PERF_SCALE). */
export const EDITOR_SCALES = {
  small: { largeBytes: 1024 * 1024, flatFiles: 1_000, treeFiles: 200 },
  large: { largeBytes: 5 * 1024 * 1024, flatFiles: 10_000, treeFiles: 2_000 },
  stress: { largeBytes: 10 * 1024 * 1024, flatFiles: 100_000, treeFiles: 10_000 },
};

// A 1x1 PNG.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
  "base64",
);

/**
 * A Markdown document of about `bytes` bytes: one heading per section (so the
 * outline has about bytes/500 items) and an image reference every
 * `imageEvery` sections (so inline previews are active).
 */
export function markdownDocument({ bytes, imageEvery = 400 }) {
  const parts = ["# Measurements\n\n"];
  let size = parts[0].length;
  const filler = "The quick brown fox jumps over the lazy dog while the proof of the lemma follows by induction. ";
  for (let i = 0; size < bytes; i++) {
    let block = `## Section ${i}\n\n${filler.repeat(4)}\n\n`;
    if (i % imageEvery === 0) block += `![Figure ${i}](fig.png)\n\n`;
    parts.push(block);
    size += block.length;
  }
  return parts.join("");
}

/**
 * Writes a project for the editor measurements and returns its description:
 * `small.md` (2 KiB), `large.md` (`largeBytes`), `fig.png`, a folder `flat/`
 * with `flatFiles` files, and a nested tree `tree/` with `treeFiles` files.
 */
export function writeEditorProject(root, { largeBytes, flatFiles, treeFiles }) {
  mkdirSync(join(root, "flat"), { recursive: true });
  writeFileSync(join(root, "main.typ"), "= Main\n\nBody.\n");
  writeFileSync(join(root, "small.md"), markdownDocument({ bytes: 2_000 }));
  writeFileSync(join(root, "large.md"), markdownDocument({ bytes: largeBytes }));
  writeFileSync(join(root, "fig.png"), PNG);
  for (let i = 0; i < flatFiles; i++) writeFileSync(join(root, "flat", `f${String(i).padStart(6, "0")}.txt`), "");
  const tree = writeProject(join(root, "tree"), { files: treeFiles, depth: 4, bytesPerFile: 200 });
  return { root, flat: "flat", firstFlat: "flat/f000000.txt", treePaths: tree.paths.map((p) => `tree/${p}`) };
}
