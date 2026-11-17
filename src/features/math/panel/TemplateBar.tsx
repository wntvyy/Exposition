// Structure templates (fraction, root, scripts, big operators, delimiters,
// matrix, cases, aligned, text) and the row/column actions of the grid the
// caret is in. Templates insert a structure with empty slots at the draft's
// caret; a selection becomes the first slot where that is valid (core).
import { Button, IconButton, Toolbar, type IconComponent } from "../../../ui/components";
import type { GridEdit, TemplateId } from "../core";

interface TemplateSpec {
  id: TemplateId;
  label: string;
  glyph: string;
  options?: { rows?: number; cols?: number };
}

export const TEMPLATES: TemplateSpec[] = [
  { id: "frac", label: "Fraction", glyph: "a⁄b" },
  { id: "sqrt", label: "Square root", glyph: "√x" },
  { id: "root", label: "Root with index", glyph: "ⁿ√x" },
  { id: "sup", label: "Superscript", glyph: "xⁿ" },
  { id: "sub", label: "Subscript", glyph: "xₙ" },
  { id: "subsup", label: "Subscript and superscript", glyph: "xₙⁿ" },
  { id: "integral", label: "Integral with bounds", glyph: "∫" },
  { id: "sum", label: "Sum with bounds", glyph: "∑" },
  { id: "product", label: "Product with bounds", glyph: "∏" },
  { id: "limit", label: "Limit", glyph: "lim" },
  { id: "parens", label: "Parentheses", glyph: "( )" },
  { id: "brackets", label: "Brackets", glyph: "[ ]" },
  { id: "braces", label: "Braces", glyph: "{ }" },
  { id: "abs", label: "Absolute value", glyph: "|x|" },
  { id: "norm", label: "Norm", glyph: "‖x‖" },
  { id: "matrix", label: "Matrix (2 × 2)", glyph: "[⋯]", options: { rows: 2, cols: 2 } },
  { id: "cases", label: "Cases", glyph: "{⋮" },
  { id: "aligned", label: "Aligned lines", glyph: "=⋮" },
  { id: "text", label: "Text", glyph: "Aa" },
];

/** A text glyph shown where an IconButton expects an icon (the button keeps its accessible name and tooltip). */
const glyphIcons = new Map<string, IconComponent>();
function glyphIcon(glyph: string): IconComponent {
  let icon = glyphIcons.get(glyph);
  if (!icon) {
    icon = function GlyphIcon({ className }: { className?: string }) {
      return (
        <span aria-hidden className={`xp-eq-glyph-icon${className ? ` ${className}` : ""}`}>
          {glyph}
        </span>
      );
    };
    glyphIcons.set(glyph, icon);
  }
  return icon;
}

export interface TemplateBarProps {
  onTemplate: (template: TemplateId, options?: { rows?: number; cols?: number }) => void;
  /** Row/column actions, shown while the caret is in a matrix, cases, or aligned cell. */
  grid: "matrix" | "cases" | "aligned" | null;
  onGrid: (edit: GridEdit) => void;
  disabled?: boolean;
}

export function TemplateBar({ onTemplate, grid, onGrid, disabled }: TemplateBarProps) {
  return (
    <>
      <Toolbar aria-label="Equation templates" className="xp-eq-templates" data-testid="equation-templates">
        {TEMPLATES.map((t) => (
          <IconButton
            key={t.id}
            aria-label={t.label}
            icon={glyphIcon(t.glyph)}
            size="sm"
            isDisabled={disabled}
            data-template={t.id}
            onPress={() => onTemplate(t.id, t.options)}
          />
        ))}
      </Toolbar>
      {grid && (
        <Toolbar aria-label={grid === "cases" ? "Case rows" : "Rows and columns"} className="xp-eq-grid-actions" data-testid="equation-grid-actions">
          <Button size="sm" variant="ghost" onPress={() => onGrid("addRowBefore")}>
            Row above
          </Button>
          <Button size="sm" variant="ghost" onPress={() => onGrid("addRowAfter")}>
            Row below
          </Button>
          {grid !== "cases" && (
            <>
              <Button size="sm" variant="ghost" onPress={() => onGrid("addColBefore")}>
                Column left
              </Button>
              <Button size="sm" variant="ghost" onPress={() => onGrid("addColAfter")}>
                Column right
              </Button>
            </>
          )}
          <Button size="sm" variant="ghost" onPress={() => onGrid("removeRow")}>
            Remove row
          </Button>
          {grid !== "cases" && (
            <Button size="sm" variant="ghost" onPress={() => onGrid("removeCol")}>
              Remove column
            </Button>
          )}
        </Toolbar>
      )}
    </>
  );
}
