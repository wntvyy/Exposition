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

Approvals show the complete code or text that will run or change, with
invisible and direction-changing characters made visible; what runs is
exactly what was shown, and an approval answered after the project changed
does nothing. A read-only task changes nothing and runs nothing. A response
cut off by the per-turn output or context limit ends as "Stopped" with the
reason, and a task that uses up its own budget ends as "Budget exceeded";
neither is reported as completed. Review reflects the
document's actual content: a change you undid in the editor shows as undone.

The API key is stored in Windows Credential Manager for this computer only
(it does not roam with a domain profile). It is kept out of every child
process's environment and redacted from output, transcripts, and stored
text artifacts. Trusted native code (research kernels, and the commands,
Python, LaTeX builds, and Lean checks you approve) runs as your Windows user
and could read the stored key; Exposition cannot hide it from code you trust.

A live end-to-end run against the real provider is covered by the opt-in
scenario `npm run e2e -- live-agents` (it spends API credit): it computes with
Python, edits the paper, runs a Lean check, builds, reverts through review,
cancels a running task, handles a concurrent user edit, and requires that an
out-of-scope read be refused by the application itself. The default automated
suite uses a scripted development provider that release builds do not contain,
and development builds use a separate development credential entry unless told
otherwise.

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
