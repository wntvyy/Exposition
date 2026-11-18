// V2 review samples: the explorer icon sample in one palette — the Files tree
// at 14 px (current gutter) and 16 px, row states, and how each name resolved.
import { ChromeLabel } from "./kit";
import { FileTree, type RowState } from "./Workbench";
import { resolveFileIcon, type ThemeKind } from "./fileIcons";
import { ICON_TREE, type TreeNode } from "./mock";
import type { V2Palette } from "./palettes";

const RULE_LABEL = {
  folder: "Folder (node kind first)",
  path: "Project-relative path",
  filename: "Exact file name",
  extension: "Extension",
  "existing-glyph": "No package asset: current glyph",
  generic: "Generic fallback",
} as const;

function allNodes(nodes: TreeNode[], prefix = "", out: { path: string; node: TreeNode }[] = []) {
  for (const node of nodes) {
    const path = prefix ? `${prefix}/${node.name}` : node.name;
    out.push({ path, node });
    if (node.children) allNodes(node.children, path, out);
  }
  return out;
}

const FORCED: Record<string, RowState> = {
  "analysis/kinetics_model.jl": "hover",
  "sections/results.typ": "focused",
  "proofs/lakefile.lean": "selected",
  "data/raw_spectra.zip": "disabled",
};

export function IconSample({ palette }: { palette: V2Palette }) {
  const theme: ThemeKind = palette.kind;
  const rows = allNodes(ICON_TREE);
  return (
    <div className="v2-icons" data-testid={`v2-icons-${palette.id}`}>
      <header className="v2-sheet__head">
        <h1 className="v2-page-title">File icons · {palette.name}</h1>
        <p className="v2-page-lead">
          Supplied Material Icon Theme assets (MIT; see icons/THIRD_PARTY_NOTICES.md), unchanged, in the existing icon gutter. Folders keep the current treatment.
        </p>
      </header>
      <div className="v2-icons__cols">
        {([14, 16] as const).map((size) => (
          <section key={size} className="v2-icons__tree" aria-label={`Files at ${size} px`}>
            <div className="v2-pane-header">
              <ChromeLabel>Files · {size} px icons</ChromeLabel>
            </div>
            <FileTree tree={ICON_TREE} theme={theme} iconSize={size} forced={FORCED} label={`Files sample at ${size} px`} />
          </section>
        ))}
        <section className="v2-icons__table" aria-label="How each name resolved">
          <h2 className="v2-section-title">How each name resolved</h2>
          <p className="v2-meta">Hovered: kinetics_model.jl · focused: results.typ · selected: lakefile.lean · disabled: raw_spectra.zip</p>
          <table className="v2-contrast-table">
            <thead>
              <tr>
                <th>Path</th>
                <th>Rule</th>
                <th>Icon</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(({ path, node }) => {
                const r = resolveFileIcon(path, node.kind, theme);
                return (
                  <tr key={path}>
                    <td>
                      <code>{path}</code>
                    </td>
                    <td>{RULE_LABEL[r.rule]}</td>
                    <td>
                      <code>{r.asset ? `${r.asset}.svg` : r.legacyType ? `(${r.legacyType} glyph)` : "folder"}</code>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>
      </div>
    </div>
  );
}
