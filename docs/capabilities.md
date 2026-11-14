# Capabilities and limitations

This page states what the current build of Exposition actually does, per
format and per subsystem, and what it does not do. Every "Yes" is backed by an
automated test that drives the real application (`npm run e2e`) or the real
toolchain (`cargo test`); nothing here describes planned work.

Status: local alpha. Windows 11 (x64) is the verified platform. macOS and
Linux builds are structurally supported but **unverified**.

## Document formats

Each format is judged separately for source editing, building and export,
navigation between source and pages, and direct editing on the final pages.

| | Typst | Markdown | LaTeX (pdfLaTeX) | LaTeX (LuaLaTeX) |
|---|---|---|---|---|
| Source editing (Monaco, shared undo) | Yes | Yes | Yes | Yes |
| Build from unsaved buffers | Yes, in-process (Typst 0.15.1) | Yes, through the print profile `exposition-markdown-print/1` (laid out by Typst) | Yes (TeX Live; requires a trusted folder) | Yes (TeX Live; requires a trusted folder) |
| Diagnostics linked to source | Yes | Yes (Markdown ranges, incl. unsupported-math markers) | Yes (line level, from the log) | Yes (line level, from the log) |
| Last good pages kept when a build fails | Yes | Yes | Yes | Yes |
| PDF export from the same layout | Yes | Yes | Yes (the build's own PDF) | Yes (the build's own PDF) |
| Export never replaces a source, an open document, a file the build read, or a non-PDF file | Yes | Yes | Yes | Yes |
| Source ↔ page navigation | Character level | Character level | Character level for mapped text; line level (SyncTeX) otherwise | Same as pdfLaTeX |
| Direct editing on the page: prose | Yes | Yes | Yes (mapped text) | Yes (mapped text) |
| Direct editing on the page: an equation token | Yes | Yes (declared LaTeX-math subset) | Yes | Yes |
| Reflow, shared undo with the source editor, Split view | Yes | Yes | Yes | Yes |
| Typical edit-to-page time (debug build, Ryzen 7 9800X3D) | ~2–15 ms | ~16–26 ms | ~0.3–0.45 s | ~0.8–1 s |

### What cannot be edited on the page

Text that the compiler or a template generates (numbering, references,
citations, bibliography entries, list markers, footnote marks, hyphens
inserted by line breaking), text produced by code or macros (Typst code
expressions, `#let` and function bodies, loops, `#set`, imported modules and
`#show` rule replacements; LaTeX user macros), raw/verbatim text, and content
from packages is shown with a "not editable here — edit in source" message.
Content written in place stays editable, including Typst content blocks
passed to a function (captions, footnotes) and `#if` bodies.
Edits that would change document structure (for example deleting a delimiter
of `\left`/`\right`, or typing a character that would open markup) are refused
rather than guessed.

LaTeX character mapping is inferred (SyncTeX line windows, a model of the
LaTeX source, and alignment with the PDF's glyphs) and checked glyph by glyph;
text it cannot map is not editable on the page. Exotic catcode changes and
packages that rewrite text (e.g. `siunitx`, `cleveref` names, TikZ) stay
source-only.

When you type a character that is special in LaTeX, the page editor inserts
its escaped form (`\&`, `\textless{}`, `\textasciitilde{}`, ...). pdfLaTeX's
default font encoding has no straight double quote, so a typed `"` becomes
an opening or closing quote (` `` ` / `''`) in both engines. pdfLaTeX reports
non-Latin text and combining marks as LaTeX errors, listed in Problems as
when you type them in source; LuaLaTeX reports characters missing from the
font as one warning per font.

Right-to-left text can be selected, copied, and navigated on the page; in
lines that mix directions the caret is approximate exactly at a direction
boundary. Content that a Typst document includes twice is not detected as
duplicated and stays editable in both places.

If an edit arrives from elsewhere (the source editor or an agent) while you
type on the page, your keystrokes apply at the correspondingly moved
position; if the page cannot place them, typing pauses with a message naming
the text that was not applied, and nothing is applied at a guessed position.

If a TeX engine is missing or its configured path is wrong, Problems and the
page view say so and link to Settings → Tool locations; the status bar names
the missing engine. The LaTeX engine for each main file is chosen in
Settings › Project or with the palette commands "LaTeX: Build This File with
LuaLaTeX" / "...with pdfLaTeX".

Builds, their pages, and their exports are available only while the folder
that built them is open. Only `.typ`, `.md`, and `.tex` files are built. LaTeX
page images are capped at about 32 million pixels; larger pages are shown at
a lower resolution.

Markdown mathematics uses a declared LaTeX-math subset (see
[markdown-profile.md](markdown-profile.md)); unsupported commands render as a
visible marker and a diagnostic, never silently.

## Project graph

Every open project has a typed project graph in `.exposition/graph.sqlite3`.
It is kept in step with the project's files and open buffers whether or not
a graph view is open, and the Graph, Research, and Agents pages, the Editor's
Dependencies view, and agent tasks all read and change it through one
validated service. It records files, folders, collections, the research
space, mathematical statements, definitions, assumptions, questions, proofs,
Lean declarations, bibliography entries, experiments, findings,
investigations, and workflow drafts (23 kinds), joined by 12 typed
relationships with endpoint rules.

**What is indexed.** Manuscripts (`.tex`, `.typ`, `.md`), notes, BibTeX,
scripts (Python, R, Julia, notebooks), Lean files, datasets, images and
PDFs, and configuration files become resources with one stable identity per
file. Version-control, tool, environment, and cache folders (`.git`,
`.lake`, `node_modules`, `target`, `.venv`, `__pycache__`, `.cache`,
`.vscode`, `.claude`, …), generated build files (`.aux`, `.log`,
`.synctex.gz`, `.bbl`, `.olean`, `.pyc`, …), agent instruction files
(`CLAUDE.md`, `AGENTS.md`), and Exposition's own metadata are excluded: run
records are never entities, and run outputs become resources only when you
keep or attach them. Links and junctions are never followed. A scan covers
at most 32 folder levels, 200,000 folder entries, and 20,000 files; files
beyond a bound are unknown, not removed. Files over 8 MB are listed but not
interpreted.

**What is extracted.** Extraction reads source text literally: it never
expands macros or runs code, and what it cannot interpret is reported as
partial coverage rather than guessed.

| Format | Recognised | Not recognised |
|---|---|---|
| LaTeX | Theorem-like environments with standard names, or defined by a literal `\newtheorem`/`\declaretheorem` earlier in the same file; `\label`; `\ref`-family references and `\cite`-family citations; `\input`, `\include`, `\subfile`, `\import`; bibliography files; `\includegraphics`; a proof that names its statement, or a proof immediately after a statement | Macros (a definition that could hide tracked constructs is reported); `\newtheorem` in another file or a package; non-literal names; remarks and examples |
| Typst | Theorem-like calls with the default names, and `ctheorems` definitions (`thmbox`, `thmplain`, `thmproof`) in the same file; labels, `@` references, `cite`; imported and included files; bibliography; images | Other theorem packages; anything that needs evaluation; proof links |
| Markdown | Pandoc fenced divs with the default class names (`::: {.theorem #id}`); links, images, `[@key]` citations | Footnotes; math content; headings are context only |
| Lean | `theorem`, `lemma`, `def`, `abbrev`, `structure`, `inductive`, `class`, named `instance`, `axiom`, `opaque`, qualified by namespace; `import` | Declarations generated by macros, elaborators, or notation; anonymous instances, `example`; use sites |
| BibTeX | Entries by key, with title, year, and DOI when literal | `@string` expansion |
| Python, R, Julia scripts | Literal imports of project files (`import`/`from`, `source()`, `include()`) | Data files a script reads at run time; non-literal arguments |
| Notebooks, data, images, configuration | The file as a resource only | Contents |

Each file's coverage is shown as complete, partial (with what was not
interpreted), unsupported, pending, or failed, together with the references
it could not resolve. A label or citation that is missing or ambiguous is
counted and explained, never attached to a guessed target. `.ltx` files are
not indexed.

