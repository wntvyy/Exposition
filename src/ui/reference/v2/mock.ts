// V2 review samples: shared mock content. Every direction renders exactly this
// data, so the user compares presentation only. Nothing here touches a
// project, a credential, a document, or the graph.

export type Lang = "typst" | "latex" | "python" | "lean";

export interface TreeNode {
  name: string;
  kind: "file" | "folder";
  children?: TreeNode[];
  expanded?: boolean;
}

const file = (name: string): TreeNode => ({ name, kind: "file" });
const folder = (name: string, children: TreeNode[], expanded = true): TreeNode => ({ name, kind: "folder", children, expanded });

export const PROJECT_NAME = "arrhenius-kinetics";

/** The Files tree in the Editor, Settings, and Agent views. */
export const EDITOR_TREE: TreeNode[] = [
  folder(".exposition", [file("project.json")], false),
  folder("analysis", [
    folder("legacy.py", [file("old_fit.py")], false),
    file("arrhenius_fit.py"),
    file("bootstrap_confidence_intervals_for_activation_energy.R"),
    file("kinetics_model.jl"),
    file("exploratory_rate_constants.ipynb"),
  ]),
  folder("data", [file("rate_constants_2026-03-14_calibrated.csv")], false),
  folder("figures", [file("arrhenius_plot.pdf")], false),
  folder("proofs", [file("ArrheniusBounds.lean"), file("lakefile.lean"), file("lean-toolchain")]),
  folder("sections", [file("results.typ"), file("appendix_derivation_of_the_modified_arrhenius_equation.tex"), file("methods.md")]),
  file("references.bib"),
  file("pyproject.toml"),
  file(".Rprofile"),
];

/** The explorer icon sample: every main mark, overrides, and fallbacks. */
export const ICON_TREE: TreeNode[] = [
  folder(".exposition", [file("project.json")]),
  folder(".venv", [file("pyvenv.cfg")]),
  folder("analysis", [
    folder("legacy.py", [], false),
    file("arrhenius_fit.py"),
    file("bootstrap_confidence_intervals_for_activation_energy.R"),
    file("fit_summary.RData"),
    file("kinetics_model.jl"),
    file("exploratory_rate_constants.ipynb"),
  ]),
  folder("data", [
    file("rate_constants_2026-03-14_calibrated.csv"),
    file("spectra.parquet"),
    file("cell_temperatures.mat"),
    file("project.json"),
    file("instrument-settings.yaml"),
    file("measurement.xyz"),
    file("raw_spectra.zip"),
  ]),
  folder("figures", [file("arrhenius_plot.pdf"), file("arrhenius_plot.svg"), file("setup_photo.png"), file("report.html")]),
  folder("proofs", [file("ArrheniusBounds.lean"), file("lakefile.lean"), file("lean-toolchain"), file("lake-manifest.json")]),
  folder("sections", [file("results.typ"), file("appendix_derivation_of_the_modified_arrhenius_equation.tex"), file("methods.md"), file("notes.txt")]),
  folder("styles", [file("thesis.cls"), file("unsrtnat.bst")]),
  file("references.bib"),
  file("pyproject.toml"),
  file(".Rprofile"),
  file("CHANGELOG"),
];

export interface EditorTab {
  id: string;
  path: string;
  lang: Lang;
  dirty?: boolean;
  code: string;
}

export const TABS: EditorTab[] = [
  {
    id: "results",
    path: "sections/results.typ",
    lang: "typst",
    dirty: true,
    code: `// Results: Arrhenius fit of the corrected rate constants
#import "@preview/ctheorems:1.1.3": *
#import "../analysis/table2.typ": table2

= Results <sec:results>

The corrected calibration of 2026-03-14 gives an activation energy of
$E_a = 53.4 plus.minus 0.6 "kJ/mol"$ (@tab:arrhenius), consistent with
the bound proved in \`ArrheniusBounds.lean\`.

#figure(
  table2(unit: "K", digits: 3),
  caption: [Arrhenius parameters fitted per temperature range.],
) <tab:arrhenius>

#let k(T) = A * calc.exp(-Ea_kj / (R * T))
#theorem[
  For $T_1 < T_2$ the fitted rate satisfies $k(T_1) < k(T_2)$.
]`,
  },
  {
    id: "appendix",
    path: "sections/appendix_derivation_of_the_modified_arrhenius_equation.tex",
    lang: "latex",
    code: `\\section{Derivation of the modified Arrhenius equation}\\label{sec:derivation}
% The pre-exponential factor depends weakly on temperature.
\\begin{equation}
  k(T) = A\\,T^{n} \\exp\\!\\left(-\\frac{E_a}{R T}\\right)
  \\label{eq:modified-arrhenius}
\\end{equation}
Taking logarithms, $\\ln k = \\ln A + n \\ln T - E_a / (R T)$, so a linear
fit in $1/T$ recovers $E_a$ when $n = 0$ \\cite{laidler1984}.

\\begin{theorem}[Monotonicity]\\label{thm:monotone}
  If $E_a > 0$ and $n \\ge 0$, then $k$ is increasing in $T$.
\\end{theorem}`,
  },
  {
    id: "fit",
    path: "analysis/arrhenius_fit.py",
    lang: "python",
    code: `"""Arrhenius fit for Table 2."""
from dataclasses import dataclass

import numpy as np
import pandas as pd

R = 8.314462618  # J/(mol K)


@dataclass
class FitResult:
    ln_a: float
    ea_kj: float
    stderr: float

    def table(self) -> pd.DataFrame:
        return pd.DataFrame({"ln A": [self.ln_a], "Ea (kJ/mol)": [self.ea_kj]})


def fit(data: pd.DataFrame, temperature_unit: str = "K") -> FitResult:
    t = data["T"].to_numpy()
    if temperature_unit == "C":
        t = t + 273.15
    slope, intercept = np.polyfit(1 / t, np.log(data["k"]), 1)
    return FitResult(ln_a=intercept, ea_kj=-slope * R / 1000, stderr=0.6)`,
  },
  {
    id: "lean",
    path: "proofs/ArrheniusBounds.lean",
    lang: "lean",
    code: `import Mathlib.Analysis.SpecialFunctions.Exp

/-- The Arrhenius rate law with positive activation energy. -/
noncomputable def rate (A Ea R T : ℝ) : ℝ := A * Real.exp (-Ea / (R * T))

theorem rate_pos {A Ea R T : ℝ} (hA : 0 < A) : 0 < rate A Ea R T := by
  unfold rate
  positivity

theorem rate_mono {A Ea R : ℝ} (hA : 0 < A) (hEa : 0 < Ea) (hR : 0 < R)
    {T₁ T₂ : ℝ} (h₁ : 0 < T₁) (h : T₁ < T₂) : rate A Ea R T₁ < rate A Ea R T₂ := by
  sorry`,
  },
];

