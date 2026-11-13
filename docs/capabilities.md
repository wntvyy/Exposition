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
seven registered operations (Python batch run, native Python, Lean audit,
evidence association, agent review, keeping a result, requesting a
promotion), connections, outcomes, and a ceiling on what the workflow may
run. Two presets exist, Numerical experiment (Python; without a script it
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

### Workflow runs

A run starts only from **Review and run…** in the workflow editor, for a
saved, well-formed revision (never from unsaved edits). The review shows
the complete plan: every input file with the SHA-256 of the content the run
will read and whether that version is pinned, every step in the order it
will run with its complete code (inline, or the full text of the script
file it runs) or model instructions, parameters, runner, containment,
effects, and limits, the run's budgets, the restricted image (by name and
by its exact image identity) and folder trust it relies on, notices (native privileges, material sent to
the model provider, proposals you review later), and anything that blocks
it. **Start run** approves exactly that plan. If anything in it changed
before the run starts (the workflow was saved again, an input file changed,
folder trust or the restricted image changed, including the same image name
now naming a different image after a re-tag or pull, a runner became
available or unavailable, or another project is open), the start is
refused and you review the new plan. A plan must be started within 30 minutes.

Steps run one at a time in dependency order; there are no loops,
conditions, retries, or parallel steps, and only one workflow run is active
at a time. Before each step Exposition checks again that the project is
still open, the run was not canceled, its budgets remain, every input file
and earlier output the step reads still has the digest recorded for it, a
native step's folder is still trusted, and the step's runner is still
available with the approved image identity, and the container is started
from that identity; a restricted step never falls back to native
execution. A failed step stops the run and the remaining steps are
skipped. Every output a step returns is checked before a later step can use
it: its declared type, its recomputed SHA-256, that it came from this
step's own service operation, and that formal evidence and retained results
come only from the Lean audit and the retention service (a file that merely
looks like evidence stays a file). **Cancel run** stops the current step;
closing or switching the project cancels its run, and revoking trust
cancels a run that has native steps.

The run's budgets come from the plan: at most as many step dispatches as the
workflow has steps, the sum of the steps' time limits (at most 4 hours),
the sum of their output limits (at most 1 GiB), and 25 model requests per
agent review step; requests a review made count even when it fails or is
canceled. Each step's attempt is recorded before its service is called. If Exposition stops during a run, the next start marks the run
**Interrupted** (a step that was running has an unknown outcome) and never
resumes or repeats it; you mark it as reviewed and start a new run if
needed. Run records are kept in the application data folder on this
computer, not in the project.

| Step | Where it runs | What it produces |
|---|---|---|
| Research Python batch (restricted) | A Docker container (below) | The files it writes for its declared outputs, as research run artifacts |
| Trusted native Python | A new Python process with your account's privileges, in a trusted folder; not a sandbox | Same as the restricted batch |
| Lean audit | The Lean service with the project's toolchain, natively, in a trusted folder | The audit's own per-declaration verdicts, statements, and axioms for exactly the audited text |
| Associate audited declarations | Exposition | A proposal you accept or reject per declaration; only accepted declarations get Lean evidence recorded |
| Read-only agent review | A read-only agent task with commands off, scoped to the declared material, which it sends to the model provider; a tool that would write or run is refused and the review continues | A review report (prose, never evidence) |
| Retain artifact | Research | A kept result with computational evidence naming the mode it actually ran in |
| Request reviewed promotion | Research | A prepared insertion at the end of the manuscript that you review and approve in the run view |

**Restricted Python batches.** Only the declared input files and the code
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
are stored as text and never rendered as active content.

**Limitations.** Workflow code steps are Python only; there are no R or
Julia steps. The container image must provide `python3` and every package
the code needs: Exposition never pulls images and the container has no
network. The trusted native Python step and the Lean audit run with your
account's privileges and are not contained. The Lean audit step has a
10-minute limit, the most the Lean service allows for one check; if the
Lean file is open with unsaved changes that differ from the reviewed file,
the step fails rather than audit other text. A promotion is always
prepared at the end of the manuscript; a discarded one reads Rejected, and
one that can no longer be approved (its 30-minute window passed, or
Exposition restarted) reads Expired.

## Research

| | Python | R | Julia |
|---|---|---|---|
| Discovery of installed environments | Yes | Yes (with IRkernel) | Yes (with IJulia) |
| Managed kernel sessions, run input / selection / file | Yes | Yes | Yes |
| Interrupt | Yes | No — IRkernel ignores interrupts on Windows; Cancel restarts the kernel | Yes (IJulia; a kernel without message-mode interrupts reports it as unsupported, and Cancel restarts it) |
| Figures and tables captured as artifacts with provenance | Yes | Yes | Yes |
| Insert and update a figure/table in a Typst, LaTeX, or Markdown document | Yes | Yes | Yes |

Sessions belong to one project folder and require it to be trusted; switching
or closing the folder, or revoking trust, detaches them. Agent runs use
separate sessions. Exposition never installs runtimes or packages.

Interactive research sessions run natively with your account's privileges
("Trusted native"); the research space and per-owner sessions organize work
but do not contain it. Restricted research execution exists only as the
workflow's Python batch step (see Workflow runs), and the Research page
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
- Workflow restricted Python steps use the same Docker backend and settings,
  with a working copy of only their declared inputs instead of the project
  folder, and likewise never fall back to native execution. Workflow native
  Python and Lean audit steps run with your account's privileges and need a
  trusted folder.
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