**References are not dependencies.** A `\ref`, `@label`, or citation is
recorded as a reference: a mention, never a mathematical dependence.
Extraction never creates "depends on" or "assumes" relationships; only you,
an agent (as a suggestion), or a project preset do. File inclusion and asset
use are separate kinds again. Relationships found in the sources are shown
as observed and change only when the source changes; ones you add are
accepted; ones an agent adds stay suggestions until you accept or reject
them.

**Identities.** A renamed or moved file keeps its identity when the old and
new paths are seen in the same reconciliation and match uniquely by file
identity or identical content; a copy is a new item. A declaration keeps its
identity across edits when it matches uniquely by name, then by identical
text, then by surrounding context; a name declared twice is shown as
ambiguous instead of being merged. Items whose file or declaration
disappears are marked "Removed from source" and are revived if it returns
unchanged; reconciliation never deletes an item, changes a record you or a
service created, or reassigns an identity on an uncertain match. A move
whose old and new paths are seen in different reconciliations gets a new
identity.

**Unsaved edits.** While a buffer has unsaved changes, answers about it
(the Dependencies view at the caret) come from its text and are labelled
"Unsaved buffer (revision N)"; nothing from an unsaved buffer is written to
the graph, and saving or undoing back to the saved text removes the overlay.
Changed files read as "Reconciliation pending" until a reconciliation has
processed them (one starts once changes pause for 250 ms, and at most 2
seconds after the first change), and the Graph page says so.

**Impact and freshness.** "What needs review if this changes?" follows
relationships by meaning, up to 4 steps by default (at most 8): dependents
and proofs need review, citing items need citation review, files that
include or use the item are stale. Rejected relationships are not followed,
suggested ones are labelled, cycles are reported, and files whose coverage
is unknown are listed, because a missing result there is not evidence of
absence. Evidence is current only when every input it records still has the
same content; it is stale when an input changed or was deleted, and unknown
when that cannot be established. Unknown is never shown as current, and
freshness reflects the last reconciled content of each file.

**Mathematics and Lean.** The Graph page's Mathematics view lists
statements with their informal proof, formal association, verifier verdict,
and freshness. The verdict comes only from Lean's audit evidence. The Lean
audit reports no declaration dependencies, so formal evidence is never
current against imported modules and formal dependency edges are shown as
unavailable. Evidence recorded on another computer (for example in a graph
copied with the project) is shown as an imported historical claim, never as
current verification on this computer. Associating a Lean declaration with
a statement records your reviewed correspondence, not a machine proof of
equivalence.

**Storage, backups, and recovery.** The graph database is treated as
untrusted input: it is opened only if it is a regular file of at most 1 GB
inside the project's own `.exposition` folder, with SQLite's defensive mode
and with triggers and views disabled; every schema object and column must be
exactly the expected ones, and every row is checked before use. A database
from a newer version, or one that is damaged or fails these checks, is left
unchanged and the graph is shown as unavailable: editing files keeps
working, graph-dependent actions (including agent file actions) pause, and
you choose to restore one of the newest backups, start a new graph (the old
file is kept aside, never deleted), or retry. Nothing replaces a damaged
graph automatically. Consistent backups are kept in
`.exposition/graph-backups/` (the newest 8): one after the first
reconciliation of an open when the newest is more than a day old, and one
before a schema upgrade. Recovery performs only an option the app offered,
and never through a link. A folder that
cannot hold the graph (for example a read-only one, or a graph on read-only
storage) gets a temporary graph: it is indexed and readable, but nothing
curated persists and the Graph page offers no changes or saved layouts.

