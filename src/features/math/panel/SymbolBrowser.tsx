// Symbol browser: categories, search (names, synonyms, glyphs, commands), and
// a keyboard-navigable grid of glyphs. Inserting never moves the equation
// caret elsewhere: the symbol goes in at the draft's caret (or at the
// document caret when no draft is open), and a mouse press does not take
// focus from the equation editor. Results are capped and render plain glyphs
// (no typeset previews), so typing in the search stays fast.
import { useDeferredValue, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { EmptyState, SearchField, Select } from "../../../ui/components";
import { catalog, searchCatalog, type CatalogEntry, type MathLanguage, type SearchHit } from "../core";
import { availableIn, commandFor, LANGUAGE_LABELS } from "./tree";

/** Most results shown at once (search narrows further). */
export const SYMBOL_LIMIT = 240;
const ALL = "all";

export interface SymbolBrowserProps {
  /** Language of the target document (availability is shown for it), or null without one. */
  language: MathLanguage | null;
  /** Insert an available entry. `keyboard`: chosen with Enter (focus stays in the browser). */
  onInsert: (entry: CatalogEntry, keyboard: boolean) => void;
}

const titleCase = (s: string) => (s ? s[0]!.toUpperCase() + s.slice(1) : s);

function safe<T>(fn: () => T, fallback: T): T {
  try {
    return fn();
  } catch {
    return fallback;
  }
}

export function SymbolBrowser({ language, onInsert }: SymbolBrowserProps) {
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<string>(ALL);
  const [active, setActive] = useState(0);
  const deferred = useDeferredValue(query.trim());
  const listRef = useRef<HTMLDivElement>(null);
  const baseId = useId();

  const entries = useMemo(() => safe(() => catalog(), [] as readonly CatalogEntry[]), []);
  const categories = useMemo(() => [...new Set(entries.map((e) => e.category))], [entries]);
  const hits = useMemo<SearchHit[]>(() => {
    const scope = category === ALL ? undefined : category;
    if (deferred) {
      return safe(() => searchCatalog(deferred, { language: language ?? undefined, limit: SYMBOL_LIMIT + 1, category: scope }), []);
    }
    return entries
      .filter((e) => !scope || e.category === scope)
      .slice(0, SYMBOL_LIMIT + 1)
      .map((entry) => ({ entry, matched: "", available: language ? availableIn(entry, language) : true }));
  }, [deferred, category, language, entries]);
  const shown = hits.slice(0, SYMBOL_LIMIT);
  const current = Math.min(active, Math.max(0, shown.length - 1));
  const hit = shown[current];

  const insert = (index: number, keyboard: boolean) => {
    const chosen = shown[index];
    if (!chosen || !chosen.available) return;
    onInsert(chosen.entry, keyboard);
  };

  const focusOption = (index: number) => {
    setActive(index);
    listRef.current?.querySelector(`[data-index="${index}"]`)?.scrollIntoView?.({ block: "nearest" });
  };

  /** Options per visual row (for Up/Down). */
  const columns = () => {
    const options = [...(listRef.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];
    const top = options[0]?.offsetTop;
    const count = options.filter((o) => o.offsetTop === top).length;
    return Math.max(1, count);
  };

  const onListKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const last = shown.length - 1;
    let next: number | null = null;
    switch (event.key) {
      case "ArrowRight":
        next = Math.min(last, current + 1);
        break;
      case "ArrowLeft":
        next = Math.max(0, current - 1);
        break;
      case "ArrowDown":
        next = Math.min(last, current + columns());
        break;
      case "ArrowUp":
        next = Math.max(0, current - columns());
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = last;
        break;
      case "Enter":
      case " ":
        event.preventDefault();
        insert(current, true);
        return;
      default:
        return;
    }
    event.preventDefault();
    if (last >= 0) focusOption(next);
  };

  const detail = hit ? describe(hit, language) : null;
  const optionId = (i: number) => `${baseId}-symbol-${i}`;

  return (
    <div className="xp-eq-symbols" data-testid="equation-symbols">
      <div className="xp-eq-symbols__filters">
        <Select
          aria-label="Symbol category"
          className="xp-eq-symbols__category"
          options={[{ id: ALL, label: "All categories" }, ...categories.map((c) => ({ id: c, label: titleCase(c) }))]}
          value={category}
          onChange={(value) => {
            setCategory(value);
            setActive(0);
          }}
        />
        <SearchField
          aria-label="Search symbols"
          placeholder="Name, command, or symbol"
          value={query}
          onChange={(value) => {
            setQuery(value);
            setActive(0);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" && shown.length > 0) {
              event.preventDefault();
              listRef.current?.focus();
            }
          }}
          onSubmit={() => insert(current, true)}
        />
      </div>
      {shown.length === 0 ? (
        <EmptyState
          title={deferred ? `No symbols match “${deferred}”` : "No symbols in this category"}
          description="Search by name (less than or equal), command (leq), or the symbol itself (≤)."
        />
      ) : (
        <>
          <div
            ref={listRef}
            role="listbox"
            aria-label={deferred ? `Symbols matching ${deferred}` : "Symbols"}
            aria-orientation="horizontal"
            aria-activedescendant={hit ? optionId(current) : undefined}
            tabIndex={0}
            className="xp-eq-symbols__grid"
            data-testid="equation-symbol-grid"
            onKeyDown={onListKeyDown}
          >
            {shown.map((h, i) => (
              <div
                key={h.entry.id}
                id={optionId(i)}
                role="option"
                aria-selected={i === current}
                aria-disabled={!h.available || undefined}
                aria-label={h.available || !language ? h.entry.name : `${h.entry.name}, not available in ${LANGUAGE_LABELS[language]}`}
                className="xp-eq-symbols__option"
                data-index={i}
                data-symbol={h.entry.id}
                data-active={i === current || undefined}
                data-unavailable={!h.available || undefined}
                // Pressing does not take focus from the equation editor.
                onMouseDown={(event) => event.preventDefault()}
                onMouseEnter={() => setActive(i)}
                onClick={() => {
                  setActive(i);
                  insert(i, false);
                }}
              >
                <span aria-hidden>{h.entry.glyph}</span>
              </div>
            ))}
          </div>
          <p className="ui-meta xp-eq-symbols__detail" data-testid="equation-symbol-detail">
            {detail}
          </p>
          {hits.length > SYMBOL_LIMIT && (
            <p className="ui-meta xp-eq-symbols__more">Showing the first {SYMBOL_LIMIT}. Search to narrow the list.</p>
          )}
        </>
      )}
    </div>
  );
}

function describe(hit: SearchHit, language: MathLanguage | null): string {
  const parts = [hit.entry.name];
  const command = language ? commandFor(hit.entry, language) : hit.entry.latex?.command;
  if (command) parts.push(command);
  if (hit.matched) parts.push(`matched ${hit.matched}`);
  if (language && !hit.available) parts.push(`not available in ${LANGUAGE_LABELS[language]}`);
  else if (language === "latex" && hit.entry.latex?.package) parts.push(`needs ${hit.entry.latex.package}`);
  return parts.join(" · ");
}
