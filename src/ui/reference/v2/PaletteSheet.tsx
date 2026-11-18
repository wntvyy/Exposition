// V2 review samples: one palette sheet — controls in every state, text roles,
// semantic states, syntax, graph nodes, file icons at actual size, the main
// token values, and the computed contrast table.
import { AlertTriangle, Check, FileText, Hammer, Save, Trash2 } from "lucide-react";
import type { CSSProperties, ReactNode } from "react";

import type { ColorRole } from "../../tokens";
import { tokenizeLine } from "./highlight";
import { Badge, Banner, Btn, FileGlyph, StatusPill, StatusText, type ForcedState, type Tone } from "./kit";
import type { Lang } from "./mock";
import { evaluateContrast, type V2Palette } from "./palettes";

const STATES: ForcedState[] = ["rest", "hover", "pressed", "selected", "focused", "disabled"];
const VARIANTS = [
  ["primary", "Primary"],
  ["secondary", "Secondary"],
  ["ghost", "Quiet"],
  ["danger", "Destructive"],
] as const;

export function PaletteSheet({ palette }: { palette: V2Palette }) {
  const results = evaluateContrast(palette);
  const failed = results.filter((r) => !r.pass);
  const theme = palette.kind;
  return (
    <div className="v2-sheet" data-testid={`v2-sheet-${palette.id}`}>
      <header className="v2-sheet__head">
        <h1 className="v2-page-title">{palette.name}</h1>
        <p className="v2-page-lead">{palette.summary}</p>
        <p className="v2-meta">
          Same role names as tokens.css · contrast {results.length - failed.length} of {results.length} pairs pass
          {failed.length ? ` · ${failed.length} below target` : ""}
        </p>
      </header>

      <div className="v2-sheet__grid">
        <SheetSection title="Controls" wide>
          <table className="v2-state-table">
            <thead>
              <tr>
                <th scope="col" />
                {STATES.map((s) => (
                  <th key={s} scope="col">
                    {cap(s)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {VARIANTS.map(([variant, label]) => (
                <tr key={variant}>
                  <th scope="row">{label}</th>
                  {STATES.map((s) => (
                    <td key={s}>
                      <Btn variant={variant} size="sm" state={s} icon={variant === "danger" ? Trash2 : variant === "primary" ? Check : variant === "ghost" ? Hammer : Save}>
                        {variant === "danger" ? "Delete" : variant === "primary" ? "Approve" : variant === "ghost" ? "Build" : "Save"}
                      </Btn>
                    </td>
                  ))}
                </tr>
              ))}
              <tr>
                <th scope="row">Field</th>
                {STATES.map((s) => (
                  <td key={s}>
                    <input
                      className="v2-input"
                      data-state={s === "selected" ? "error" : s}
                      disabled={s === "disabled"}
                      readOnly
                      value={s === "selected" ? "proj_" : "org-42"}
                      aria-label={`Field ${s}`}
                      aria-invalid={s === "selected"}
                    />
                    {s === "selected" && <span className="v2-state-note">error</span>}
                  </td>
                ))}
              </tr>
              <tr>
                <th scope="row">List row</th>
                {STATES.map((s) => (
                  <td key={s}>
                    <div className="v2-tree__row v2-tree__row--sample" data-state={s === "pressed" ? "selected-inactive" : s} aria-disabled={s === "disabled" || undefined}>
                      <FileGlyph path="results.typ" theme={theme} />
                      <span className="v2-tree__name">results.typ</span>
                      <StatusText tone="danger">1</StatusText>
                    </div>
                    {s === "pressed" && <span className="v2-state-note">selected, unfocused</span>}
                  </td>
                ))}
              </tr>
              <tr>
                <th scope="row">Tab</th>
                {STATES.map((s) => (
                  <td key={s}>
                    <div className="v2-tab v2-tab--sample" data-state={s} aria-selected={s === "selected" || s === "focused"}>
                      <FileText size={14} aria-hidden />
                      <span className="v2-tab__title">methods.md</span>
                    </div>
                  </td>
                ))}
              </tr>
              <tr>
                <th scope="row">Choice</th>
                {STATES.map((s) => (
                  <td key={s}>
                    <span className="v2-check" data-state={s}>
                      <span className="v2-check__box" data-checked={s === "selected" || s === "focused" ? "true" : undefined}>
                        {(s === "selected" || s === "focused") && <Check size={12} aria-hidden />}
                      </span>
                      Setup
                    </span>
                  </td>
                ))}
              </tr>
            </tbody>
          </table>
        </SheetSection>

        <SheetSection title="Text roles on each surface">
          <div className="v2-surfaces">
            {(["surface-base", "surface-sunken", "surface", "surface-raised"] as ColorRole[]).map((bg) => (
              <div key={bg} className="v2-surface-sample" style={{ background: `var(--color-${bg})` }}>
                <code className="v2-meta">{bg}</code>
                <span style={{ color: "var(--color-text)" }}>Text</span>
                <span style={{ color: "var(--color-text-muted)" }}>Muted text</span>
                <span style={{ color: "var(--color-text-subtle)" }}>Subtle meta</span>
                <span style={{ color: "var(--color-text-disabled)" }}>Disabled</span>
                <span className="v2-link">Accent link</span>
              </div>
            ))}
          </div>
        </SheetSection>

        <SheetSection title="Semantic states (distinct from the accent)">
          <div className="v2-stack">
            <div className="v2-row v2-row--wrap">
              <span className="v2-accent-chip">Accent</span>
              <StatusPill tone="success">Built</StatusPill>
              <StatusPill tone="warning">Waiting for approval</StatusPill>
              <StatusPill tone="danger">Failed</StatusPill>
              <StatusPill tone="info">Running</StatusPill>
              <StatusPill tone="stale">Stale</StatusPill>
            </div>
            {(
              [
                ["success", "Lean check passed", "No errors or sorries."],
                ["warning", "Uses sorry", "rate_mono is not proved yet."],
                ["danger", "Build failed", "unknown variable: Ea_kj (results.typ 16:27)."],
                ["info", "Needs your answer", "The agent waits for your answer."],
              ] as [Tone, string, string][]
            ).map(([tone, title, text]) => (
              <Banner key={tone} tone={tone} title={title}>
                {text}
              </Banner>
            ))}
            <div className="v2-row v2-row--wrap">
              <span className="v2-selection-sample">
                Selected <mark>text in the editor</mark>
              </span>
              <span className="v2-focus-sample" tabIndex={-1}>
                Focus ring
              </span>
              <span className="v2-selected-row-sample">
                <StatusText tone="danger">Failed</StatusText>
                <StatusText tone="success">Current</StatusText>
              </span>
            </div>
          </div>
        </SheetSection>

        <SheetSection title="Syntax">
          <CodeSample lang="python" code={PY} />
          <CodeSample lang="typst" code={TYP} />
          <p className="v2-meta">Keywords use the info role (proposal), so syntax keeps its meaning when the accent changes.</p>
        </SheetSection>

        <SheetSection title="Graph nodes and edges">
          <GraphSample />
        </SheetSection>

        <SheetSection title="File icons at 14 and 16 px">
          <div className="v2-icon-sizes">
            {([14, 16] as const).map((size) => (
              <div key={size} className="v2-icon-size" style={{ ["--v2-icon" as string]: `${size}px` }}>
                <div className="v2-meta">{size} px</div>
                {ICON_SAMPLES.map((p, i) => (
                  <div key={p} className="v2-tree__row v2-tree__row--sample" data-state={i === 2 ? "focused" : i === 4 ? "hover" : i === 6 ? "selected-inactive" : undefined}>
                    <FileGlyph path={p} theme={theme} size={size} />
                    <span className="v2-tree__name">{p.slice(p.lastIndexOf("/") + 1)}</span>
                  </div>
                ))}
              </div>
            ))}
          </div>
        </SheetSection>

        <SheetSection title="Main token values">
          <table className="v2-token-table">
            <tbody>
              {MAIN_ROLES.map((role) => (
                <tr key={role}>
                  <td>
                    <span className="v2-swatch" style={{ background: palette.colors[role] }} />
                  </td>
                  <td>
                    <code>--color-{role}</code>
                  </td>
                  <td>
                    <code>{palette.colors[role]}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </SheetSection>

        <SheetSection title={`Contrast: ${results.length - failed.length} of ${results.length} pass`} wide>
          <p className="v2-meta">Computed with src/ui/color.ts. Text needs 4.5:1; boundaries and focus need 3:1.</p>
          <div className="v2-contrast-cols">
            {chunks(results, 3).map((part, c) => (
              <table key={c} className="v2-contrast-table">
                <thead>
                  <tr>
                    <th>Foreground on background</th>
                    <th className="v2-num">Ratio</th>
                    <th>Result</th>
                  </tr>
                </thead>
                <tbody>
                  {part.map((r, i) => (
                    <tr key={i} data-pass={r.pass ? "true" : "false"} title={r.use}>
                      <td>
                        <span className="v2-pair-sample" style={{ background: palette.colors[r.bg], color: palette.colors[r.fg], borderColor: palette.colors[r.fg] }}>
                          Aa
                        </span>
                        {r.fg} <span className="v2-meta">on</span> {r.bg}
                      </td>
                      <td className="v2-num">
                        {r.ratio.toFixed(2)} / {r.min}
                      </td>
                      <td>{r.pass ? <StatusText tone="success">Pass</StatusText> : <StatusText tone="danger">Fail</StatusText>}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ))}
          </div>
        </SheetSection>
      </div>
    </div>
  );
}

function chunks<T>(items: T[], n: number): T[][] {
  const size = Math.ceil(items.length / n);
  return Array.from({ length: n }, (_, i) => items.slice(i * size, (i + 1) * size));
}

function cap(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function SheetSection({ title, children, wide }: { title: string; children: ReactNode; wide?: boolean }) {
  return (
    <section className="v2-sheet__section" data-wide={wide ? "true" : undefined} aria-label={title}>
      <h2 className="v2-section-title">{title}</h2>
      {children}
    </section>
  );
}

const PY = `def fit(data: pd.DataFrame, unit: str = "K") -> FitResult:
    # Kelvin is the default unit
    slope, b = np.polyfit(1 / t, np.log(data["k"]), 1)
    return FitResult(ea_kj=-slope * R / 1000)`;
const TYP = `= Results <sec:results>
#figure(table2(unit: "K", digits: 3)) <tab:arrhenius>`;

function CodeSample({ lang, code }: { lang: Lang; code: string }) {
  return (
    <pre className="v2-code-sample">
      {code.split("\n").map((line, i) => (
        <div key={i}>
          <span className="v2-code__gutter">{i + 1}</span>
          {tokenizeLine(line, lang).map((t, j) => (
            <span key={j} className={`v2-tok v2-tok--${t.kind}`}>
              {t.text}
            </span>
          ))}
        </div>
      ))}
    </pre>
  );
}

const CATEGORIES = [
  ["structure", "Section", "3 Results"],
  ["document", "Manuscript", "results.typ"],
  ["data", "Dataset", "rate_constants.csv"],
  ["math", "Theorem", "Monotonicity"],
  ["formal", "Lean", "rate_mono"],
  ["reference", "Citation", "laidler1984"],
  ["workflow", "Workflow", "Refit Table 2"],
] as const;

function GraphSample() {
  return (
    <div className="v2-graph">
      <svg className="v2-graph__edges" viewBox="0 0 600 40" preserveAspectRatio="none" aria-hidden>
        <line x1="10" y1="10" x2="140" y2="10" stroke="var(--color-graph-edge)" strokeWidth="1.5" />
        <line x1="160" y1="10" x2="290" y2="10" stroke="var(--color-graph-edge-math)" strokeWidth="1.5" strokeDasharray="6 3" />
        <line x1="310" y1="10" x2="440" y2="10" stroke="var(--color-graph-edge-reference)" strokeWidth="1.5" strokeDasharray="2 3" />
        <line x1="10" y1="30" x2="140" y2="30" stroke="var(--color-graph-edge-evidence)" strokeWidth="1.5" />
        <line x1="160" y1="30" x2="290" y2="30" stroke="var(--color-graph-edge-formal)" strokeWidth="1.5" strokeDasharray="8 2 2 2" />
      </svg>
      <div className="v2-graph__legend v2-meta">Edges: structural · mathematical · citation · evidence · formal</div>
      <div className="v2-graph__nodes">
        {CATEGORIES.map(([cat, type, title], i) => (
          <div key={cat} className="v2-node" data-state={i === 3 ? "selected" : i === 4 ? "focused" : undefined} style={nodeStyle(cat)}>
            <div className="v2-node__band">
              <span className="v2-node__type">{type}</span>
            </div>
            <div className="v2-node__body">
              <span className="v2-node__title">{title}</span>
              {i === 2 ? (
                <Badge tone="stale">Stale</Badge>
              ) : i === 4 ? (
                <StatusText tone="warning" icon={AlertTriangle}>
                  Uses sorry
                </StatusText>
              ) : i === 1 ? (
                <StatusText tone="success">Built</StatusText>
              ) : (
                <span className="v2-meta">{i === 3 ? "selected" : "1 relation"}</span>
              )}
            </div>
          </div>
        ))}
        <div className="v2-node" data-state="dimmed" style={nodeStyle("data")}>
          <div className="v2-node__band">
            <span className="v2-node__type">Dataset</span>
          </div>
          <div className="v2-node__body">
            <span className="v2-node__title">spectra.parquet</span>
            <span className="v2-meta">filtered out</span>
          </div>
        </div>
      </div>
      <p className="v2-meta">Node states: rest, selected (accent outline), focused (focus ring), stale badge, filtered (dimmed).</p>
    </div>
  );
}

function nodeStyle(cat: string): CSSProperties {
  return {
    ["--node-bg" as string]: `var(--color-graph-${cat}-bg)`,
    ["--node-border" as string]: `var(--color-graph-${cat}-border)`,
    ["--node-text" as string]: `var(--color-graph-${cat}-text)`,
  };
}

const ICON_SAMPLES = [
  "sections/results.typ",
  "sections/appendix.tex",
  "analysis/arrhenius_fit.py",
  "analysis/fit.R",
  "analysis/kinetics_model.jl",
  "proofs/ArrheniusBounds.lean",
  "references.bib",
  "pyproject.toml",
  "data/rates.csv",
  "data/spectra.parquet",
  "figures/arrhenius_plot.pdf",
  "notebook.ipynb",
];

const MAIN_ROLES: ColorRole[] = [
  "surface-base",
  "surface-sunken",
  "surface",
  "surface-raised",
  "surface-hover",
  "surface-selected",
  "text",
  "text-muted",
  "text-subtle",
  "border",
  "border-strong",
  "accent",
  "accent-text",
  "accent-subtle",
  "focus-ring",
  "selection",
  "danger-text",
  "warning-text",
  "success-text",
  "info-text",
  "status-stale-text",
];
