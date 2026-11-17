// The structural equation editor: the draft's math tree drawn as nested boxes
// with visible empty slots and one caret. Keyboard: typing enters variables,
// numbers, and operators (`^`, `_`, `/` start templates in the core), arrows
// move, Tab / Shift+Tab move between slots (Tab leaves the editor from the
// last slot), Backspace/Delete delete, Enter applies unless the caret is in a
// matrix, cases, or aligned cell (Ctrl+Enter always applies), Escape cancels,
// Ctrl+Z / Ctrl+Y undo and redo inside the draft. A click places the caret.
import { forwardRef, useId, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";

import {
  catalogEntry,
  deleteBackward,
  deleteForward,
  move,
  setCaret,
  typeText,
  type CatalogEntry,
  type EditorState,
  type MathCaret,
  type MathNode,
  type MathRow,
  type Motion,
  type SlotPath,
} from "../core";
import { childPath, nodesOnPath, samePath, slotsOf, TEXT_SLOT } from "./tree";

export type EditOperation = (state: EditorState) => EditorState;

export interface MathEditorProps {
  state: EditorState;
  onEdit: (operation: EditOperation) => void;
  onApply: () => void;
  onCancel: () => void;
  onUndo: () => void;
  onRedo: () => void;
  /** Enter applies (false when Enter is ambiguous, e.g. in a matrix cell). */
  enterApplies: boolean;
  "aria-label": string;
}

/** A catalog entry, or undefined when the catalog cannot answer. */
export function entryOf(id: string): CatalogEntry | undefined {
  try {
    return catalogEntry(id);
  } catch {
    return undefined;
  }
}

const glyphOf = (id: string): string => entryOf(id)?.glyph ?? id;

const ARROWS: Record<string, Motion> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Home: "home",
  End: "end",
};

const sameCaret = (a: MathCaret, b: MathCaret) => a.offset === b.offset && a.anchor === b.anchor && samePath(a.path, b.path);

/** Where the caret is, in words (for assistive technology). */
export function describeCaret(state: EditorState): string {
  const nodes = nodesOnPath(state.root, state.caret.path);
  const parts: string[] = [];
  let row: MathRow = state.root;
  for (let i = 0; i < nodes.length; i++) {
    const node = nodes[i]!;
    if (node.kind === "text") {
      parts.push("text");
      return `In ${parts.reverse().join(", in ")}, position ${state.caret.offset + 1} of ${node.text.length + 1}`;
    }
    const slot = slotsOf(node).find((s) => s.name === state.caret.path[i * 2 + 1]);
    if (!slot) break;
    parts.push(`${slot.label} of ${node.kind === "scripts" ? "scripts" : node.kind}`);
    row = slot.row;
  }
  const where = parts.length ? `In ${parts.reverse().join(", in ")}` : "In the equation";
  return `${where}, position ${state.caret.offset + 1} of ${row.children.length + 1}`;
}

export const MathEditor = forwardRef<HTMLDivElement, MathEditorProps>(function MathEditor(
  { state, onEdit, onApply, onCancel, onUndo, onRedo, enterApplies, "aria-label": label },
  ref,
) {
  const descriptionId = useId();

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.nativeEvent.isComposing) return;
    const mod = event.ctrlKey || event.metaKey;
    const key = event.key;
    const handled = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    if (mod && !event.altKey && (key === "z" || key === "Z")) {
      handled();
      if (event.shiftKey) onRedo();
      else onUndo();
      return;
    }
    if (mod && !event.altKey && (key === "y" || key === "Y")) {
      handled();
      onRedo();
      return;
    }
    if (key === "Enter") {
      handled();
      if (mod || enterApplies) onApply();
      return;
    }
    if (key === "Escape") {
      handled();
      onCancel();
      return;
    }
    if (key === "Tab") {
      const next = move(state, event.shiftKey ? "prevSlot" : "nextSlot");
      // At the first/last slot, Tab leaves the editor (focus is never trapped).
      if (sameCaret(next.caret, state.caret)) return;
      handled();
      onEdit(() => next);
      return;
    }
    const motion = ARROWS[key];
    if (motion && !event.altKey && !mod) {
      handled();
      onEdit((s) => move(s, motion, event.shiftKey));
      return;
    }
    if (key === "Backspace" || key === "Delete") {
      handled();
      onEdit(key === "Backspace" ? deleteBackward : deleteForward);
      return;
    }
    // Printable characters (AltGr characters included; Ctrl/Cmd chords are not text).
    if ([...key].length === 1 && !(mod && !event.altKey)) {
      handled();
      onEdit((s) => typeText(s, key));
    }
  };

  const place = (path: SlotPath, offset: number) => onEdit((s) => setCaret(s, { path, offset }));

  return (
    <div
      ref={ref}
      role="textbox"
      aria-multiline="false"
      aria-roledescription="equation editor"
      aria-label={label}
      aria-describedby={descriptionId}
      tabIndex={0}
      className="xp-eq-editor"
      data-testid="equation-editor"
      onKeyDown={onKeyDown}
    >
      <RowView row={state.root} path={[]} caret={state.caret} onPlace={place} root />
      <span id={descriptionId} className="ui-visually-hidden" aria-live="polite" data-testid="equation-caret">
        {describeCaret(state)}
      </span>
    </div>
  );
});

