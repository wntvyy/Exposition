// Change-impact manifest: which validation a change to each part of the
// repository requires. This is development tooling for choosing tests; it is
// unrelated to the product's own project graph.
//
// Automatic analysis (scripts/impact/repo.mjs) supplies the precise parts:
//   - Rust: the crate that owns a changed file plus every workspace crate that
//     depends on it, directly or transitively (`cargo metadata`, dev-deps too).
//   - Frontend and scripts: every vitest file and e2e scenario that imports a
//     changed module, directly or transitively.
//   - Cross-language reads: tests that read a file by path (include_str!,
//     readFileSync("src/..."), fixture names) depend on that file.
// This manifest supplies what analysis cannot see: the end-to-end scenarios
// that exercise an area through the real app, how far a change escalates, and
// the qualification runs that need credentials, release packaging, or CI.
//
// Maintenance: every tracked file must match at least one area and every e2e
// scenario must be named by an area or SCENARIO_POLICY (manifest.test.mjs
// enforces both). When you add a module, crate, or scenario, add it here.

/**
 * Escalation levels, from narrowest to broadest:
 * - local: the area's own checks plus analysed consumers.
 * - broad: shared behavior (graph, persistence, permissions, contracts, wiring):
 *   adds every frontend test and the INVARIANT_SCENARIOS end-to-end set.
 * - full: uncertain or system-wide impact: the full validation plan.
 */
export const LEVELS = ["none", "local", "broad", "full"];

/** End-to-end scenarios grouped by the behavior they exercise. */
export const SCENARIOS = {
  shell: ["smoke", "ui-shell", "pages-review"],
  visual: ["ui-shell", "pages-review"],
  // Theme switches keep drafts, undo, editor identity, and graph state (UI standard V2).
  themes: ["v2-theme-preservation"],
  // Four-theme tours of populated surfaces (UI standard V2); shots in ui-reference/v2/<area>/.
  tourWorkbench: ["v2-tour-workbench"],
  tourResearch: ["v2-tour-research"],
  editor: [
    "editor-find-outline",
    "workspace-files",
    "workspace-external",
    "workspace-save-failure",
    "workspace-window-close",
    "workspace-recovery",
    "qol-media",
    "qol-journey",
  ],
  explorer: ["workspace-files", "qol-files", "qol-journey", "p1-files", "p1-security", "project-switch"],
  operations: [
    "p1-files",
    "p1-decisions",
    "p1-impacts",
    "p1-storage",
    "p1-research",
    "p1-research-runs",
    "p1-graph-workflows",
    "p1-source-removal",
    "p1-security",
  ],
  pages: ["page-typst", "page-latex", "page-markdown", "page-recovery", "tex-build", "research-attach-build"],
  equation: ["equation-source", "equation-page"],
  research: [
    "research-python",
    "research-isolation",
    "research-output",
    "research-attach-build",
    "research-workspace",
    "p1-research",
    "p1-research-runs",
    "a1-environment",
    "review-m7-ipc",
  ],
  execution: ["execution-jobs", "review-sec-exec", "a1-environment", "research-isolation", "settings", "m9-parallel"],
  agents: [
    "agents-harness",
    "agents-attention",
    "agents-composer",
    "agents-questions",
    "agents-graph",
    "agent-save",
    "agentctx-narration",
    "a1-permissions",
    "a1-responses",
    "review-a1",
    "review-m5-agents",
    "review-m8-agent",
    "graph-dependency-status",
    "project-create",
  ],
  graph: [
    "graph-core",
    "graph-canvas",
    "graph-dependency-status",
    "graph-evidence-impact",
    "graph-lean-association",
    "agents-graph",
    "p1-graph-workflows",
    "p1-source-removal",
    "review-m7-graph-fs",
    "review-m7-ipc",
  ],
  lean: [
    "lean-check",
    "lean-evidence",
    "graph-lean-association",
    "graph-evidence-impact",
    "mathlib-plan",
    "mathlib-setup-guard",
    "review-m4-lean-ui",
    "workflow-formal",
    "workflow-agent-audit",
    "review-m7-ipc",
  ],
  workflows: [
    "workflow-run",
    "workflow-formal",
    "workflow-identity",
    "workflow-preset-template",
    "workflow-repro",
    "workflow-state-refresh",
    "workflow-agent-audit",
    "m9-bundles",
    "m9-parallel",
    "m9-triggers",
    "m9-workflow-ui",
    "review-m8-runs",
    "review-m9",
    "p1-decisions",
    "review-m7-ipc",
  ],
  projects: [
    "project-create",
    "project-switch",
    "workspace-recovery",
    "p1-storage",
    "consistency",
    "m9-bundles",
    "review-m7-graph-fs",
    "m6-paper-project",
    "review-m7-ipc",
  ],
  settings: ["settings", "consistency", "trust-propagation", "a1-permissions", "review-sec-secrets"],
  security: [
    "smoke",
    "review-sec-acl",
    "review-sec-ipc",
    "review-sec-exec",
    "review-sec-render",
    "review-sec-secrets",
    "p1-security",
    "review-m7-ipc",
    "research-isolation",
    "trust-propagation",
  ],
  harness: ["smoke", "ui-shell", "workspace-files", "workspace-recovery", "agents-harness", "agents-attention", "research-python", "review-sec-secrets"],
};

