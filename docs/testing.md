# Testing and validation

Exposition has three layers of automated checks: the CI gates of `npm run check`
(private-path guard, permission files, typecheck, vitest, Vite build, rustfmt,
clippy, `cargo test`, bindings drift), the end-to-end scenarios that drive the
built desktop app (`npm run e2e`), and qualification runs that need credentials
or packaging (live model-provider scenarios, release builds). Running all of it
takes well over an hour on the development host, so routine work runs the
checks a change actually requires, chosen by the change-impact selector in
`scripts/impact`. This is development tooling; it is unrelated to the
product's project graph.

## Commands

| Command | What it does |
|---|---|
| `npm run check:plan` | Prints the plan for the current changes: every check, why it was selected, what is deferred, what is uncovered. Runs nothing. `-- --json` for machine-readable output. |
| `npm run check:affected` | Resolves the same plan and runs it. |
| `npm run check:full` | Full validation: `npm run check`, `npm run build:app`, and every end-to-end scenario except live and manual ones (named one by one, so the run-by-name review scenarios run too). |
| `npm run check` | The CI gates, unchanged. CI runs this on every push and pull request. |
| `npm run e2e -- <scenario>...` | Named scenarios against the built app, unchanged. |

Options (after `--`): `--base <ref>`, `--json`, `--json-out <file>`,
`--expect-plan <hash>`, `--jobs <n>`, `--e2e-jobs <n>`, `--skip <kinds or ids>`, `--fail-fast`,
`--live`, `--release`, `--allow-full`, `--full`. `node scripts/impact.mjs` without arguments
prints the usage.

To run exactly what a preview showed: `npm run check:plan` prints the plan hash
(`Impact plan 4a9d2675f261 …`); `npm run check:affected -- --expect-plan
4a9d2675f261` refuses to start if the changes or options now resolve to a
different plan. Preview and execution call the same planner on the same inputs,
and the plan is deterministic: the same tree, base, and options give the same
hash.

## What counts as "the changes"

The comparison base is the merge base of `HEAD` and `origin/main` (or `main`
when there is no `origin/main`), so commits that landed on `main` after the
branch started are not counted. Override it with `--base <ref>` or the
`EXPOSITION_IMPACT_BASE` environment variable. The change set is:

- committed changes on the branch since the merge base,
- staged and unstaged changes in the working tree,
- untracked files that are not ignored,
- renames, counted under both the old and the new path,
- deletions; files that still import a deleted module are found and tested.

An explicit base that does not name a commit is an error (exit status 2). When
no default base exists, `HEAD` has no commit, or a shallow clone lacks the merge
base, the plan broadens to full validation and says why. Ignored files
(`target/`, `node_modules/`, local planning files) are never part of a plan.

## How checks are selected

**Baseline.** Every plan runs the private-path guard
(`scripts/check-private-paths.mjs`, measured 0.2 s): any change can stage a
private file, and the guard is the only check that sees the whole index.

**Automatic analysis.** The selector reads the repository on each run (about
0.2–0.5 s):

- *Frontend and scripts.* A reverse import graph over `src/` and `scripts/`
  (static and dynamic imports with literal specifiers, `vi.mock`, CSS
  `@import`). A change selects every vitest file and every e2e scenario that
  imports it directly or transitively. A shared component therefore selects the
  tests of every feature that uses it, not only its neighbors.
- *Rust.* The crate that owns a changed file plus every workspace crate that
  depends on it, directly or transitively, including dev-dependencies
  (`cargo metadata`). A file only tests use (`tests/`, `tests.rs`,
  `*_tests.rs`) selects only its own crate. A `-p` selection always includes
  the app crate (`exposition`, which depends on every crate): without it Cargo
  resolves different dependency features for the subset and rebuilds the
  third-party dependencies (measured: 141 crates, 18 minutes at one build job),
  while with it the warm workspace build is reused (the same selection: 2
  minutes, workspace crates only).