interface RowViewProps {
  row: MathRow;
  path: SlotPath;
  caret: MathCaret;
  onPlace: (path: SlotPath, offset: number) => void;
  root?: boolean;
  /** Slot name, for test ids and styling ("num", "sup", "0,1"). */
  slot?: string;
}

function RowView({ row, path, caret, onPlace, root, slot }: RowViewProps) {
  const here = samePath(caret.path, path);
  const anchor = here ? caret.anchor : undefined;
  const selFrom = anchor === undefined ? -1 : Math.min(anchor, caret.offset);
  const selTo = anchor === undefined ? -1 : Math.max(anchor, caret.offset);

  const onMouseDown = (event: ReactMouseEvent<HTMLSpanElement>) => {
    if (event.button !== 0) return;
    // The innermost row under the pointer places the caret.
    event.stopPropagation();
    const container = event.currentTarget;
    const children = [...container.children].filter((c): c is HTMLElement => c instanceof HTMLElement && c.dataset.eqIndex !== undefined);
    const hit = children.find((c) => c.contains(event.target as Node));
    if (!hit) {
      // Empty slot, or the space after the last item.
      const box = container.getBoundingClientRect();
      onPlace(path, row.children.length === 0 || event.clientX > box.left + box.width / 2 ? row.children.length : 0);
      return;
    }
    const index = Number(hit.dataset.eqIndex);
    const box = hit.getBoundingClientRect();
    onPlace(path, event.clientX > box.left + box.width / 2 ? index + 1 : index);
  };

  const items: ReactNode[] = [];
  row.children.forEach((node, i) => {
    if (here && caret.offset === i) items.push(<span key={`caret-${i}`} className="xp-eq-caret" aria-hidden data-testid="equation-caret-mark" />);
    items.push(
      <span
        key={i}
        className="xp-eq-item"
        data-eq-index={i}
        data-kind={node.kind}
        data-selected={i >= selFrom && i < selTo ? true : undefined}
      >
        <NodeView node={node} path={path} index={i} caret={caret} onPlace={onPlace} />
      </span>,
    );
  });
  if (here && caret.offset === row.children.length) {
    items.push(<span key="caret-end" className="xp-eq-caret" aria-hidden data-testid="equation-caret-mark" />);
  }
  return (
    <span
      className="xp-eq-row"
      data-root={root || undefined}
      data-empty={row.children.length === 0 || undefined}
      data-slot={slot}
      data-active={here || undefined}
      onMouseDown={onMouseDown}
    >
      {items}
    </span>
  );
}

/** Upright text inside math; the caret can be inside it (path ends with [index, "text"]). */
function TextView({ text, path, caret, onPlace }: { text: string; path: SlotPath; caret: MathCaret; onPlace: (path: SlotPath, offset: number) => void }) {
  const here = samePath(caret.path, path);
  const onMouseDown = (event: ReactMouseEvent<HTMLSpanElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    const box = event.currentTarget.getBoundingClientRect();
    const ratio = box.width > 0 ? (event.clientX - box.left) / box.width : 1;
    onPlace(path, Math.max(0, Math.min(text.length, Math.round(ratio * text.length))));
  };
  if (!here) {
    return (
      <span className="xp-eq-text" data-empty={text === "" || undefined} onMouseDown={onMouseDown}>
        {text || " "}
      </span>
    );
  }
  const lo = Math.min(caret.anchor ?? caret.offset, caret.offset);
  const hi = Math.max(caret.anchor ?? caret.offset, caret.offset);
  const caretMark = <span className="xp-eq-caret" aria-hidden data-testid="equation-caret-mark" />;
  return (
    <span className="xp-eq-text" data-active onMouseDown={onMouseDown}>
      {text.slice(0, lo)}
      {caret.offset === lo && caretMark}
      {hi > lo && <span className="xp-eq-item" data-selected>{text.slice(lo, hi)}</span>}
      {caret.offset === hi && hi > lo && caretMark}
      {text.slice(hi)}
      {text === "" && " "}
    </span>
  );
}