export interface Problem {
  path: string;
  severity: "error" | "warning";
  message: string;
  line: number;
  col: number;
  source: string;
}

export const PROBLEMS: Problem[] = [
  { path: "sections/results.typ", severity: "error", message: "unknown variable: Ea_kj", line: 16, col: 27, source: "Typst" },
  { path: "proofs/ArrheniusBounds.lean", severity: "warning", message: "declaration uses 'sorry'", line: 10, col: 9, source: "Lean" },
];

/** Squiggle positions per tab (line is 1-based, columns 1-based, inclusive start). */
export const SQUIGGLES: Record<string, { line: number; col: number; length: number; severity: "error" | "warning" }[]> = {
  results: [{ line: 16, col: 27, length: 5, severity: "error" }],
  lean: [{ line: 10, col: 9, length: 9, severity: "warning" }],
};

// ---- Agent

const APPROVAL_CODE = `import pandas as pd
from analysis.arrhenius_fit import fit
data = pd.read_csv("data/rate_constants_2026-03-14_calibrated.csv")
result = fit(data, temperature_unit="K")
result.table().to_csv("data/table2.csv", index=False)
print(result)`;

export const AGENT = {
  title: "Refit Table 2 with the corrected Arrhenius calibration",
  prompt:
    "Refit the Arrhenius model with the corrected 2026-03-14 calibration, regenerate Table 2 in sections/results.typ, and keep the figure axis in Kelvin.",
  turn1Summary: "Read 3 files · Edited sections/results.typ (unsaved) · 1 figure",
  turn1Text: "I updated the Table 2 call in results.typ to read the new CSV. The change is in the open document and stays unsaved until you save it.",
  turn2Summary: "Ran Python · 2 waiting",
  turn2Text:
    "The corrected calibration moves the activation energy from 52.1 to 53.4 kJ/mol. I need to rerun the fit to regenerate the table, and I need one decision about units.",
  approval: {
    title: "Run Python code",
    payload: `Run in this task's separate research session (${new TextEncoder().encode(APPROVAL_CODE).length} bytes):

${APPROVAL_CODE}

Runs natively with your account's privileges.`,
    scope: "Approve for this task also covers later native code in this task; revoke it from the task.",
  },
  question: {
    banner: "The agent waits for your answer. It is passed on as your reply and approves nothing.",
    progress: "0 of 1 answered",
    text: "Which temperature unit should Table 2 use?",
    detail: "The fit uses Kelvin; the lab notebook records Celsius.",
    options: [
      { id: "k", label: "Kelvin", description: "Matches the fit and the figure axis", recommended: true },
      { id: "c", label: "Celsius", description: "Matches the lab notebook" },
    ],
  },
  context: `${PROJECT_NAME} · Whole project`,
  options: "Options: claude-opus-5-5 · High · Ask · Ask before each change",
};

/** "11 lines · 352 bytes" for an approval payload, as the production card counts it. */
export function payloadSize(text: string): string {
  const lines = text.split("\n").length;
  const bytes = new TextEncoder().encode(text).length;
  return `${lines} lines · ${bytes} bytes`;
}

// ---- Settings

export const SETTINGS_NAV = [
  "Appearance",
  "Editor",
  "Project",
  "Trusted folders",
  "Execution",
  "Model providers",
  "Research",
  "Storage & History",
  "About",
] as const;
export type SettingsPage = (typeof SETTINGS_NAV)[number];