**Changes and layout.** Every change to the graph is validated by the
application before it is applied, all or nothing; the webview cannot run
SQL or create service-owned records such as computational or Lean evidence.
Canvas positions, filters, and hidden items are saved per section with the
project and never change relationships. On the canvas, connections are
straight lines that can pass behind other nodes (no edge routing); a
table view offers the same items and relationships for keyboard use.

## Projects and workflows

File › New Project… creates a project from one of six presets: Mathematical
paper, Computational study, Lean investigation, Blank research project,
Paper with computation, and Paper with Lean. A manuscript is Typst, LaTeX
(pdfLaTeX or LuaLaTeX), or Markdown; a computational study declares Python,
R, or Julia; a Lean project gets `lean-toolchain` (an installed toolchain, or
`leanprover/lean4:v4.35.0`), `lakefile.toml`, and a Lean file. The preview
lists every file and folder, the starting project structure (for example a
main result that depends on a key definition and assumes standing
assumptions, a research question, a baseline experiment), and the tools the
preset needs with whether each is installed. Tools are declared, never
installed or run. "Describe an objective" creates an empty project with a
research space and then starts an ordinary agent task (whole project,
commands off unless you choose otherwise) that proposes the structure; its
changes are reviewed like any agent's.

A project is created only in a new folder or an empty one that Exposition
would open as a project, never in a folder that already holds a project, the
application's data folder, or a too-broad location. Creating it inside a
trusted folder requires an explicit acknowledgement, because native tools may
run the new files there. Existing files are never replaced: if any file the
project needs already exists, nothing is created. Creating a project grants
nothing: no trust, no code execution, no package installation, and nothing
is sent to a model (the objective path starts its task only after you
choose "Create and Start Agent").

Every blueprint (from a preset, an agent, or an import) is validated before
anything is written. It has no fields for trust, running code, startup
hooks, or permissions (unknown fields are refused); it is limited to 200
files of at most 1 MB each (8 MB in total); its paths are relative, confined
to the project, and may not create version-control or tool folders, files
that tools run on startup (such as `latexmkrc`, `texmf.cnf`,
`sitecustomize.py`, `.Rprofile`, `lakefile.lean`, `CLAUDE.md`), executable
files, or anything under `.exposition/` except the project's own research
space. Relationships from a built-in preset are accepted; those from any
other blueprint are suggestions.

Creation is journaled. If Exposition stops part-way, opening the project
shows "Project setup did not finish" with each file marked Created, Missing,
or Different content; you choose Finish Setup (creates only the missing
files and applies the rest) or Keep as Is. An interrupted creation is never
replayed automatically or reported complete.

Workflow drafts describe a computation as typed steps: inputs, steps from
twelve registered operations (Python, R, and Julia batch runs in the
restricted container; native Python, R, and Julia; Lean audit, evidence
association, agent review, keeping a result, requesting a promotion,
running another saved workflow), connections, outcomes, and a ceiling on
what the workflow may run. Code is inline in the step's language or a
script file of that language (`.py`, `.R`/`.r`, `.jl`); a script of another
language is refused when the workflow is validated, also when it reaches
the step through a composite. A batch step may also take a one-line text
`value` (at most 1000 bytes) that its code receives as
`EXPOSITION_PARAM_VALUE`. Two presets exist, Numerical experiment (Python; without a script it
starts with an editable inline code template) and Formal audit (Lean).
Drafts are edited in a form or on a canvas, validated by the application
against a strict contract (typed connections, no cycles, every step
reaching an outcome, effects within the ceiling), and saved as immutable
revisions in the project graph; imports are treated as untrusted data.
Saving, importing, or an agent's proposal never runs anything. A draft is
reported as runnable only when it is well formed and every step's runner is
available in the open project right now (for example, a native step needs
a trusted folder, a restricted step a configured container image that is
present locally, an agent review step a configured model provider); a
stored or imported claim of runnability is ignored.

### Parallel steps, composite workflows, conditions, and repeats

These belong to workflow schema `exposition.workflow/2`; the editor
switches a draft to it when one of them is used. A draft that uses none of
them stays `exposition.workflow/1`, byte for byte, so its content hash does
not change, and a `/1` definition that names one is refused.

- **Parallel steps.** "Steps that may run at the same time" is 1 to 4
  (default 1: one step at a time, as before). Only steps with no data,
  prerequisite, or condition path between them run together, and
  application-wide limits apply on top (see Workflow runs).
- **Composite steps** ("Run a saved workflow"). A step can run another
  saved workflow of the project at a pinned revision. Its inputs are the
  child's inputs and its outputs the child's outcomes; its containment is
  the least contained of the child's steps and its effects are theirs.
  Editing the child makes a new revision and never moves the pin: the
  parent offers **Use latest revision (N)**, an ordinary edit of the parent
  that you review and save. Validation refuses a workflow that includes
  itself (directly or through another composite), nesting deeper than 3
  levels, more than 100 steps once composites are expanded, a child
  revision that does not exist or is not well formed, and a child whose
  ceiling allows more than the parent's. A composite step cannot carry a
  condition, a repeat, parameters, or code. The plan review shows every
  step of the child in full (code and prompts) under the composite's name,
  and each child step runs through its own runner.
