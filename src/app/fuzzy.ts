// Fuzzy subsequence matching for the command palette and quick pickers.
// Scores favor matches at word starts, contiguous runs, and early positions.

export interface FuzzyMatch {
  score: number;
  /** Indices in the candidate text that matched query characters. */
  positions: number[];
}

function isWordStart(text: string, index: number): boolean {
  if (index === 0) return true;
  const prev = text[index - 1]!;
  const cur = text[index]!;
  if (/[\s:/\\._\-()[\]]/.test(prev)) return true;
  return prev === prev.toLowerCase() && cur !== cur.toLowerCase(); // camelCase boundary
}

/**
 * Best-effort greedy match with a forward pass that prefers word starts.
 * Returns null when the query is not a subsequence of the text.
 */
export function fuzzyMatch(query: string, text: string): FuzzyMatch | null {
  const q = query.replace(/\s+/g, "").toLowerCase();
  if (!q) return { score: 0, positions: [] };
  const lower = text.toLowerCase();
  const positions: number[] = [];
  let from = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const ch = q[qi]!;
    // Prefer continuing a run, then the next word start containing ch, then any occurrence.
    let index = -1;
    if (positions.length && lower[from] === ch) index = from;
    if (index < 0) {
      for (let i = from; i < lower.length; i++) {
        if (lower[i] === ch && isWordStart(text, i)) {
          index = i;
          break;
        }
      }
    }
    if (index < 0) index = lower.indexOf(ch, from);
    if (index < 0) return null;
    positions.push(index);
    from = index + 1;
  }
  let score = 0;
  for (let i = 0; i < positions.length; i++) {
    const pos = positions[i]!;
    score += 1;
    if (isWordStart(text, pos)) score += 8;
    if (i > 0 && pos === positions[i - 1]! + 1) score += 5;
    if (i > 0) score -= Math.min(3, pos - positions[i - 1]! - 1) * 0.5;
  }
  score -= positions[0]! * 0.05;
  score -= text.length * 0.01;
  return { score, positions };
}

export interface Ranked<T> extends FuzzyMatch {
  item: T;
}

/** Filter and sort items by fuzzy score; stable for ties and for an empty query. */
export function rankByFuzzy<T>(query: string, items: readonly T[], text: (item: T) => string): Ranked<T>[] {
  const ranked: (Ranked<T> & { index: number })[] = [];
  items.forEach((item, index) => {
    const match = fuzzyMatch(query, text(item));
    if (match) ranked.push({ item, index, ...match });
  });
  ranked.sort((a, b) => b.score - a.score || a.index - b.index);
  return ranked.map(({ item, score, positions }) => ({ item, score, positions }));
}
