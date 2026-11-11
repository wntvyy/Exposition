// Color utilities for token verification (WCAG 2.x contrast) and for reading
// the token stylesheet. Kept dependency-free so tests run against the real CSS.

export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

export function parseColor(value: string): Rgba {
  const v = value.trim().toLowerCase();
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/.exec(v);
  if (hex) {
    let h = hex[1]!;
    if (h.length === 3) h = [...h].map((c) => c + c).join("");
    const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
    return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
  }
  const rgb = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/.exec(v);
  if (rgb) {
    return { r: Number(rgb[1]), g: Number(rgb[2]), b: Number(rgb[3]), a: rgb[4] === undefined ? 1 : Number(rgb[4]) };
  }
  throw new Error(`unsupported color ${value}`);
}

/** Composite a (possibly translucent) foreground over an opaque background. */
export function composite(fg: Rgba, bg: Rgba): Rgba {
  const a = fg.a;
  return { r: fg.r * a + bg.r * (1 - a), g: fg.g * a + bg.g * (1 - a), b: fg.b * a + bg.b * (1 - a), a: 1 };
}

export function relativeLuminance({ r, g, b }: Rgba): number {
  const channel = (c: number) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** WCAG contrast ratio between two colors; translucent foregrounds are composited over `bg`. */
export function contrastRatio(fg: string, bg: string): number {
  const back = parseColor(bg);
  if (back.a < 1) throw new Error(`background ${bg} must be opaque`);
  const front = composite(parseColor(fg), back);
  const [hi, lo] = [relativeLuminance(front), relativeLuminance(back)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * Extract custom properties from the declaration block opened by `selector`
 * (first occurrence) in a stylesheet. Comments are ignored.
 */
export function readCustomProperties(css: string, selector: string): Record<string, string> {
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const start = clean.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`selector ${selector} not found`);
  const open = clean.indexOf("{", start);
  const close = clean.indexOf("}", open);
  const body = clean.slice(open + 1, close);
  const out: Record<string, string> = {};
  for (const decl of body.split(";")) {
    const m = /^\s*(--[\w-]+)\s*:\s*([\s\S]+?)\s*$/.exec(decl);
    if (m) out[m[1]!] = m[2]!;
  }
  return out;
}

/** Resolve `var(--x)` references (no fallbacks) against a property map. */
export function resolveVar(value: string, props: Record<string, string>, depth = 0): string {
  if (depth > 10) throw new Error(`var() cycle at ${value}`);
  const m = /^var\((--[\w-]+)\)$/.exec(value.trim());
  if (!m) return value;
  const next = props[m[1]!];
  if (next === undefined) throw new Error(`undefined custom property ${m[1]}`);
  return resolveVar(next, props, depth + 1);
}