- **Conditions.** A step can run only when a fixed predicate on an earlier
  step's validated output holds; there is no expression language. The
  predicates: the Lean audit's verdict is one of a chosen set (Checked,
  Checked with nonstandard axioms, Incomplete, Relies on compiled code,
  Partially checked, Unverified, Errors; an overall verdict such as
  "Errors (2)" counts as its class); a value at a JSON pointer (RFC 6901)
  in a JSON dataset of at most 1 MiB compared with a number (=, ≠, <, ≤, >,
  ≥) or with text or true/false (equal or not equal); or an optional output
  was produced. Each can be negated. The step waits for the step whose
  output its condition reads. When the predicate does not hold the step
  reads **Condition not met** with what was observed, and the steps that
  need its outputs are **Skipped**, each with its reason; the run can still
  succeed, and the outcomes they would have produced read Not produced with
  the reason. A source that cannot be read (not JSON, larger than 1 MiB, no
  value at the pointer, a value of another type, a file changed after it
  was validated) fails the step: it is never taken as "false". A condition
  cannot read a step that runs once per value.
- **Repeats.** *For each*: the step runs once for each of 1 to 20 values of
  one of its operation's declared parameters (instances may run at the
  same time when the workflow allows parallel steps); its outputs are
  collected in value order and can feed only an input that accepts several
  sources, never an outcome directly. *Until*: the step runs again, one
  iteration after another, at most 1 to 10 times, until a predicate (as
  above) on one of its own outputs holds; if it never does, the step fails
  or uses the last iteration's outputs, as the definition says. Every
  instance is recorded and counts as one step against the run's step
  budget. A composite step runs once. Batch code (Python, R, or Julia)
  sees which instance it is: `EXPOSITION_ITERATION` is the 0-based
  instance number, each parameter set on the step (the for-each value
  included) arrives as `EXPOSITION_PARAM_<NAME>` (`value` →
  `EXPOSITION_PARAM_VALUE`, `timeoutSeconds` →
  `EXPOSITION_PARAM_TIMEOUT_SECONDS`), and every instance gets its own
  `EXPOSITION_SEED`. Batch parameter values must be one line of text; one
  that looks like a secret is refused before the batch runs.

Agents' workflow proposals stay `exposition.workflow/1`: an agent cannot
propose parallel steps, composites, conditions, or repeats, and it cannot
revise a workflow that uses any of them (the proposal is refused and
nothing is saved, because saving it would remove them; an agent can still
propose a new workflow).

### Workflow runs

A run starts only from **Review and run…** in the workflow editor (or
**Reproduce…** in a run, reviewed the same way), for a saved, well-formed
revision (never from unsaved edits), or from a trigger under a standing
plan you approved (see Triggers). The review shows
the complete plan: every input file with the SHA-256 of the content the run
will read and whether that version is pinned, every step in the order it
will run with its complete code (inline, the full text of the script file
it runs, or the full text of the Lean file it audits, since elaborating
Lean can run code) or model instructions, parameters, runner, containment,
effects, and limits, the run's budgets, the restricted image (by name and
by its exact image identity) and folder trust it relies on, how many steps
may run at the same time and the application-wide limits, each composite's
steps grouped under it, each condition and repeat in plain words, notices
(native privileges, material sent to the model provider, proposals you
review later), and anything that blocks it. **Start run** approves exactly
that plan. If anything in it changed before the run starts (the workflow
was saved again, an input file changed, folder trust or the restricted
image changed, including the same image name now naming a different image
after a re-tag or pull, a runner became available or unavailable, or
another project is open), the start is refused and you review the new
plan. A plan must be started within 30 minutes.

Steps run in dependency order. With the default of one step at a time,
each step waits for the previous one. A workflow that allows more (up to
4) starts a step as soon as every step it reads from, runs after, or whose output its
condition reads has finished, within application-wide limits shared by
every run: at most 3 active runs (one per workflow), 4 steps in flight, of
which at most 2 in the restricted container, 2 native (a Lean audit counts
here too), 2 agent reviews, and 1 Lean audit; steps that only use
Exposition's own services fall under the 4 alone. A step that finds a
limit reached waits; it is never refused for it. There are no retries.
Before each step Exposition checks again that the project is still open,
the run was not canceled, its budgets remain, every input file and earlier
output the step reads still has the digest recorded for it, a native
step's folder is still trusted, and the step's runner is still available
with the approved image identity, and the container is started from that
identity; a restricted step never falls back to native execution. A
failed step stops the run: nothing new starts, steps already running finish
and are recorded, and the remaining steps are skipped. Every output a step
returns is checked before a later step can use it: its declared type, its
recomputed SHA-256, that it came from this step's own service operation,
and that formal evidence and retained results come only from the Lean
audit and the retention service (a file that merely looks like evidence
stays a file). **Cancel run** stops every running step through its service
(a batch's container is removed, a review's task canceled) and skips the
rest; closing or switching the project cancels its active runs, and
revoking trust cancels every active run of that folder that has a native
step, including its restricted steps.

The run's budgets come from the plan: at most as many step dispatches as the
workflow has steps after composites are expanded (each for-each value and
each until iteration counts as one), the sum of the steps' time limits (at
most 4 hours), the sum of their output limits (at most 1 GiB), and 25 model
requests per agent review step; requests a review made count even when it
fails or is canceled. Before a step starts, its output limit and, for a
review, its 25 model requests are reserved from what the run has left, so
steps running together cannot jointly exceed the budgets; a step whose
reservation does not fit waits until running steps release theirs. The
time budget is wall-clock time since the run started (waiting included),
and each step's time limit is capped by what remains. Each attempt is
recorded before its service is called. If Exposition stops during a run,
the next start marks the run **Interrupted** (every step or instance that
was running has an unknown outcome) and never resumes or repeats anything;
you mark it as reviewed and start a new run if needed. Run records are kept
in the application data folder on this computer, not in the project. Each
run records why it started: by you from a reviewed plan, by a trigger
(with the inputs that changed), or as a reproduction of an earlier run.