/**
 * Scenarios run for every "broad" change: one pass over the cross-cutting
 * invariants (startup and IPC refusal, shell, file integrity and recovery,
 * graph identity, trust, project switching, the security surface, and the
 * integrated paper project).
 */
export const INVARIANT_SCENARIOS = [
  "smoke",
  "ui-shell",
  "pages-review",
  "workspace-files",
  "workspace-external",
  "workspace-recovery",
  "graph-core",
  "consistency",
  "trust-propagation",
  "project-switch",
  "qol-journey",
  "p1-security",
  "review-sec-acl",
  "review-sec-ipc",
  "m6-paper-project",
];

/**
 * Scenarios that never run automatically, with the reason. `live-*` call a paid
 * model provider with the user's saved key; the others need prepared external
 * state. They appear in plans as deferred qualification items and run only with
 * explicit authorization (`--live`) or by hand.
 */
export const SCENARIO_POLICY = {
  "live-a1-loop": "live",
  "live-agentctx": "live",
  "live-agents": "live",
  "live-attachments": "live",
  "live-graph": "live",
  "live-m9-agents": "live",
  "live-restricted": "live",
  "live-review-m8-agent": "live",
  "live-workflow": "live",
  "live-workflow-audit": "live",
  mathlib: "manual: needs EXPOSITION_MATHLIB_PACKAGE (a prepared Mathlib package)",
  "mathlib-setup": "manual: needs EXPOSITION_MATHLIB_SETUP=1, network, and several GB for Mathlib",
};

const LIVE_AGENTS = ["live-agents", "live-attachments", "live-agentctx", "live-a1-loop", "live-graph", "live-restricted"];
const LIVE_WORKFLOWS = ["live-workflow", "live-workflow-audit", "live-m9-agents", "live-review-m8-agent"];

/**
 * Areas. `paths` are repository-relative globs (`*` within a segment, `**`
 * across segments). A file may match several areas; their requirements add up.
 * - level: escalation (see LEVELS).
 * - e2e: scenarios for behavior changes; `visual` replaces it when only
 *   stylesheets in the area changed.
 * - checks: extra named checks (see CHECKS in plan.mjs).
 * - live: live scenarios to list as deferred qualification.
 * - deferred: other qualification that this run cannot do (release, CI).
 * - note: shown in the plan.
 */