- *Files read by tests.* Path literals in tests and scripts
  (`readFileSync("src/ui/tokens.css")`, `join(ROOT, "package.json")`), files
  embedded with `include_str!`, and fixture names (`fixture("lean-basic")`)
  make the reading test a consumer. Example: the agents crate test that checks
  interface names in `src/features/*/register.tsx` runs when those files
  change, and the kernel host Python file, compiled into the research crate,
  selects the research crate and its dependents.

- *Crates built on a changed crate.* The Rust tests of a dependent crate do not
  show that its features still work in the app. An area can name the scenarios
  that exercise it through its dependencies (`dependents` in the manifest); they
  run when a crate one of the area's crates depends on changes. Example: a
  typesetting change runs the agent scenarios in which an agent edits, saves,
  and builds a document; a Lean change runs a formal-audit workflow.

**Explicit mapping.** `scripts/impact/manifest.mjs` maps areas of the
repository (editor, explorer, project operations, pages, research, execution,
agents, graph, Lean, workflows, projects, settings, shell, shared UI, contracts,
permissions, build configuration, …) to the end-to-end scenarios that exercise
them through the real app, to an escalation level, and to qualification runs
the plan cannot do itself.

**Escalation levels.**

| Level | Applies to | Adds |
|---|---|---|
| local | feature code, a crate's own tests, scenarios, fixtures, docs | the area's scenarios and the analysed consumers |
| broad | graph, document/persistence service, execution and trust, permissions and capabilities, contracts and bindings, IPC wiring, workbench shell, shared UI tokens and components, crate manifests | every vitest file, typecheck, Vite build, and the invariant scenarios (startup and IPC refusal, shell, file integrity and recovery, graph identity, trust propagation, project switching, the security surface, the integrated paper project) |
| full | toolchains, lockfiles, `tsconfig`/`vite.config`, `.gitattributes`, `package.json` changes other than new ordinary scripts (dependencies, existing scripts, or `pre*`/`post*`/lifecycle scripts npm runs implicitly), and any uncertainty | full validation (as `npm run check:full`) |

Test-only changes never escalate. A change limited to stylesheets runs the
area's visual scenarios; compare the refreshed `ui-reference` screens with the
previous ones.

**Uncertainty broadens the plan.** A path that matches no area, a manifest
entry naming a scenario that does not exist, an unavailable crate graph, or an
unusable comparison base escalates to full validation. A fixture that no test
or scenario names escalates to broad. A changed or deleted file that no
selected check exercises (documentation, for instance, or a script a scenario
reaches through a variable) is listed as *uncovered*: the run can pass, but it
is reported as not complete. Where analysis is known to be blind, the manifest
maps explicitly (for example, every `src-tauri/src` change runs
`review-sec-acl`, which reads all command modules through a variable path).

## Check kinds

| Kind | Checks | Runs when |
|---|---|---|
| static | private paths, permission files | always / permissions or capabilities change |
| frontend | typecheck, vitest (selected files or the suite), Vite build | frontend sources change or the plan is broad |
| rust | rustfmt, clippy and `cargo test` for the selected crates, bindings | Rust or crate files change, or a test reads a changed file |
| gates | `npm run check` | full validation, CI workflow or gate runner changes |
| native-build | `npm run build:app` | any end-to-end scenario is planned |
| e2e | `node scripts/e2e/run.mjs <scenario>`, one process per scenario | the area mapping or import analysis selects it |
| live | every `live-*` scenario (paid model-provider requests), listed in the manifest or not | never implicitly: listed as deferred; `--live` runs them, only with the account owner's authorization |
| release | `npm run build:release`, then `smoke`, `ui-shell`, `pages-review` against `target/release/exposition.exe` | listed as deferred for packaging, icon, and app-configuration changes; `--release` runs both |