| Step | Where it runs | What it produces |
|---|---|---|
| Research Python batch (restricted) | A Docker container (below) from the configured container image | The files it writes for its declared outputs, as research run artifacts |
| Research R batch / Research Julia batch (restricted) | The same Docker container, from the configured R image or Julia image (`Rscript --vanilla`, `julia --startup-file=no --history-file=no`); refused when that image is not configured or not present locally, never run natively or in another language's image instead | Same as the Python batch |
| Trusted native Python / R / Julia | A new process of the discovered Python, `Rscript`, or `julia` with your account's privileges, in a trusted folder; not a sandbox | Same as the restricted batch |
| Lean audit | The Lean service with the project's toolchain, natively, in a trusted folder | The audit's own per-declaration verdicts, statements, and axioms for exactly the audited text |
| Associate audited declarations | Exposition | A proposal you accept or reject per declaration; only accepted declarations get Lean evidence recorded |
| Read-only agent review | A read-only agent task with commands off, scoped to the declared material, which it sends to the model provider; a tool that would write or run is refused and the review continues; up to 2 reviews run at once, each its own task, sharing the run's model-request budget | A review report (prose, never evidence); a later step can take several reports |
| Retain artifact | Research | A kept result with computational evidence naming the mode it actually ran in |
| Request reviewed promotion | Research | A prepared insertion at the end of the manuscript that you review and approve in the run view |
| Run a saved workflow (composite) | Each of the child's steps through its own runner, as listed here | The child's outcomes, as this step's outputs |

