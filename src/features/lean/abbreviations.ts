// Lean Unicode input: `\alpha` → α, `\to` → →, `\forall` → ∀, … (a subset of
// the abbreviations of the official Lean editor extensions). An abbreviation
// is replaced when it is followed by a character that ends it (space, newline,
// tab, or a bracket/comma), and can be picked from completion after `\`.

export const ABBREVIATIONS: Readonly<Record<string, string>> = {
  // Greek
  a: "α", alpha: "α", b: "β", beta: "β", g: "γ", gamma: "γ", d: "δ", delta: "δ", e: "ε", epsilon: "ε",
  z: "ζ", zeta: "ζ", eta: "η", th: "θ", theta: "θ", i: "ι", iota: "ι", k: "κ", kappa: "κ",
  l: "λ", lambda: "λ", fun: "λ", m: "μ", mu: "μ", n: "ν", nu: "ν", xi: "ξ", pi: "π", r: "ρ", rho: "ρ",
  s: "σ", sigma: "σ", t: "τ", tau: "τ", phi: "φ", f: "φ", chi: "χ", psi: "ψ", w: "ω", omega: "ω",
  G: "Γ", Gamma: "Γ", D: "Δ", Delta: "Δ", Th: "Θ", Theta: "Θ", L: "Λ", Lambda: "Λ", Xi: "Ξ", Pi: "Π",
  S: "Σ", Sigma: "Σ", Phi: "Φ", Psi: "Ψ", W: "Ω", Omega: "Ω",
  // Arrows
  to: "→", "->": "→", imp: "→", "<-": "←", gets: "←", "<->": "↔", iff: "↔", lr: "↔",
  "=>": "⇒", "<=>": "⇔", mapsto: "↦", "|->": "↦", "-->": "⟶", u: "↑", up: "↑", "u-": "↓", down: "↓",
  // Logic
  and: "∧", or: "∨", not: "¬", neg: "¬", forall: "∀", all: "∀", fa: "∀",
  exists: "∃", ex: "∃", top: "⊤", bot: "⊥", "|-": "⊢", vdash: "⊢", "|=": "⊨", qed: "∎",
  // Relations
  le: "≤", "<=": "≤", ge: "≥", ">=": "≥", ne: "≠", neq: "≠", equiv: "≡", "==": "≡",
  approx: "≈", "~~": "≈", sim: "∼", "~": "∼", "~-": "≃", cong: "≅", "=~": "≅", "dvd": "∣", "|": "∣",
  "<": "⟨", ">": "⟩", langle: "⟨", rangle: "⟩", "<<": "⟪", ">>": "⟫",
  // Sets
  in: "∈", mem: "∈", notin: "∉", nin: "∉", sub: "⊂", subset: "⊂", sube: "⊆", subseteq: "⊆", ss: "⊆",
  sup: "⊃", supset: "⊃", supe: "⊇", supseteq: "⊇", cap: "∩", inter: "∩", cup: "∪", union: "∪",
  empty: "∅", emptyset: "∅", setminus: "\\", compl: "ᶜ", powerset: "𝒫",
  bigcap: "⋂", bigcup: "⋃", Inter: "⋂", Union: "⋃",
  // Operators
  x: "×", times: "×", cdot: "·", ".": "·", o: "∘", circ: "∘", comp: "∘", "+": "⊕", oplus: "⊕",
  otimes: "⊗", sum: "∑", prod: "∏", sqrt: "√", inf: "∞", infty: "∞", "-1": "⁻¹", inv: "⁻¹",
  "^-1": "⁻¹", nabla: "∇", partial: "∂", int: "∫", "||": "‖", norm: "‖",
  // Number sets
  N: "ℕ", nat: "ℕ", Z: "ℤ", Q: "ℚ", R: "ℝ", real: "ℝ", C: "ℂ", complex: "ℂ",
  // Subscripts and superscripts
  "0": "₀", "1": "₁", "2": "₂", "3": "₃", "4": "₄", "5": "₅", "6": "₆", "7": "₇", "8": "₈", "9": "₉",
  _0: "₀", _1: "₁", _2: "₂", _3: "₃", _4: "₄", _5: "₅", _6: "₆", _7: "₇", _8: "₈", _9: "₉",
  "^0": "⁰", "^1": "¹", "^2": "²", "^3": "³", "^n": "ⁿ", _i: "ᵢ", _n: "ₙ",
  // Misc
  "::": "∷", ":=": "≔", "!": "¬", ell: "ℓ", "#": "♯", flat: "♭", "^": "↑",
};

/** Characters that end an abbreviation (they are kept after the replacement). */
export function isTerminator(text: string): boolean {
  return /^(?:[ \t(),[\]{}]|\r?\n[ \t]*)$/.test(text);
}

export interface Expansion {
  /** Index in `before` where the `\` starts. */
  start: number;
  /** Replacement for `before.slice(start)`. */
  replacement: string;
}

/**
 * If `before` (the line text up to the cursor, terminator excluded) ends with
 * a known `\abbreviation`, return where it starts and what replaces it.
 */
export function expandAbbreviation(before: string): Expansion | null {
  const match = /\\([^\s\\()[\]{},]+)$/.exec(before);
  if (!match) return null;
  // `\\` at the very start of the match (an escaped backslash) is not an abbreviation.
  if (match.index > 0 && before[match.index - 1] === "\\") return null;
  const name = match[1];
  if (name === undefined) return null;
  const replacement = ABBREVIATIONS[name];
  return replacement === undefined ? null : { start: match.index, replacement };
}

/** Abbreviations starting with `prefix` (for completion), shortest first. */
export function abbreviationsWithPrefix(prefix: string): [string, string][] {
  return Object.entries(ABBREVIATIONS)
    .filter(([name]) => name.startsWith(prefix))
    .sort((a, b) => a[0].length - b[0].length || a[0].localeCompare(b[0]));
}