Bindings: when a changed Rust file defines exported contract types, the plan
regenerates the bindings of the owning crates and compares them with the staged
files (`scripts/impact/bindings-drift.mjs`). When a change removes exported
types, or touches the shared contracts, the full bindings gate runs instead,
because only it detects stale files of removed types.

Scenarios that need a tool (Docker, a Lean toolchain, Windows) skip themselves
when it is missing. The runner reads that and reports the scenario as
*unavailable*, not passed. `mathlib` and `mathlib-setup` need prepared external
state and are always deferred, as are the live scenarios.

**Scenario inventory.** `scripts/e2e/scenarios/` holds 105 scenario files
(every `.mjs` file there is a scenario; helper modules live one folder up in
`scripts/e2e/` and are not counted). `npm run e2e` without names runs the 94
whose name does not start with `live-`; eight of those (`review-m5-agents`,
`review-m7-graph-fs`, `review-m7-ipc`, and the five `review-sec-*`) skip
themselves unless they are named. The other eleven are the `live-*` scenarios.

**Scenarios that need a debug build.** Scenarios run against the debug build
made by `npm run build:app` unless `EXPOSITION_E2E_EXECUTABLE` names another
executable. Sixteen IPC commands exist only in debug builds (`DEV_COMMANDS`
in `src-tauri/build.rs`): the scripted model provider's script loader, two
fixed jobs, two fault injections, and eleven probes of live services that no
interface calls (`tex_build`, `tex_status`, `tex_page_image`, `tex_page_map`,
`lean_check`, `lean_package_inspect`, `graph_reconcile`, `graph_backup`,
`graph_backups`, `graph_operations`, `research_space`). 42 scenario files
name one of them and need a debug build for those parts; the 21 that call a
probe say "Requires a debug build" in their header, and `m6-paper-project`
records its `lean_check` part as not verified in a release build instead. The
release run uses only `smoke`, `ui-shell`, and `pages-review`, which need
none of them.

## Results and exit status

Each planned check ends **passed**, **failed**, **skipped** (by `--skip`,
`--fail-fast`, or because a check it needs did not pass), or **unavailable**
(missing tool, other platform, scenario skipped itself).

| Exit status | Meaning |
|---|---|
| 0 | every planned check ran and passed, and no required gate is pending |
| 1 | a check failed |
| 2 | usage error, invalid base, the plan differs from `--expect-plan`, or the working tree changed while the checks ran (stale result) |
| 3 | nothing failed, but planned checks were skipped or unavailable |
| 4 | pending: planned checks passed, but required gates were deferred (live, release, CI, manual), or the change escalated to full validation and the run was not allowed to expand |

Only 0 is a passing result. An affected run never expands into full validation
by itself: when the plan escalates to full it stops before running anything
(exit 4) until you rerun with `--allow-full` or use `npm run check:full`.
Live-provider and release runs need `--live` / `--release`.

The plan hash binds the plan to its inputs: the comparison base, `HEAD`, and
the content of every changed file. Editing a file after a preview makes
`--expect-plan <hash>` refuse the run (exit 2) even when the selected checks
would be the same.

The summary and the JSON result (`--json`, `--json-out`) also list the deferred
qualification items and uncovered files; `complete` is true only when every
planned check passed, nothing is deferred, and nothing is uncovered.

**Results are evidence for one snapshot.** The checks read the working tree as
they go, not a copy. When the checks finish, the plan is resolved again; if the
tree no longer gives the plan that was started (a file was edited, added, or
removed, or `HEAD` moved), the run ends as **stale** with exit status 2 and is
not a pass, whatever the individual checks reported. Do not edit the checkout
a run is reading; work in another worktree, or commit and run the checks in a
separate worktree. The JSON result records the snapshot (`HEAD`, merge base,
the digest of the changed files), the wall time, and the settings used.