Each restricted runner's image is bound by its exact identity in the plan
you approve (the review lists the R and Julia runners' images), checked
again before each restricted step, and the container is started from that
identity.

**Restricted batches** (Python, R, Julia). Only the declared input files and the code
are copied, each verified against its SHA-256, into a working copy that the
container sees read-only; the project folder itself is never mounted, so an
undeclared file is simply absent and an import of it fails visibly.
Declared inputs that are links, junctions, aliases, or project metadata
(the graph, run records, version-control or tool folders, agent
instruction files) are refused. The container has no network, no host
environment or credentials, a non-root user, a read-only file system, the
configured memory, CPU, and process limits, and the step's time limit. The
code writes into the folder named by `EXPOSITION_OUTPUT_DIR`; output `p` is
the file `p.<ext>`. Only regular files directly in that folder with a safe
name and an exported type (PNG, JPEG, GIF, SVG, PDF, CSV, TSV, JSON, TXT,
HTML) whose content matches the type are kept, at most 32 files of 64 MiB
each; links, folders, hard links, and other entries are refused and nothing
they point to is copied. A batch that writes far more than its output
budget is stopped. Kept files become artifacts of a research run that
records the container image, user, and limits it ran with. SVG and HTML
are stored as text and never rendered as active content. Batches
(restricted and native, every language) also receive `EXPOSITION_SEED`
(see Reproducibility) and, when set, their parameters and instance number
(see Repeats above).

**Limitations.** Each image must provide its interpreter (`python3`,
`Rscript`, or `julia` on its standard path) and every package the code
needs: Exposition never pulls images or installs packages, and the
container has no network. Restricted R and Julia batches run only in their
own configured image (Settings › Execution › R image / Julia image); there
is no default R or Julia image. The trusted native Python, R, and Julia
steps and the Lean audit run with your account's privileges and are not
contained. The Lean audit step has a
10-minute limit, the most the Lean service allows for one check; if the
Lean file is open with unsaved changes that differ from the reviewed file,
the step fails rather than audit other text. A promotion is always
prepared at the end of the manuscript; a discarded one reads Rejected, and
one that can no longer be approved (its 30-minute window passed, or
Exposition restarted) reads Expired. A step that waits for a free slot or
for its budget reservation also holds back the steps after it in the same
run, and the wait counts against the run's time budget. JSON conditions
compare numbers exactly, as decimals; a number in the JSON file that has a
fraction or lies outside the 64-bit integer range is first read as the
nearest double-precision value.

### Triggers

A trigger starts runs of one workflow by itself, under a *standing plan*
you approve. Triggers are off by default, and only you can turn one on,
from the **Triggers** section of the workflow editor: agents have no tool
for them (naming a trigger command is refused as an unknown tool), and
imported workflows, bundles, project files, and generated drafts never
carry one. A workflow has at most one trigger; when it already has one, the
dialog says so and **Replace trigger** replaces its kind and settings (its
history is kept).

- **When watched inputs change:** the saved content of chosen input files
  (at most 16). Code inputs and Lean sources cannot be watched (changed
  code, and Lean, whose elaboration can run code, always need a new
  review), nor inputs the workflow pins to one version. Changes by any
  program count, not only saves in the editor.
- **On a fixed interval:** every 15 minutes to one week while the project
  is open, whether or not anything changed, optionally with inputs whose
  saved content may differ between runs (by default none may).

**The standing plan** is the complete run plan of the latest saved
revision, reviewed like any plan (every step, its code, runner,
containment, effects, and limits, the budgets, the restricted image's
identity, folder trust when native steps are present), plus exactly what
may change without asking (the watched inputs' saved content) and what
suspends the trigger. You choose a quiet period (2 seconds to 1 hour; a
trigger on watched inputs fires only after no further change for that
long, and each change starts it again), at most 1–12
runs an hour and 1–100 a day, and, when the plan has agent review steps,
model requests per day (at least one run's budget, at most 1000; each run
counts its full budget). A plan with native steps can be enabled only if
you explicitly allow them to run without asking, and every review, a
re-review included, starts with that permission off; they then run with
your account's privileges on every fire. The suggested settings are 10 seconds,
4 an hour, 24 a day, native steps not allowed, and four runs' model
requests.

**Firing.** A trigger listens only while its project is open and after the
project has finished opening and indexing: it never fires because a project
was opened, and changes made while the project was closed are never caught
up (the content found at open becomes the new starting point). When the
quiet period ends, Exposition plans the run again. If the plan matches the
standing plan in everything except the watched content, the run starts
through the normal start check and records its cause: the trigger and each
changed input with its SHA-256 before and after. If a run of the workflow
is active (or three runs are), one follow-up is queued and starts after it
with whatever content is saved then; if the watched content is back to what
the last triggered run read, the follow-up is dropped. While it waits,
Exposition checks again after 1 second, then less and less often (up to
once a minute), and at once when a run ends; these checks plan without
keeping a plan, so they never displace a plan you are reviewing. When the
hourly, daily, or model-request limit is reached, the run waits until the
limit allows it.

**Suspension.** Any other difference suspends the trigger until you review
and enable it again: a new revision, a change to a code input, a Lean
source, or another unwatched input, folder trust (for a plan with native
steps), a restricted image name now naming a different image, a runner
becoming unavailable or available (for example Docker not running when it
fires), or any step, budget, or limit. The card and a notification
("Workflow trigger: suspended: …") give the reason. Revoking trust for the
folder suspends all of its triggers, as every standing plan says. Closing
or switching the project stops listening and drops pending and queued
fires; the trigger stays on and listens again the next time the project is
open and indexed. Triggers are stored in the application data folder, never
in the project, a workflow definition, or a bundle; they stay on across
restarts, and a restarted Exposition does not catch up on missed intervals
or changes. If that file cannot be read, it is never overwritten: triggers
cannot be enabled or turned off until it can, and the cards say why; an
unparsable file is set aside and Exposition starts with no triggers.

### Reproducibility

Every step attempt records the environment it ran in, shown under
**Environment** in the run view: the runner and its containment, the
operating system and architecture, the Exposition version, and, where they
apply, the restricted image's name and exact identity, the interpreter and
its version, the package list with a SHA-256 digest of it, the Lean
toolchain with the digest of `lake-manifest.json` and the Lake dependencies
it pins, the model provider and the model the provider reported serving,
and the seed given to the step. Nothing extra runs natively to find this
out:

- A restricted Python batch lists the Python distributions installed in
  its image the first time a batch uses that image identity: a separate
  container started from the same identity, with the same restrictions (no
  network, no project files, no credentials) and a 30-second limit, as part
  of the approved batch. The list is cached per image identity in the
  application data folder; if it cannot be made, the attempt notes why and
  the batch runs anyway.
- A restricted R or Julia batch records its image's name and identity
  only: no interpreter version and no package list (the attempt says so);
  the identity names the image's exact content.
- A native Python step records what research environment discovery
  already reports: the interpreter's version and the nine packages
  discovery checks (jupyter_client, ipykernel, pyzmq, NumPy, pandas,
  Matplotlib, SciPy, SymPy, Polars), not every installed package. A native
  R or Julia step records the discovered runtime's version (and IRkernel's
  for R).
- The Lean audit records the exact toolchain it used and reads
  `lean-toolchain` and `lake-manifest.json`.
- An agent review records the configured model and, when the provider
  reports it, the model it actually served.
- Each batch (Python, R, Julia) receives `EXPOSITION_SEED`, a number
  derived from the run, the step, and the instance, and records it; a
  reproduction receives the original run's seeds. Lean, review, and other
  steps get no seed.

**Run manifest.** **Export manifest…** in the run view writes the run's
manifest (`exposition.workflowRunManifest/2`, JSON) to a file you choose in
the save dialog: the revision, definition and approval hashes, every input
and output with its SHA-256, the budgets and usage, the cause, the images
the restricted steps were approved with, and each step's attempts (and each
instance of a repeated step) with their environment. It contains no logs,
no code or review text (only the SHA-256 of the exact text), and no local
paths: the project, the application data folder, and your home folder read
`<project>`, `<app data>`, and `~`, other absolute paths `<path>`, and
secrets are redacted. Keys are sorted, so the same run always gives the
same bytes. The destination follows the bundle rules: a `.json` file
outside the project's `.exposition` folder and the application data
folder, and an existing file is replaced only if it is an earlier run
manifest.

**Reproduce.** **Reproduce…** on a run that has ended builds a new plan of
the same revision with every input pinned to the SHA-256 that run read. A
changed or missing input blocks it until you restore that version, and a
revision whose content no longer matches cannot be reproduced. Before you
approve it, the review shows the differences from the recorded
environment that can be seen without running anything (image identity,
platform, interpreter and toolchain versions, Lake manifest, package
lists, configured model, Exposition version), compared for every instance
of a repeated step, and the seeds the batches and their instances will
receive; differences do not block the run. It starts like any plan, and the new
run records which run it reproduces and links to it.

### Workflow bundles

**Export revision N…** in the workflow editor writes the saved revision,
together with every saved workflow its composite steps run (at their
pinned revisions, recursively), as one `exposition.workflow-bundle/1` file
you choose in the save dialog (at most 8 MiB and 32 child workflows). A
bundle holds definitions only: no trust, approvals, triggers, run records,
or results. The destination must be a `.json` file outside the project's
`.exposition` folder and the application data folder, and an existing file
is replaced only if it is an earlier workflow bundle.

**Import Bundle…** on the Workflows tab reads a bundle (or a single
workflow definition) as untrusted data. It is parsed strictly: an unknown
field anywhere, including any claim of trust, approval, or a trigger,
refuses the whole file; every carried workflow must be one the root runs;
nothing may include itself or nest deeper than 3 levels. Then the children
are saved first as new imported drafts with fresh ids, and the root last,
with its references rewritten to them. Nothing runs, nothing is trusted or
approved, and no trigger is created. Limitation: every check happens
before anything is saved, but if saving fails part-way (for example a
storage error), the workflows already saved stay as drafts. A single
definition imported this way keeps its composite references as written;
they resolve against this project's own workflows.

