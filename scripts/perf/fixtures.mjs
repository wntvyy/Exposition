// Deterministic synthetic projects for scale measurements. The same options
// always produce the same bytes, so a measurement can be repeated on another
// revision or machine. Content is real-shaped (headings, labels, references,
// equations), not random bytes: extraction, outline, and graph reconciliation
// have work to do.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

/** Small deterministic generator (mulberry32). */
export function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = "let the map be continuous and bounded on a compact set then its image is compact hence the supremum is attained by some point of the domain which proves the claim for every sequence there is a convergent subsequence".split(" ");

function sentence(next, words) {
  const out = [];
  for (let i = 0; i < words; i++) out.push(WORDS[Math.floor(next() * WORDS.length)]);
  return `${out.join(" ")}.`;
}

/** Scale presets: files, folder depth, bytes per file. */
export const SCALES = {
  small: { files: 200, depth: 3, bytesPerFile: 2_000 },
  large: { files: 10_000, depth: 6, bytesPerFile: 2_000 },
  stress: { files: 100_000, depth: 8, bytesPerFile: 1_000 },
};

/**
 * A Typst document of about `bytes` bytes with labelled sections and
 * equations. Each section refers to up to `refs` earlier labels, taken from
 * `labels` (labels of other files) and from the file's own earlier sections.
 */
export function typstDocument({ name, bytes, seed = 1, refs = 2, labels = [] }) {
  const next = rng(seed);
  const known = [...labels];
  const parts = [`= ${name}\n`];
  let size = parts[0].length;
  for (let i = 0; size < bytes; i++) {
    const label = `${name.replace(/[^a-z0-9]/gi, "-")}-s${i}`;
    let block = `\n== Section ${i} <${label}>\n${sentence(next, 24)}\n`;
    for (let r = 0; r < refs && known.length; r++) block += `See @${known[Math.floor(next() * known.length)]}. `;
    block += `\n$ sum_(k=0)^n k = (n (n + 1)) / 2 $ <${label}-eq>\n${sentence(next, 40)}\n`;
    known.push(label);
    parts.push(block);
    size += block.length;
  }
  return parts.join("");
}

/** A text of exactly `bytes` ASCII bytes in lines of `lineLength` characters. */
export function plainText({ bytes, lineLength = 80, seed = 1 }) {
  const next = rng(seed);
  const lines = [];
  let size = 0;
  while (size < bytes) {
    let line = "";
    while (line.length < lineLength) line += `${WORDS[Math.floor(next() * WORDS.length)]} `;
    line = `${line.slice(0, lineLength)}\n`;
    lines.push(line);
    size += line.length;
  }
  return lines.join("").slice(0, bytes);
}

/**
 * Writes a project tree of `files` Typst files spread over nested folders
 * (`fanout` subfolders per level, up to `depth` levels) and returns
 * `{ root, paths }`. One file in `linkEvery` refers to labels of earlier files.
 */
export function writeProject(root, { files, depth = 3, bytesPerFile = 2_000, fanout = 8, seed = 1, linkEvery = 4, extension = "typ" } = {}) {
  const next = rng(seed);
  const paths = [];
  const labels = [];
  for (let i = 0; i < files; i++) {
    const segments = [];
    const levels = 1 + Math.floor(next() * depth);
    for (let d = 0; d < levels; d++) segments.push(`d${Math.floor(next() * fanout)}`);
    const name = `f${String(i).padStart(6, "0")}`;
    const rel = [...segments, `${name}.${extension}`].join("/");
    const path = join(root, rel);
    mkdirSync(dirname(path), { recursive: true });
    const text = typstDocument({ name, bytes: bytesPerFile, seed: seed * 7919 + i, labels: i % linkEvery === 0 ? labels.slice(-50) : [] });
    writeFileSync(path, text);
    labels.push(`${name}-s0`);
    paths.push(rel);
  }
  return { root, paths };
}