**Concurrency.** Light checks run concurrently (`--jobs`, default 2). Cargo and
the app build run one at a time and never overlap each other, the light checks,
or a scenario. End-to-end scenarios then run up to `--e2e-jobs` at once
(default 1; `EXPOSITION_E2E_JOBS` sets the default). Each scenario is its own
process and app instance with its own data directory, WebView2 profile,
debugging port, and credential namespace, and its page reports focus whichever
window is in front, so scenarios do not see each other. Two kinds of scenario
are still kept apart (`SCENARIO_RESOURCES` in the manifest, kept exact by its
test): one that moves the real pointer, needs the foreground window, or drives
a native dialog runs with no other scenario; one that reads or writes the OS
clipboard never overlaps another clipboard scenario. The same scenarios run,
each with the same isolation and the same failure reporting; only their
overlap in time changes. `CARGO_BUILD_JOBS` and `RUST_TEST_THREADS` pass
through to cargo; on a memory-constrained host set them low. Interrupting a run
stops the running process trees.

## When to run full validation

- before merging a branch or worker lane into `main`, and before a milestone is
  reported as integrated,
- before release qualification (with `--release`, then the release smoke
  scenarios against the release executable),
- after toolchain, lockfile, or dependency updates (the plan escalates by
  itself),
- whenever an affected plan escalates to full.

CI runs `npm run check` on every push and pull request regardless of local
selection. Live scenarios run only with explicit authorization; when an agents,
graph-tool, or workflow-agent change lists them as deferred, record that live
qualification is still outstanding.

## Keeping the mapping honest

- New module, crate, scenario, or fixture: `scripts/impact/manifest.test.mjs`
  fails until every tracked file maps to an area and every scenario is named by
  an area (or listed as live/manual). Add the mapping in
  `scripts/impact/manifest.mjs`; imports and crate dependencies need no
  mapping.
- New `npm run check` gate: the manifest test fails until `GATES` in
  `scripts/impact/plan.mjs` mirrors it.
- A failure found by a full run, CI, or a reviewer that the affected plan for
  the same change did not select is a mapping miss. Reproduce the plan with
  `npm run check:plan -- --base <commit before the change> --json`, find the
  missing edge (a scenario missing from an area, a file read the analysis
  cannot see, a level that is too low), fix the manifest or the analysis, and
  add the case to `scripts/impact/plan.test.mjs` or `manifest.test.mjs`.

## Reporting

Report the command, the plan hash, and the summary: which checks passed,
failed, were skipped or unavailable, which qualification items were deferred,
and which changed files no check exercised. "Passed" covers only the checks
listed; everything deferred or uncovered remains unverified.

## Known limitations

- Import analysis sees static relative imports and literal dynamic imports;
  a computed `import()` path would be invisible (there are none in `src/`).
  File reads are found from literal paths and `join()` segment lists; a path
  assembled through variables is invisible unless the manifest maps it.
- End-to-end selection follows areas. A change in a lower crate runs its
  dependents' Rust tests, its own area's scenarios, and the `dependents`
  scenarios of each area built on it: a representative set, not every scenario
  of those areas. The full set runs at the broad level's invariants or in full
  validation.
- Interrupting a run stops it without a summary or JSON result.
- Path-literal detection is a heuristic that errs toward selecting more. The
  selector's own tests use real paths as sample data, so changing those files
  also runs those tests (about a second).
- Rust selection is per crate. A `#[cfg(test)]` module inside a source file is
  treated as product code. `cargo -p` selections unify features differently
  from `--workspace`; the full gate in CI covers the workspace build.
- The plan hash covers the checks (commands, order, environment, deferred
  items, notes such as the e2e executable) and the content of changed files,
  not the content of unchanged files the analysis read.
- End-to-end scenarios are verified on Windows only.
- `agents-composer-input` and `qol-files` perform real operating-system drags
  (`scripts/e2e/os-drag.mjs`): they move the pointer and raise the app window,
  so the desktop must not be used while they run. A drag helper that has not
  finished 60 s after its hold is killed and the step fails with "os-drag
  timed out"; the app window may then still be topmost and the left button
  held until the scenario's app exits or the button is pressed again.