interface NodeViewProps {
  node: MathNode;
  path: SlotPath;
  index: number;
  caret: MathCaret;
  onPlace: (path: SlotPath, offset: number) => void;
}

function NodeView({ node, path, index, caret, onPlace }: NodeViewProps) {
  const slot = (name: string, row: MathRow | null) =>
    row ? <RowView row={row} path={childPath(path, index, name)} caret={caret} onPlace={onPlace} slot={name} /> : null;
  switch (node.kind) {
    case "ident":
      return <span className="xp-eq-ident">{node.text}</span>;
    case "number":
      return <span className="xp-eq-number">{node.text}</span>;
    case "symbol":
      return (
        <span className="xp-eq-symbol" title={entryOf(node.id)?.name}>
          {glyphOf(node.id)}
        </span>
      );
    case "text":
      return <TextView text={node.text} path={childPath(path, index, TEXT_SLOT)} caret={caret} onPlace={onPlace} />;
    case "func":
      return <span className="xp-eq-func">{node.text || (entryOf(node.name)?.glyph ?? node.name)}</span>;
    case "frac":
      return (
        <span className="xp-eq-frac">
          <span className="xp-eq-frac__num">{slot("num", node.num)}</span>
          <span className="xp-eq-frac__den">{slot("den", node.den)}</span>
        </span>
      );
    case "sqrt":
      return (
        <span className="xp-eq-sqrt">
          {node.index && <span className="xp-eq-sqrt__index">{slot("index", node.index)}</span>}
          <span className="xp-eq-sqrt__sign" aria-hidden>
            √
          </span>
          <span className="xp-eq-sqrt__body">{slot("body", node.body)}</span>
        </span>
      );
    case "scripts":
      return (
        <span className="xp-eq-scripts">
          <span className="xp-eq-scripts__base">{slot("base", node.base)}</span>
          <span className="xp-eq-scripts__stack">
            <span className="xp-eq-scripts__sup">{slot("sup", node.sup)}</span>
            <span className="xp-eq-scripts__sub">{slot("sub", node.sub)}</span>
          </span>
        </span>
      );
    case "bigop":
      return (
        <span className="xp-eq-bigop">
          <span className="xp-eq-bigop__upper">{slot("upper", node.upper)}</span>
          <span className="xp-eq-bigop__op" title={entryOf(node.op)?.name}>
            {glyphOf(node.op)}
          </span>
          <span className="xp-eq-bigop__lower">{slot("lower", node.lower)}</span>
        </span>
      );
    case "delim":
      return (
        <span className="xp-eq-delim">
          <span className="xp-eq-delim__fence">{node.left === "delim.none" ? "" : glyphOf(node.left)}</span>
          {slot("body", node.body)}
          <span className="xp-eq-delim__fence">{node.right === "delim.none" ? "" : glyphOf(node.right)}</span>
        </span>
      );
    case "accent":
      return (
        <span className="xp-eq-accent" title={entryOf(node.accent)?.name}>
          <span className="xp-eq-accent__mark" aria-hidden>
            {glyphOf(node.accent)}
          </span>
          {slot("body", node.body)}
        </span>
      );
    case "font":
      return (
        <span className="xp-eq-font" title={entryOf(node.font)?.name}>
          {slot("body", node.body)}
          <span className="xp-eq-font__tag" aria-hidden>
            {glyphOf(node.font)}
          </span>
        </span>
      );
    case "matrix":
    case "aligned":
    case "cases": {
      const slots = slotsOf(node);
      const cols = node.kind === "cases" ? 2 : Math.max(1, ...node.rows.map((r) => r.length));
      return (
        <span className="xp-eq-grid-wrap" data-grid={node.kind} data-fence={node.kind === "matrix" ? node.fence : undefined}>
          {node.kind === "cases" && (
            <span className="xp-eq-delim__fence" aria-hidden>
              {"{"}
            </span>
          )}
          <span className="xp-eq-grid" style={{ gridTemplateColumns: `repeat(${cols}, auto)` }}>
            {slots.map((s) => (
              <span key={s.name} className="xp-eq-grid__cell">
                {slot(s.name, s.row)}
              </span>
            ))}
          </span>
        </span>
      );
    }
    case "opaque":
      return (
        <code className="ui-code xp-eq-opaque" title={`Kept exactly as written: ${node.reason}`}>
          {node.source}
        </code>
      );
  }
}