## Research

| | Python | R | Julia |
|---|---|---|---|
| Discovery of installed environments | Yes | Yes (with IRkernel) | Yes (with IJulia) |
| Managed kernel sessions, run input / selection / file | Yes | Yes | Yes |
| Interrupt | Yes | No — IRkernel ignores interrupts on Windows; Cancel restarts the kernel | Yes (IJulia; a kernel without message-mode interrupts reports it as unsupported, and Cancel restarts it) |
| Figures and tables captured as artifacts with provenance | Yes | Yes | Yes |
| Insert and update a figure/table in a Typst, LaTeX, or Markdown document | Yes | Yes | Yes |
| Workflow batch steps: restricted container / trusted native | Yes / Yes | Yes (its own configured R image) / Yes (`Rscript`) | Yes (its own configured Julia image) / Yes (`julia`) |

The restricted R and Julia batches are covered by Docker tests that run
only on request (`EXPOSITION_DOCKER_TESTS=1 cargo test … -- --ignored`
with the local images `r-base:4.6.2` and `julia:1.13`), not by the default
suite; the native ones by tests that skip when R or Julia is not
installed.

Sessions belong to one project folder and require it to be trusted; switching
or closing the folder, or revoking trust, detaches them. Agent runs use
separate sessions. Exposition never installs runtimes or packages.

Interactive research sessions run natively with your account's privileges
("Trusted native"); the research space and per-owner sessions organize work
but do not contain it. Restricted research execution exists only as the
workflow's Python, R, and Julia batch steps (see Workflow runs), and the
Research page
shows whether it is available for the project; nothing moves between the
two modes silently. Computational evidence names the mode a run actually ran
in. Enabling Research creates one research space (`.exposition/research/<id>/`
with sources, data, and notes) and runs nothing; research works without any
manuscript. Kept results become project resources with the run's evidence and
provenance; inserting one into a Typst, LaTeX, or Markdown document is
reviewed first (exact markup, document revision, artifact digest) and is
refused if either changed before approval. If the project graph cannot be
updated after an insertion, the insertion stays (undoable) and a pending
record asks for reconciliation. Data files a script reads are pinned only
when declared (experiment inputs); the kernel does not report the files it
reads.

## Lean

Lean files are checked with the project's own toolchain through the Lean
language server. A file is reported **Checked** only when Lean fully
elaborated exactly that text with no errors, no `sorry` (including through
imports), kernel checking enabled, and only the standard axioms (`propext`,
`Classical.choice`, `Quot.sound`). Anything else gets its own status:
Incomplete (`sorry`), Checked with axioms, Relies on compiled code
(`native_decide` and other native evaluation), Partially checked (`#exit`,
or another command that ends elaboration early),
Not verified, Errors. Goals are shown as plain text; third-party infoview
widgets are not executed. Toolchains are never downloaded implicitly.

What Checked certifies: the declarations listed under the status were
re-checked by Lean's kernel in the audit's own elaboration of exactly that
text, and they use only the axioms shown. The audit compiles the text, then a
separate Lean program that runs none of the file's code reads the result as
data, re-checks every project and dependency declaration it relies on with the
kernel (as lean4checker does), lists the axioms, and prints each declaration's
type with the toolchain's notation only. A declaration added without the
kernel makes the file Not verified, as does a module that skips Lean's `Init`
(`prelude`) or declares a constant the kernel evaluates by name; a standard
axiom counts only if it is the toolchain's own.

A Lean file is a program, and it also runs in the language server, so a file
written to behave differently under the audit can make the audit check
something other than what the editor shows. Exposition notices this only when
the audit lacks a declaration the language server reports for the file (then
Not verified); otherwise, the listed declarations are what was checked. A
trusted file's code runs with your account's privileges, and code that
tampers with the Lean toolchain itself is not contained. The audit programs
target Lean 4.35 (another toolchain they do not compile on reports Not
verified), and re-checking large dependencies such as Mathlib takes
correspondingly long.

Formal evidence in the project graph is recorded only from the audit Lean
itself produced for an exact revision, one declaration at a time, with the
declaration's own verdict next to the file's. The audit does not report
declaration dependencies, so evidence is never shown as current against
imported modules; after the file changes it is historical. Associating a Lean
declaration with a statement in a paper records your reviewed correspondence;
it is not a machine proof that the two statements are equivalent.

## Agents

The first provider is Anthropic's Messages API (`claude-opus-5-5` by default).
Agents use the same services as people: they read and edit documents through
the shared document service (every change is reviewable and revertible), build
documents, run Python (in their own research sessions when you approve native
runs, or one run at a time in the container in restricted mode), and check
Lean files.

The application, not the model, enforces each task's scope, consent, permission
profile, execution mode, and budgets. Every tool declares what it reads,
changes, and runs, and one gate checks that declaration before anything
happens:

| Tool | Execution off | Ask before each | Restricted (container) | Scope and consent |
|---|---|---|---|---|
| List, read files | allowed | allowed | allowed | path in scope (links resolved); reading needs content consent |
| Edit, create files; insert an artifact | allowed (approval when reviewing edits; never in a read-only task) | same | same | path in scope; for an artifact, its file too |
| Build Typst or Markdown (in-process) | allowed | allowed | allowed | entry in scope; the build reads only the scope; refused under "file names only" |
| Build LaTeX, check Lean | refused | trusted folder + approval | refused | whole project + content consent |
| Run Python, run a command | refused | trusted folder + approval | container only, unattended | whole project + content consent |