export const AREAS = [
  // ---- Documentation and repository metadata
  {
    id: "docs",
    paths: ["docs/**", "README.md"],
    level: "local",
    e2e: [],
    note: "Documentation: no automated check reads it; review the text against current behavior.",
  },
  {
    id: "prototypes",
    paths: ["prototypes/**"],
    level: "local",
    e2e: [],
    note: "Design-review harnesses (standalone Vite pages and screenshots; not part of the app). Typecheck covers their TSX; re-shoot to review.",
  },
  { id: "git-ignore", paths: [".gitignore"], level: "local", e2e: [], note: "Ignore rules: the private-path guard verifies them." },
  {
    id: "line-endings",
    paths: [".gitattributes"],
    level: "full",
    e2e: [],
    note: "Line-ending rules change the checked-out bytes of every source and of the byte-exact encoding fixtures.",
  },

  // ---- Toolchains, dependencies, and build configuration (system-wide)
  {
    id: "toolchain",
    paths: ["package-lock.json", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/**", "rustfmt.toml", "tsconfig.json", "vite.config.ts"],
    level: "full",
    e2e: [],
    note: "Dependency, toolchain, or build configuration: every consumer may change.",
  },
  // package.json is classified by content (plan.mjs: added script entries only
  // are local; any other change is full).
  { id: "package-manifest", paths: ["package.json"], level: "local", e2e: [] },
  {
    id: "crate-manifests",
    paths: ["crates/*/Cargo.toml", "src-tauri/Cargo.toml"],
    level: "broad",
    e2e: [],
    note: "A crate manifest changes dependency wiring: the crate and its dependents are tested.",
  },
  {
    id: "ci",
    paths: [".github/**"],
    level: "local",
    e2e: [],
    checks: ["check-all"],
    deferred: ["ci"],
    note: "CI runs `npm run check`; the workflow file itself is verified only by a GitHub run.",
  },
  {
    id: "check-tooling",
    paths: ["scripts/check.mjs", "scripts/check-*.mjs"],
    level: "local",
    e2e: [],
    checks: ["check-all"],
    note: "The CI gate runner and its gates: `npm run check` runs in full.",
  },
  {
    id: "impact-tooling",
    paths: ["scripts/impact.mjs", "scripts/impact/**"],
    level: "local",
    e2e: [],
    note: "The impact selector: its own tests (scripts/impact/*.test.mjs) cover planning and execution.",
  },

  // ---- End-to-end harness
  {
    id: "e2e-harness-core",
    paths: ["scripts/e2e/run.mjs", "scripts/e2e/app.mjs", "scripts/e2e/app-env.mjs", "scripts/e2e/processes.mjs", "scripts/e2e/wait.mjs"],
    level: "local",
    e2e: SCENARIOS.harness,
    note: "Harness core (launch, isolation, credentials namespace, process cleanup): a representative scenario set runs.",
  },
  {
    id: "e2e-helpers",
    paths: ["scripts/e2e/*.mjs", "scripts/e2e/*.ps1"],
    level: "local",
    e2e: [],
    note: "Scenario helpers: scenarios that import them run (import analysis).",
  },
  { id: "e2e-scenarios", paths: ["scripts/e2e/scenarios/*.mjs"], level: "local", e2e: [] },
  {
    id: "e2e-repro",
    paths: ["scripts/e2e/repro/**"],
    level: "local",
    e2e: [],
    deferred: ["manual-repro"],
    note: "Manual reproduction scripts: not part of any automated run.",
  },
  { id: "fixtures", paths: ["tests/fixtures/**"], level: "local", e2e: [] },

  // ---- Native shell, permissions, release
  {
    id: "permissions",
    paths: ["src-tauri/capabilities/**", "src-tauri/permissions/**", "src-tauri/build.rs"],
    level: "broad",
    e2e: SCENARIOS.security,
    checks: ["permissions"],
    note: "Command permissions and capabilities are a security boundary.",
  },
  {
    id: "tauri-config",
    paths: ["src-tauri/tauri.conf.json"],
    level: "broad",
    e2e: SCENARIOS.security,
    checks: ["permissions"],
    deferred: ["release"],
    note: "App configuration (CSP, windows, bundling): security surface and release packaging.",
  },
  {
    id: "release-assets",
    paths: ["src-tauri/icons/**", "assets/**"],
    level: "local",
    e2e: ["smoke"],
    deferred: ["release"],
    note: "Icons are embedded at build time; the installer needs a release build to verify.",
  },
  {
    // review-sec-acl reads every src-tauri/src/*.rs through a variable path
    // (command registration, handlers, grants), which analysis cannot see.
    id: "tauri-commands",
    paths: ["src-tauri/src/**"],
    level: "local",
    e2e: ["smoke", "review-sec-acl"],
    note: "Every command module is checked statically by review-sec-acl (registered, listed, granted).",
  },
  {
    id: "ipc-core",
    paths: ["src-tauri/src/lib.rs", "src-tauri/src/main.rs", "src-tauri/src/state.rs", "src-tauri/src/error.rs", "src-tauri/src/app.rs", "src/lib/ipc.ts"],
    level: "broad",
    e2e: SCENARIOS.security,
    note: "Command registration, shared state, and the IPC client are used by every feature.",
  },

  // ---- Shared contracts and services
  {
    id: "contracts",
    paths: ["src/bindings/**", "crates/exposition-core/**"],
    level: "broad",
    e2e: [],
    checks: ["bindings-full"],
    note: "Shared contracts: every Rust consumer and every frontend consumer is affected.",
  },
  {
    id: "documents",
    paths: ["crates/exposition-workspace/**", "src-tauri/src/documents.rs", "src-tauri/src/workspace.rs"],
    level: "broad",
    e2e: [...SCENARIOS.editor, ...SCENARIOS.explorer],
    note: "Document service, saves, recovery, and file operations protect user files.",
  },
  {
    id: "execution",
    paths: ["crates/exposition-execution/**", "src/features/jobs/**", "src-tauri/src/execution.rs", "src-tauri/src/setup.rs"],
    level: "broad",
    e2e: [...SCENARIOS.execution, ...SCENARIOS.security],
    visual: ["execution-jobs"],
    live: ["live-restricted"],
    note: "Execution service: restricted/native boundaries, trust, and process containment.",
  },
  {
    id: "graph",
    paths: ["crates/exposition-graph/**", "src/features/graph/**", "src/lib/graphIpc.ts", "src-tauri/src/graph.rs", "src-tauri/src/source_removal.rs"],
    level: "broad",
    e2e: [...SCENARIOS.graph, ...SCENARIOS.tourResearch],
    visual: ["graph-canvas", "pages-review", ...SCENARIOS.tourResearch],
    live: ["live-graph"],
    note: "Graph semantics and consistency are shared by research, Lean, agents, and workflows.",
  },
  {
    id: "projects",
    paths: ["src/features/projects/**", "src-tauri/src/projects.rs", "src-tauri/src/project_data.rs"],
    level: "broad",
    e2e: SCENARIOS.projects,
    visual: ["project-create"],
    note: "Project persistence, creation, migrations, and import/export.",
  },

  // ---- Features
  {
    id: "shell",
    paths: ["src/app/**", "src/App.tsx", "src/main.tsx", "src/features/index.ts", "src/vite-env.d.ts", "index.html"],
    level: "broad",
    e2e: [...SCENARIOS.shell, ...SCENARIOS.tourWorkbench],
    visual: [...SCENARIOS.visual, ...SCENARIOS.tourWorkbench],
    note: "Workbench wiring (registry, layout, keybindings, pages) hosts every feature.",
  },
  {
    id: "ui-system",
    paths: ["src/ui/**", "src/styles.css"],
    level: "broad",
    e2e: [...SCENARIOS.shell, ...SCENARIOS.themes, "editor-find-outline", "graph-canvas"],
    visual: [...SCENARIOS.visual, "editor-find-outline", "graph-canvas"],
    note: "Shared UI tokens and components: compare the refreshed ui-reference screens with the previous ones.",
  },
  {
    id: "editor",
    paths: ["src/features/editor/**", "src/features/outline/**", "src/features/media/**", "src/lib/monaco.ts", "src-tauri/src/outline.rs", "src-tauri/src/media.rs"],
    level: "local",
    // Equation writes go through the source editor's document handle.
    e2e: [...SCENARIOS.editor, "equation-source"],
    visual: ["editor-find-outline", "qol-media"],
  },
  {
    id: "explorer",
    paths: ["src/features/workspace/**", "src-tauri/src/files.rs", "src-tauri/src/clipboard.rs"],
    level: "local",
    e2e: [...SCENARIOS.explorer, ...SCENARIOS.tourWorkbench],
    visual: ["qol-files", "p1-files", ...SCENARIOS.tourWorkbench],
  },
  {
    id: "operations",
    paths: ["src/features/operations/**", "src-tauri/src/project_ops.rs", "src-tauri/src/project_ops/**"],
    level: "local",
    e2e: SCENARIOS.operations,
    visual: ["p1-files"],
    note: "Project operations (delete, move, restore) and their consequence decisions.",
  },
  {
    id: "pages",
    paths: [
      "crates/exposition-page/**",
      "crates/exposition-tex/**",
      "crates/exposition-typesetting/**",
      "crates/exposition-markdown/**",
      "src/features/page/**",
      "src/features/typesetting/**",
      "src-tauri/src/page.rs",
      "src-tauri/src/tex.rs",
      "src-tauri/src/typesetting.rs",
    ],
    level: "local",
    // The equation editor reads the page caret (activePage.ts, PageSession.currentCaret).
    e2e: [...SCENARIOS.pages, "equation-page"],
    visual: ["page-typst", "pages-review"],
  },
  {
    // Visual equation editor (core: math tree, language adapters, catalog;
    // panel: UI and source/page integration). It edits documents of every
    // typesetting language through both surfaces, so its scenarios are the
    // page-editing ones plus the editor's.
    id: "equation",
    paths: ["src/features/math/**"],
    level: "local",
    e2e: [...SCENARIOS.equation, ...SCENARIOS.pages, "editor-find-outline"],
    visual: ["equation-source", "equation-page", "page-typst", "editor-find-outline", ...SCENARIOS.tourResearch],
    note: "Equation editor: compiled output from each language is authoritative; KaTeX preview is approximate.",
  },
  {
    id: "research",
    paths: ["crates/exposition-research/**", "crates/exposition-attach/**", "workers/**", "src/features/research/**", "src-tauri/src/research.rs"],
    level: "local",
    e2e: [...SCENARIOS.research, ...SCENARIOS.tourResearch],
    visual: ["research-workspace", ...SCENARIOS.tourResearch],
    note: "Research execution, retention, and artifact promotion.",
  },
  {
    id: "agents",
    paths: ["crates/exposition-agents/**", "src/features/agents/**", "src-tauri/src/agents.rs"],
    level: "local",
    e2e: SCENARIOS.agents,
    visual: ["a1-responses"],
    live: LIVE_AGENTS,
    note: "The scripted provider replaces only the model; provider requests, tool schemas, and prompts are qualified only by live runs.",
  },
  {
    id: "lean",
    paths: ["crates/exposition-lean/**", "src/features/lean/**", "src-tauri/src/lean.rs"],
    level: "local",
    e2e: [...SCENARIOS.lean, ...SCENARIOS.tourResearch],
    visual: ["review-m4-lean-ui", ...SCENARIOS.tourResearch],
    deferred: ["mathlib"],
  },
  {
    id: "workflows",
    paths: ["crates/exposition-workflow/**", "src/features/workflows/**", "src-tauri/src/workflows.rs", "src-tauri/src/workflow_*.rs"],
    level: "local",
    e2e: [...SCENARIOS.workflows, ...SCENARIOS.tourResearch],
    visual: ["m9-workflow-ui", ...SCENARIOS.tourResearch],
    live: LIVE_WORKFLOWS,
  },
  {
    id: "settings",
    paths: ["src/features/settings/**", "src-tauri/src/settings.rs"],
    level: "local",
    e2e: [...SCENARIOS.settings, ...SCENARIOS.tourWorkbench],
    visual: ["settings", ...SCENARIOS.tourWorkbench],
  },
];

/** Qualification items a run cannot complete by itself. */
export const DEFERRED = {
  release: {
    label: "Release build and release smoke",
    command: ["npm", "run", "build:release"],
    reason:
      "Packaging and embedded resources are verified only by a release build; then run smoke, ui-shell, pages-review with EXPOSITION_E2E_EXECUTABLE set to the release exe (never key-touching scenarios).",
    flag: "--release",
  },
  ci: {
    label: "GitHub CI run",
    reason: "A workflow-file change is verified only by a CI run on GitHub after an authorized push.",
  },
  mathlib: {
    label: "Mathlib qualification (mathlib, mathlib-setup scenarios)",
    reason: "Needs a prepared Mathlib package (EXPOSITION_MATHLIB_PACKAGE) or network setup (EXPOSITION_MATHLIB_SETUP=1).",
  },
  "manual-repro": {
    label: "Manual reproduction script",
    reason: "scripts/e2e/repro is run by hand when investigating the issue it reproduces.",
  },
};

/** Test-file conventions (must match vite.config.ts `test.include`). */
export const VITEST_INCLUDE = ["src/**/*.test.{ts,tsx}", "scripts/**/*.test.mjs"];
export const SCENARIO_DIR = "scripts/e2e/scenarios";
export const FIXTURE_DIR = "tests/fixtures";