The project graph is part of every agent task, enforced by the application:
each run starts with a scoped graph context (the project's identity and
revision, the files and research entities in the task's scope with their
versions, coverage, evidence freshness, and relations, and the operations the
task may perform; under "file names only" no labels or notes), file tools
resolve the paths they act on to graph identities, and an action whose target
the graph has not reconciled, cannot identify, or cannot reach at all is
paused with a stable code instead of falling back to plain file access. Agents
can query the graph within their scope and propose typed entities and
relations; changes are approved like edits, and the relations an agent adds
remain suggestions until you accept them. Keeping a task as an investigation
admits it into the graph; ordinary tasks stay operational records. The Agents
page and the Agent panel show the same tasks and approvals, with each task's
graph context, proposals, and changes.

Approvals show the complete code or text that will run or change, with
invisible and direction-changing characters made visible; what runs is
exactly what was shown, and an approval answered after the project changed
does nothing. A read-only task changes nothing and runs nothing. A response
cut off by the per-turn output or context limit ends as "Stopped" with the
reason, and a task that uses up its own budget ends as "Budget exceeded";
neither is reported as completed. Review reflects the
document's actual content: a change you undid in the editor shows as undone.

The API key is stored in Windows Credential Manager for this computer only
(it does not roam with a domain profile). A key saved by an earlier version
with roaming persistence is moved the first time it is read: the roaming
copy is deleted and a local copy written, and the result is checked after
Credential Manager has reloaded its stores, so no old copy comes back later
over a replaced key or after Remove. It is kept out of every child
process's environment and redacted from output, transcripts, and stored
text artifacts. Trusted native code (research kernels, and the commands,
Python, LaTeX builds, and Lean checks you approve) runs as your Windows user
and could read the stored key; Exposition cannot hide it from code you trust.

A live end-to-end run against the real provider is covered by the opt-in
scenario `npm run e2e -- live-agents` (it spends API credit): it computes with
Python, edits the paper, runs a Lean check, builds, reverts through review,
cancels a running task, handles a concurrent user edit, and requires that an
out-of-scope read be refused by the application itself. A second opt-in
scenario, `npm run e2e -- live-restricted`, runs an unattended task's Python in
the Docker sandbox and then checks that the same request is refused, never run
natively, when no image is configured; it uses no development-only command, so
it also runs against an installed release. The default automated
suite uses a scripted development provider that release builds do not contain,
and development builds use a separate development credential entry unless told
otherwise. A third, `npm run e2e -- live-graph`, has the model read the project graph, propose a mathematical dependency (kept as a suggestion until a person accepts it), and save a workflow draft, and checks that a task with "File names only" consent never sends or receives theorem labels or file text. A fourth, `npm run e2e -- live-workflow`, has the model propose a restricted Python workflow (saving it runs nothing), then plans and starts it as the user and checks that it ran in the Docker sandbox and produced its validated output; agents have no tool that starts a run.

## Execution and security

- Opening a project never runs its code. Running native tools (TeX, kernels,
  Lean, commands) requires trusting the folder; system, profile, and other
  overly broad folders cannot be trusted or opened as projects, also when
  reached through another name (for example the administrative share
  `\\localhost\C$`), and neither can Exposition's own data folder.
- A configured tool location is re-checked every time it is used: a location
  that has become part of an open project is never run.
- Trusting or revoking a folder applies at once in every view. Revoking stops
  the folder's Lean server, research kernels, LaTeX builds, and other trusted
  jobs before it completes.
- Child processes get an environment built from scratch; credential-like
  variables are refused. Logs and events are bounded and redacted.
- Trusted native code runs with your account's privileges. Exposition kills
  the process trees it starts, but processes created through system services
  (for example WMI) can outlive a job.
- Unattended agent execution uses the restricted backend: Docker with no
  network, a read-only project mount, a non-root user, and no host
  environment. It must be configured with a locally present image (which
  must provide `/bin/sh`); Exposition never pulls images and never falls back
  to native execution. An agent's container stops when Exposition exits or
  crashes and when its time limit passes; leftovers are removed at the next
  start when an image is configured, or else before the next restricted run.
- Workflow restricted Python, R, and Julia steps use the same Docker
  backend and settings (R and Julia with their own configured images),
  with a working copy of only their declared inputs instead of the project
  folder, and likewise never fall back to native execution or to another
  image. Workflow native Python, R, and Julia steps and Lean audit steps
  run with your account's privileges and need a trusted folder.
- Workflow triggers start runs without asking only under a standing plan
  you approved, and only you can enable one; agents, imports, and project
  files cannot. Trigger records live in the application data folder, never
  in the project. A native step runs from a trigger only if you allowed it
  explicitly for that trigger, and a change to code or a Lean source never
  starts a triggered run: it suspends the trigger.
- TeX can read any file your account can read, which is why LaTeX builds
  require trust. R and Julia kernels talk to Exposition over local loopback
  TCP with signed but unencrypted messages; Python kernels use CurveZMQ when
  the installed pyzmq, jupyter_client, and ipykernel support it, and are
  otherwise signed but unencrypted as well. The project folder is passed to
  every kernel as data, never as code.
- Rendered pages, kernel HTML/SVG, and Lean output are displayed as inert
  content without access to application commands.

## Files

Text files in UTF-8 (with or without BOM) and UTF-16 (with BOM) are edited
byte-exactly, including CRLF line endings; files with mixed line endings open
read-only until you normalize them. Saves are atomic and never touch the
original on failure; unsaved work is recovered after a crash. Binary and
legacy-encoded files are not opened as text.

## Installation

An NSIS installer installs per user without administrator rights. The
installer is not code-signed yet, so Windows SmartScreen shows a warning.
