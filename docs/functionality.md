# Exposition — functionality reference

| | |
|---|---|
| App | Exposition 0.1.0 (`org.exposition.desktop`), Tauri 2: Rust core (`src-tauri/`, `crates/`) + React/TypeScript workbench (`src/`) |
| Commit | `c8fdca6` (main; inventory taken at `c4669a5`, updated for the later merges: Lean audit hardening, the final agents fixes, credential persistence, `live-restricted`) |
| Date | 2026-11-12 |
| Platform | Windows 11 x64 verified; macOS/Linux structurally supported, unverified |
| Status | Local alpha; NSIS per-user installer, not code-signed |

**What it is.** A local desktop workspace for mathematical and technical documents. Typst, Markdown (print profile) and LaTeX (pdfLaTeX/LuaLaTeX) sources are edited in Monaco and directly on the compiled pages. Both editors share one revisioned document service and one undo history. Python, R and Julia code runs in Jupyter kernels, and the resulting figures and tables can be inserted into documents. Lean files are checked with the project's own toolchain; the verdict comes from a two-step audit that re-checks the file's declarations with the kernel without running the file's code, and the Lean panel lists exactly which declarations were checked. Agent tasks run on the Anthropic Messages API, and the application gates every tool call by scope, consent, permission profile, execution mode and budgets; changes stay reviewable and revertible. Native tools run only in folders the user trusts. Unattended agent execution runs only in a local Docker container.

**At a glance**
- IPC commands: **90 release + 3 dev-only** (93 handlers; 90 `allow-*` grants)
- Events: **16 channels** (10 with payloads + 6 `*:resync`); 7 event enums with 42 variants
- Command palette: **48 registered commands** (47 listed; Show All Commands hides itself); 14 keybindings
- Agent tools: **9**; 13 gate refusal codes; 22 agent error codes; 10 task statuses; 8 tool-call states
- Settings keys: **27 persisted** (App 2, Editor 7, Project 4, Execution 7, Provider 4, Trust 3) + API key (OS store) + 3 frontend-only
- e2e scenarios: **32** (30 in the default run; `live-agents` and `live-restricted` opt-in)
- Workbench: 7 views, 7 editors, 6 status-bar items, 12 dialogs, 8 Settings sections
- Formats: 4 (Typst, Markdown print profile, pdfLaTeX, LuaLaTeX); 4 page-edit operations
- Research: 3 languages (Python, R, Julia); Lean: 14 document statuses, 19 `lean.` error codes
- Network: 1 endpoint (`api.anthropic.com/v1/messages`); 18 external tools in the toolchain catalog

## 1. Workbench and interactions

### Layout
| Region | Contents | What the user can do |
|---|---|---|
| Activity bar (left rail) | One toggle per sidebar view; region toggles; Manage menu | Select a view (selecting the shown view hides the sidebar); toggle Sidebar (Mod+B), Panel (Mod+J), Auxiliary (Mod+Alt+B); Manage: Command Palette… (Mod+Shift+P), Settings… (Mod+,), Color Theme (Light / Dark / Follow system), UI Reference |
| Sidebar (left) | Files (order 0) | Header with Hide; "No folder open" fallback |
| Editor area | Two groups (primary, secondary), resizable split 20–80 % (default 50 %) | Tabs: close button, middle-click, Delete on focused tab, double-click pins a preview tab (italic), dirty dot; per-tab "Document mode" Source / Page / Split (only for editors with >1 mode; new tabs open in Source); Split editor right; Close editor group; empty group shows "Show all commands" |
| Bottom panel | Tabs: Problems (10), Research (20), Jobs (30), Toolchains (90) | Switch tab; Hide (Mod+J); "No output" fallback |
| Right (auxiliary) panel | Tabs: Agent (10), Lean (20) | Switch tab; Hide (Mod+Alt+B) |
| Status bar | Left: build (20), Lean (40), jobs (50). Right: document status (50), agent (50), app version (1000) | See status items |
| Window | 1440×900, min 720×480; title "<folder> — Exposition"; OS drag-and-drop disabled | Close with unsaved documents asks first (Save All / Don't Save / Cancel) |
| Layout persistence | Region visibility, size, active view, editor split (localStorage) | Auto-collapse auxiliary below 1100 px and sidebar below 860 px window width (not persisted; explicit open overrides) |

### Views (7)
| Id | Region · title | What the user can do |
|---|---|---|
| `workspace.files` | sidebar · Files | No folder: Open Folder…, Recent list (open; × "Remove <name> from Recent"; max 12). Folder open: Refresh, Open Folder…; lazy tree (arrows, Enter/click opens a file tab; link icon; outside-folder/unavailable entries disabled); recovery banner: Restore / Discard (unreadable: Copy Location / Discard). No create, rename, delete, context menu or drag-and-drop |
| `typesetting.problems` | panel · Problems | Rows from build-status problems, every diagnostic of each entry's latest build, and Lean diagnostics; severity, source, message, `path:line:col`, hints; click reveals in Monaco and the page view (open files only); "Open Settings → Tool locations" when a hint names it |
| `research.panel` | panel · Research | Environment select + refresh; session pill; Start / Interrupt / Restart / Stop; code box + Run (Ctrl+Enter); output (streams with Show more, figures, inert tables, errors); Cancel run; Provenance; "Insert or update in document…" (document, caption, insert or update an attachment); run history (200); Trust workspace… banner |
| `jobs.panel` | panel · Jobs | Newest-first job list; "Show tool probes"; Cancel; details (program, arguments, directory, policy, pid); output stream filter (both/stdout/stderr), Auto-scroll; untrusted refusal → Trust workspace… |
| `jobs.toolchains` | panel · Toolchains | Tables per category (TeX, Python, R, Julia, Lean, Restricted execution, Version control): status, version, location/guidance; Refresh. "Exposition finds tools on this computer; it never installs them." |
| `agents.panel` | auxiliary · Agent | Toolbar: New task, Tasks, provider settings; provider chip ("Claude · <model>", "Claude · no API key configured", "Claude · credential store error", "Scripted provider — development only"); task list; composer; task view; review; provider dialog |
| `lean.infoview` | auxiliary · Lean | Status pill; Restart file (rebuild imports), Restart Lean server; revision/toolchain/axioms facts; progress; banners (Trust workspace…, Install toolchain, Restart server, Retry now, partially checked, not verified); goals (plain text); "Checked declarations (N)" list (kind, line, name, statement printed with toolchain notation, axioms; Enter/click reveals the name in the editor; generated declarations in a collapsed section; ≤ 300); Incomplete list; axioms / compiled-code list; diagnostics (≤200, click to jump) |

Agent view sub-screens (task list, composer, task view, approval card, review, provider dialog): see §6.13.

### Editors (7)
| Id | Opens | Modes |
|---|---|---|
| `editor.source` | any `file:` text file (Monaco) | source |
| `typst.document` | `*.typ` | source, page, split |
| `latex.document` | `*.tex` | source, page, split |
| `markdown.document` | `*.md`, `*.markdown` | source, page, split |
| `agents.review` | `agents-review:<task>` (review in an editor tab) | source |
| `settings.editor` | `settings:` (8 sections: Appearance, Editor, Project, Trusted folders, Execution, Agents, Research, About) | source |
| `workbench.uiReference` | `ui-reference:` component gallery (all builds) | source |

Source editor (Monaco): shared document model and shared undo; typing groups until a 1000 ms pause, caret jump, insert/delete switch or line break; banners: changed on disk (Reload from Disk / Keep My Changes), deleted (Keep in Editor), save error (Retry / Dismiss), mixed line endings (read-only; Normalize to LF / CRLF), read-only reason, NUL characters; unavailable states for binary, undecodable and too-large files; aliases (case variants, short names, links) collapse to one canonical tab. Build diagnostics (Typst, LaTeX, Markdown) appear as squiggles with hover text and hints once a build of the shown revision reports; earlier marks stay and move with edits. Lean adds hover, `\` abbreviation completion, go-to-definition and diagnostics. Links in hovers are copied to the clipboard, never opened ("Link copied" / "Link not opened"); URL detection in source text is not bundled.

### Status bar items (6)
| Id | Side | Shows |
|---|---|---|
| `typesetting.build` | left | Build state of the active entry (building / "Built X" + ms / failed / pages stale / untrusted / "tool not available"); tooltip = hints |
| `lean.status` | left | `Lean: <status>` for the active `.lean` file; tooltip = toolchain |
| `jobs.running` | left | "N job(s) running" (probes excluded); hidden at 0 |
| `editor.documentStatus` | right | Encoding (UTF-8 / UTF-8 with BOM / UTF-16 LE / UTF-16 BE) · EOL (LF / CRLF / Mixed EOL) · language |
| `agents.status` | right | First match: button "Agent: waiting for approval" / "Agent: N waiting for approval" (opens the oldest pending approval); "Agent: N running"; "Agent: N to review" |
| `workbench.status.app` | right | "Exposition <version>" + " (debug)" in debug builds |

### Dialogs (12)
| Dialog | Choices |
|---|---|
| Save changes to <file>? | Cancel / Don't Save / Save |
| Save changes to N files? (window close) | Cancel / Don't Save / Save All |
| <file> changed on disk (on save) | Cancel / Reload from Disk / Overwrite |
| Trust this workspace? (one at a time; skipped if already trusted) | Don't trust (default) / Trust workspace |
| Revoke trust for this folder? | Cancel (default) / Revoke trust; names nested trusted folders; says running builds, kernels, Lean servers and commands stop now |
| Location of <tool> | Executable path; Cancel / Use this executable (Enter submits); refusal inline |
| Add LaTeX target / Edit target <name> | Name, Main file, Engine; Cancel / Add target or Save target (Enter saves); validation inline |
| Install Lean toolchain <x>? | Cancel / Install toolchain |
| Remove the API key? | Keep key (default) / Remove key |
| Agent provider | Provider, API key (Save key / Remove), Model, Effort, Default content consent; Close / Save settings |
| Command palette | Fuzzy search, Enter runs, Escape closes |
| Open Folder (native folder picker) | Choose a directory |

### Notifications
Default timeouts: info 6 s, success 5 s, warning 10 s, danger sticky; same id replaces; pause on hover/focus.
- Documents: "Could not undo/redo in X", "An edit to X was not applied", "X changed on disk" / "X was deleted on disk", "X was reloaded", "Recovery backup failed for X", "Could not save X" (sticky), "Could not reload X", "Could not resolve X", "Could not normalize line endings".
- Links: "Link copied", "Link not opened".
- Workspace: "Unsaved changes were recovered" / "Unreadable recovery data" (sticky), "Could not open folder", "Could not reopen <name>", "Could not remove <name> from Recent", "Restored unsaved changes to X" (sticky if the disk changed), "Could not restore unsaved changes", "Could not discard recovered changes", "Location copied", "Could not copy the location".
- Settings: "Invalid application/project/execution settings were reset" (warning, sticky, once per distinct warning), "Could not save editor settings", "Could not save the startup setting", "Restricted execution image set/cleared", "Could not change the container image", "Trust revoked", "Could not revoke trust", "<file> now builds with <engine>", "<file> already builds with <engine>", "Could not change the LaTeX engine", "Could not read the project settings".
- Jobs/trust: "Workspace not trusted" (sticky, action Trust workspace…), "Could not cancel job", "Workspace trusted", "Could not trust workspace".
- Page: "Exported PDF" (path), "PDF export failed".
- Research: "No script is active", "Nothing selected", "Research run failed" (only when the window is hidden), "<Figure|Table> inserted into / updated in <path>", "Check the document", "Could not attach the artifact", "Could not cancel the run", "Could not interrupt the kernel", "Could not restart the kernel", "Could not stop the session".
- Lean: "Lean is not ready", "No definition found at the cursor", "Defined outside the workspace", "Go to definition failed", "Installing <toolchain>" (action Show Jobs), "Could not install the toolchain", "Could not restart the Lean server", "Could not restart the file".
- Agents: "Agent needs your approval" (sticky, action Open approval; not shown while the card is on screen; dismissed when resolved or the task stops); "<status>: <task>" when a task stops (completed success with "N files to review"; budget exceeded, stopped, interrupted warning; failed, refused danger; canceled shows none).

### Keyboard shortcuts
| Surface | Keys | Action |
|---|---|---|
| Global (registry) | Mod+Shift+P, Mod+B, Mod+J, Mod+Alt+B, F6, Shift+F6, Mod+W, Mod+\, Mod+PageDown, Mod+PageUp, Mod+,, Mod+O, Mod+S, Mod+Alt+S | See §2. Mod = Ctrl (Cmd on macOS). Chords with Ctrl/Alt/F-keys fire in inputs and Monaco unless the editor handled them; text-editing chords (Mod+A/C/V/X/Z/Y, Mod+arrows/Home/End/Backspace/Delete) never dispatch; IME composition and AltGr characters are text |
| Monaco (app actions) | Ctrl+Z; Ctrl+Y, Ctrl+Shift+Z; Ctrl+S; Ctrl+Alt+S | Shared undo; shared redo; Save; Save All |
| Monaco (bundled defaults) | Ctrl+F, Ctrl+H; F8 / Shift+F8; F12, Ctrl+Click (Lean); Ctrl+Space (Lean); Ctrl+/; Alt+↑/↓; Ctrl+D, Alt+Click; Ctrl+Shift+[ / ]; Shift+F10 | Find/replace; next/previous problem; go to definition; suggestions; comment; move line; multi-cursor; fold/unfold; context menu |
| Lean source | `\name` then Space, Tab, `( ) , [ ] { }` or Enter | Expand abbreviation (202 entries; `\\` does not expand); `\` opens a ≤60-entry completion list |
| Command palette | type, ↑/↓, Enter, Escape | Filter, move, run highlighted or top match, close |
| Editor tabs | Delete (focused tab), middle-click, double-click | Close, close, pin preview |
| Resize handles | arrows (5 %), Home/End, double-click, F6/Shift+F6 | Resize, min/max, reset, move region focus |
| Dialogs | Escape; Tab (trapped); Enter in path/target fields | Cancel; cycle focus; submit |
| Research code box | Ctrl/Cmd+Enter | Run selection or all code (starts a session if none) |
| Files tree | arrows, Enter | Navigate, open file / expand folder |
| Approval card | Tab | Payload (only if it scrolls), then Approve, Decline |
| Page editor | see below | |

### Page-editor interactions (Page and Split modes)
| Interaction | Behavior |
|---|---|
| Click / drag / Shift+click | Hit-test places the caret, extends the selection (drag rAF-throttled); text from another file says "In <path> — open that file to edit it"; generated, code-produced, transformed, raw or package text shows "… — edit in source" |
| Arrows, Home/End, Ctrl+←/→, Ctrl+Home/End | Move by character/line (↑/↓ keep goal x), line start/end, word, document start/end; Shift extends; ←/→ with a selection collapse to its edge; Escape collapses |
| Typing, IME, paste (plain text) | `insert` edit; composition shown live, committed on end; LaTeX special characters inserted escaped |
| Backspace / Delete | Delete backward / forward (Ctrl+Backspace = Backspace; no word delete) |
| Enter (also Shift+Enter) | Paragraph break |
| Copy / Cut | Copies the selection's source text; cut = copy + delete |
| Ctrl+A | Select all |
| Ctrl+Z; Ctrl+Y, Ctrl+Shift+Z | Undo / redo in the shared history (same stack as Monaco and agent edits) |
| Refused edits | Structural edits refused with "would change the document structure — edit in source"; read-only document (e.g. mixed line endings: banner with Normalize to LF / CRLF) refuses edits with its reason |
| Concurrent edits | Keystrokes follow moved positions; if they cannot be placed: "Typing paused: …", nothing applied at a guessed position until the caret is placed again |
| Stale pages | Failed build keeps last good pages with "Showing pages from an earlier revision" + Rebuild; clicking/moving refused, typing still works |
| Toolbar | Status pill (Building / Not trusted / Not built / Build failed (n errors) / Built + ms), Text/Math caret mode, zoom 50–400 % (step 25 %, default 125 %), Export PDF (enabled once pages exist) |
| Split view | Source (Monaco) left, page right, 50/50 resizable (min 20 %); one document, edits appear in both immediately |
| Navigation | Problems click places the page caret and the Monaco cursor at the diagnostic; no automatic source-cursor ↔ page-caret sync; not handled: PageUp/PageDown, Tab, double/triple-click, context menu, keyboard/Ctrl+wheel zoom, drag-and-drop |

## 2. Command palette

48 registered commands (17 built-in + 31 feature). The palette lists the 47 other than `workbench.action.showCommands`, only while enabled, as `Category: Title`, ranked by fuzzy match; an empty query shows the session's 20 most recently run first. Keybindings fire only when the command is enabled. None sets a custom keybinding scope.

| Label | Id | Keybinding | When available |
|---|---|---|---|
| View: Show All Commands | `workbench.action.showCommands` | Mod+Shift+P | always (not listed in the palette) |
| View: Toggle Sidebar | `workbench.action.toggleSidebar` | Mod+B | always |
| View: Toggle Panel | `workbench.action.togglePanel` | Mod+J | always |
| View: Toggle Auxiliary Panel | `workbench.action.toggleAuxiliary` | Mod+Alt+B | always |
| View: Focus Next Region | `workbench.action.focusNextRegion` | F6 | always |
| View: Focus Previous Region | `workbench.action.focusPreviousRegion` | Shift+F6 | always |
| View: Close Editor | `workbench.action.closeEditor` | Mod+W | an editor is active |
| View: Split Editor Right | `workbench.action.splitEditor` | Mod+\ | an editor is active |
| View: Close Right Editor Group | `workbench.action.closeEditorGroup` | — | right group has editors |
| View: Open Next Editor | `workbench.action.nextEditor` | Mod+PageDown | an editor is active |
| View: Open Previous Editor | `workbench.action.previousEditor` | Mod+PageUp | an editor is active |
| View: Reset Layout | `workbench.action.resetLayout` | — | always |
| Preferences: Toggle Light/Dark Theme | `workbench.action.toggleTheme` | — | always |
| Preferences: Color Theme: Light | `workbench.action.theme.light` | — | always |
| Preferences: Color Theme: Dark | `workbench.action.theme.dark` | — | always |
| Preferences: Color Theme: Follow System | `workbench.action.theme.system` | — | always |
| Developer: Show UI Reference | `workbench.action.showUiReference` | — | always (release builds too) |
| Preferences: Open Settings | `workbench.action.openSettings` | Mod+, | always |
| Preferences: Manage Trusted Folders | `settings.trustedFolders` | — | always |
| Preferences: Restricted Execution and Tool Locations | `settings.execution` | — | always |
| Preferences: Project Settings and LaTeX Targets | `settings.project` | — | always |
| LaTeX: Build This File with LuaLaTeX | `latex.useLuaLatex` | — | active file is `.tex` and a folder is open |
| LaTeX: Build This File with pdfLaTeX | `latex.usePdfLatex` | — | active file is `.tex` and a folder is open |
| File: Open Folder… | `workspace.openFolder` | Mod+O | always |
| File: Close Folder | `workspace.closeFolder` | — | a folder is open |
| File: Save | `file.save` | Mod+S | active editor has a document |
| File: Save All | `file.saveAll` | Mod+Alt+S | always |
| Jobs: Show Jobs | `jobs.show` | — | always |
| Jobs: Refresh Job List | `jobs.refresh` | — | always |
| Jobs: Show Toolchains | `toolchains.show` | — | always |
| Jobs: Re-check Installed Toolchains | `toolchains.refresh` | — | always |
| Lean: Show Lean Infoview | `lean.showInfoview` | — | always |
| Lean: Restart Lean Server | `lean.restartServer` | — | active editor is a tracked Lean file |
| Lean: Restart File (Rebuild Imports) | `lean.restartFile` | — | same |
| Lean: Go to Definition | `lean.goToDefinition` | — | same |
| Lean: Trust Workspace to Check Lean | `lean.trustWorkspace` | — | active Lean file is Untrusted |
| Lean: Install Project Toolchain | `lean.installToolchain` | — | toolchain missing and installable |
| Research: Show Research Panel | `research.show` | — | always |
| Research: Run Selection in Research Session | `research.runSelection` | — | always (editor selection, else panel selection, else "Nothing selected") |
| Research: Run File in Research Session | `research.runFile` | — | always (runs only `.py`/`.ipy`/`.r`/`.jl`, else "No script is active") |
| Research: Interrupt Research Kernel | `research.interrupt` | — | some session is busy |
| Research: Restart Research Kernel | `research.restart` | — | a user session exists |
| Research: Stop Research Session | `research.stop` | — | a user session exists |
| Research: Re-check Python, R, and Julia Environments | `research.refreshEnvironments` | — | always |
| Research: Enable Research for This Project | `research.enable` | — | a folder is open (creates the research space; runs nothing) |
| Agent: New Agent Task | `agents.newTask` | — | always |
| Agent: Show Agent Tasks | `agents.showTasks` | — | always |
| Agent: Open Pending Agent Approval | `agents.openApproval` | — | a task is waiting for approval (opens the first; focuses its card) |
| Agent: Agent Provider Settings | `agents.providerSettings` | — | always |

Engine switch (`latex.*`): changes the engine of the target that builds the active file (default target if it builds that file, else the first LaTeX target for it), or adds a target named after the file; saved to `.exposition/project.json`; open page views rebuild.

## 3. Documents and formats

### 3.1 Formats compared

| | Typst | Markdown (print profile) | LaTeX pdfLaTeX | LaTeX LuaLaTeX |
|---|---|---|---|---|
| Entry files | `.typ` | `.md`, `.markdown` | `.tex` (`.ltx`/`.sty`/`.cls`/`.bib` are inputs only) | `.tex` |
| Engine | In-process Typst `0.15.1` | pulldown-cmark 0.13 → Typst via profile `exposition-markdown-print/1` (generated Typst never written to disk) | External `pdflatex` + `bibtex`/`biber` | External `lualatex` + `bibtex`/`biber` |
| Engine choice | — | — | Default for any `.tex`; or a `.exposition/project.json` target for that entry (the `defaultTarget` if it matches, else the first matching target) | Only when a project target for the entry names `luaLatex` (Settings › Project) |
| Dispatch | `typesetting_watch`/`typesetting_build` route by extension: `.tex` → TeX service; `.typ`/`.md`/`.markdown` → typesetting service; any other file → `typesetting.unsupported` (no Typst fallback). `typesetting_export_pdf` routes build ids starting `tex-` to TeX. | same | same | same |
| Build trigger | Page/Split view opens → watch → build now, then on every document change or file change in the folder; Rebuild button; agent `build_document`. Source-only view does not build. | same | Watch → Draft build now and on each change; a successful Draft schedules a Full build after 1.5 s idle; `typesetting_build` = Full (waits ≤ 600 s); newer request cancels the running build. Saving Project settings rebuilds watched `.tex` views. | same |
| Draft / Full | single compile | single compile | Draft: one pass reusing `.aux`/`.bbl`. Full: up to 5 passes; biber if `.bcf`, bibtex if `\bibdata`; reruns on "Rerun…"/label messages or `.aux` change | same |
| Trust | Not required (no child process) | Not required | Trusted folder required; untrusted → build finishes `error` with refusal `untrustedWorkspace` + "Trust workspace…" | same |
| What the build reads | Snapshot: all open buffers (captured atomically) + disk files on demand, inside the canonical root, links checked. Page views read the whole project; agent builds read only the task scope (others read as "access denied"; result not retained for page views) | same | App-owned mirror of the project with open buffers written over it; `TEXINPUTS` falls back to the project; TeX can read any file the account can read | same |
| Packages / fonts | Local packages only (`%APPDATA%\typst\packages`, `%LOCALAPPDATA%\typst\packages`); embedded + system fonts; no downloads | Template: New Computer Modern, DejaVu Sans Mono | Installed TeX distribution | same |
| Engine flags | — | — | `-synctex=1 -interaction=nonstopmode -file-line-error -no-shell-escape -recorder`; env `openout_any=p openin_any=p shell_escape=f`; 180 s timeout, 1 MiB log | same |
| Pages shown as | SVG (vector, not rasterized) | SVG | PNG rendered in-process (hayro) at 3 px/pt | same |
| Diagnostics | Typst errors/warnings with UTF-16 ranges and hints (trace points, package hint) | Translation diagnostics + Typst diagnostics remapped to Markdown ranges; unsupported math → red marker + warning | From the log: errors, undefined references/citations (hint: run a full build), LaTeX/package/class warnings, over/underfull boxes (info), missing characters (one warning per font); whole-line ranges; missing/failed bib tool; "did not run as a TeX engine" | same |
| Squiggles in Monaco | Markers (owner `typesetting`) from the latest build that compiled the editor's current revision; hover = message + hints; kept and moved with edits until a build of the current text reports | same | same (line ranges) | same |
| Missing tool | — | — | Status `toolUnavailable` with guidance; Problems/status bar/page view link to Settings › Tool locations | same |
| Page map | Glyph/character level from Typst spans, verified against source | Character level, Typst map remapped through the Markdown source map | Glyph level for mapped text (SyncTeX line windows + LaTeX source model + PDF glyph alignment, checked glyph by glyph); line level (SyncTeX) otherwise | same |
| Page editing | Prose and equation tokens | Prose, declared LaTeX-math subset, front-matter `title`/`author`/`date` values | Mapped prose and math | same; also combining marks and CJK |
| Failed build | Last good pages kept (`lastGood`), shown stale | same | same | same |
| Retention | 8 recent builds + last good per entry, 64-page SVG cache | same | 8 builds + last good per target; 4 frozen build folders (PDF, SyncTeX, log) per target; 48 cached pages per build | same |
| Open-folder binding | Build records, pages, page maps, revision texts and exports are served only while the folder that built them is open | same | same | same |
| Edit-to-page time (docs; debug build, Ryzen 7 9800X3D) | ~2–15 ms | ~16–26 ms | ~0.3–0.45 s | ~0.8–1 s |

Markdown front matter: `title`, `author` (scalar or list) → title block + PDF metadata; `date` → title block, and PDF metadata only when `YYYY-MM-DD` is a valid calendar date; `papersize` (a3, a4, a5, b5, letter, legal; default a4); `fontsize` (6–24 pt; default 11); `margin` (cm/mm/in/pt; default 2.5cm); `lang` (default en). Other keys reported and ignored. Images: workspace files relative to the document or `/`-rooted; URLs, `data:`, `mailto:`, `file:`, absolute, backslash, NUL, or escaping paths → warning + marker. Raw HTML prints verbatim with a warning. No citations/bibliography.

### 3.2 PDF export (`typesetting_export_pdf`)
- Bytes: Typst/Markdown = `typst_pdf` from the same layout as the pages; LaTeX = the build's own PDF.
- Default destination (the UI always uses it): `<open folder>/<entry folder>/<entry stem>.pdf`. Success → notification "Exported PDF" with path.
- Explicit destination (IPC argument only): absolute, `.pdf` extension (any case), parent folder exists.
- An existing destination must be a regular file starting `%PDF-`, not a file the build read (e.g. an included `.pdf` figure), and not an open document (canonical-path comparison). Otherwise refused: "the destination exists and is not a file" / "refusing to replace a file that is not a PDF" / "refusing to replace a source file of the document".
- Write: temp `<name>.pdf.<pid>.tmp`, then rename; temp removed on failure. The build must belong to the open folder. All export errors surface as `typesetting.export`.

### 3.3 Page edit operations (`page_edit`)
- Ops (only these 4): `insert{text}` (typing, paste, replace-selection), `deleteBackward`, `deleteForward`, `paragraphBreak` (Enter / Shift+Enter). Cut = delete with a selection.
- Outcomes: `applied{revision, caret, mode, rebased}`, `unchanged`, `refused{refusal}`, `stale{currentRevision}` (client maps the selection forward and retries).
- Refusals: `notEditable{reason}` ("not editable on the page (…)"), `wouldChangeStructure` ("…edit in source"), `invalidPosition`, `noChange` (silent).
- `notEditable` reasons — Typst: `code`, `raw`, `comment`, `definition or show rule`, `generated`, `line breaks inside equations`. Markdown: `code`, `raw HTML`, `front matter`, `front matter settings`, `generated content`, `equation delimiters`, `equation structure`, `link or emphasis syntax`, `line breaks inside equations`, `paragraph breaks here`, `table cells`, `footnote definitions`, `setext headings`. LaTeX: `LaTeX markup or generated content`. Any format: `read-only: <reason>`, `the file mixes line endings; normalize them to edit it`, `no page editing for this format yet` (no rules for the extension).
- Transaction: origin `page`, conflict policy rebase-disjoint; consecutive caret typing merges into one undo step (`Typing`), caret deletes merge (`Delete`), Enter = `Paragraph break`; undo/redo shared with the source editor.
- Escaping on insert: Typst escapes markup characters (verified by reparse; three escalation levels); Markdown escapes `\` `` ` `` `$` and context-dependent markers; LaTeX prose: `\` → `\textbackslash{}`, `~` → `\textasciitilde{}`, `^` → `\textasciicircum{}`, `<` `>` `|` → `\textless{}` `\textgreater{}` `\textbar{}`, `"` → ` `` ` (word start) / `''`, `{ } $ & % # _` → backslash-escaped; LaTeX math: `{ } $ & % #` escaped, `\` → `\backslash`, `~` → `\sim`, typed Greek/operators → their macros.
- Refused contexts — Typst: code, raw, comments, `#let`/`#set`/`#show`/closures/loops/imports, generated text. Markdown: code, raw HTML, generated atoms (task boxes, footnote refs, rules), delimiters, `\frac`/`\left` structure, front-matter keys. LaTeX: commands/markup, verbatim (`verbatim`, `lstlisting`, `minted`, `\verb`), comments, `\left`/`\right`/`\big…` delimiters, generated text; Enter only in body prose.

### 3.4 Page images and request limits
- Raster cap: 32 Mi pixels per page image (`MAX_PAGE_IMAGE_PIXELS`); larger LaTeX pages render at a lowered scale. Typst/Markdown SVG is not rasterized.
- `tex_page_image` scale 0.25–8 px/pt; page index < 10 000; build id `tex-…` ≤ 64 chars `[A-Za-z0-9-]`; `page_image` renderRef 1–128 chars `[A-Za-z0-9_-]`; page coordinates must be finite.

### 3.5 Format error codes
- `typesetting.`: `unsupported` (not `.typ`/`.md`/`.markdown`/`.tex`), `noWorkspace` (watch with no folder), `build` (could not run; LaTeX not finished in 600 s or channel closed), `export` (every export failure: invalid/refused destination, unknown build, I/O).
- `tex.`: `unsupportedTarget` (not LaTeX with pdfLaTeX/LuaLaTeX), `workspaceNotOpen`, `noTarget` (no LaTeX target and entry not `.tex`), `unknownBuild` (evicted, unknown, or of a folder no longer open), `noOutput` (no pages), `noSuchPage` (index too high or ≥ 10 000), `invalidScale` (outside 0.25–8 or not finite), `invalidDestination` (defined; unreachable over IPC, export reports `typesetting.export`), `io` (I/O or worker join), `invalidBuildId` (not `tex-…`, > 64 chars, bad characters).
- `page.`: `unknownBuild` (build or page map gone), `invalidRef` (bad renderRef), `unknownPage` (page image gone), `invalidPoint` (non-finite x, y or goalX).
- `page_edit` / `document_*` raw errors (`TransactionError`): `empty`, `unknownDocument`, `duplicateDocument`, `readOnly`, `mixedLineEndings`, `staleRevision`, `conflict`, `historyUnavailable`, `futureRevision`, `invalidRange`, `notCharBoundary`, `overlappingPatches`, `nothingToUndo`.

### 3.6 Files
- Encodings: UTF-8, UTF-8 with BOM, UTF-16 LE/BE with BOM; decoded strictly and saved byte-exactly in the same encoding. UTF-32 BOM or dense control characters (≥ 8 and ≥ 1 %) → `file.binary`; invalid bytes (legacy code pages) → `file.undecodable`; not opened as text.
- Size: open and save limited to 10 MiB (`file.tooLarge`); a save that would produce a file Exposition would refuse to reopen (too large or binary-looking) is refused with the buffer kept.
- Line endings: LF, CRLF, none, or mixed, re-detected on every revision. Mixed (incl. any lone CR) is read-only for every origin (source, page, agent, research, system): only a change whose result is consistent, a reload from disk, or a recovery restore is accepted; undo from mixed into other mixed text is refused. `document_normalize_line_endings` (LF or CRLF) is one undoable "Normalize line endings" step. Source editor and page view show "Mixed line endings" with Normalize to LF / Normalize to CRLF.
- Saves: refused if the file changed on disk since load/save (`file.modifiedOnDisk`) unless overwrite; refused for OS read-only files (`file.readOnly`) and folders; written to `.<name>.exposition-save-<unique>.tmp`, synced, renamed over the original; on failure the temp is removed, original untouched, buffer stays dirty. A deleted file is recreated (missing folders created). Save All reports per-file failures.
- Recovery: dirty buffers journaled (≤ every 300 ms) to the app data folder; journals removed when the buffer is saved, returns to its saved text, or is closed without saving; kept when the folder closes or the app crashes. Next open of the folder lists them (newest first; disk unchanged/changed/missing/unknown); Restore applies them as one undoable "Restore unsaved changes" step (refused if the open buffer has unsaved changes); Discard deletes. Corrupt/newer journals are quarantined (`bad-…`) and shown with their location.
- External changes (watcher, 150 ms settle): clean buffer → reloaded automatically as an undoable `external` step; dirty buffer → conflict banner: modified ("Reload from Disk" = `document_reload`, undoable; "Keep My Changes"), deleted ("Keep in Editor"; buffer marked unsaved, save recreates it), unreadable ("Keep My Changes"); keep = `document_keep_buffer` (next save overwrites); disk matching the buffer again clears the conflict. Failed save → "<name> was not saved" banner with Dismiss / Retry.
- Same physical file reached by another name (link or other spelling) → the same document. Hidden from tree/watcher: `.git`, `.lake`, `node_modules`, `target`, save temps.
- Undo history: 1000 undo steps, 2048 retained revisions per document.

## 4. Research

Python, R and Julia code runs in Jupyter kernels that a bundled Python helper (`exposition_kernel_host.py`, protocol 1) supervises. Sessions need an open, trusted folder. Exposition never installs runtimes, kernels or packages.

| Language | Discovered at (Windows; R/Julia take the first match) | Ready when | Kernel |
|---|---|---|---|
| Python | `py.exe -0p`; `python.exe` on PATH; `%LOCALAPPDATA%\Python\pythoncore-*`, `%LOCALAPPDATA%\Programs\Python\Python*`, `%PROGRAMFILES%\Python*`, `%PROGRAMDATA%\Python*`; anaconda3/miniconda3/miniforge3 (also mambaforge in the profile); project `.venv`/`venv` only if trusted | `ipykernel` and `jupyter_client` both import | that interpreter's ipykernel (`exposition-python`) |
| R | `Rscript.exe` on PATH, `%PROGRAMFILES%\R\R-*\bin[\x64]`, `%LOCALAPPDATA%\Programs\R\R-*\bin[\x64]` | a host Python with `jupyter_client` has an `r` kernelspec (IRkernel) | that kernelspec |
| Julia | `julia.exe` on PATH, `%LOCALAPPDATA%\Programs\Julia*\bin`, `%USERPROFILE%\.juliaup\bin` | the same, for a `julia` kernelspec (IJulia) | that kernelspec |

- **Discovery:** drops missing files, Store stubs (`WindowsApps`), duplicates and system candidates inside the project. Probes are 30 s application jobs (`EXPOSITION-PROBE` report); project interpreters run as trusted jobs with `-I`. Unavailable entries show a reason + `pip install` hint. Cached per (folder, trusted); refresh re-probes.
- **Start / launch:** open folder + trust + a discovered environment; ≤ 8 user and 4 agent sessions (within the execution service's 16 interactive jobs, shared with Lean). `<host python> -u -X utf8 <helper>` is a TrustedNative interactive job in the folder: scrubbed env (`JUPYTER_RUNTIME_DIR=<data>/research/runtime`), 64 KiB redacted log, helper at `<data>/research/kernel-host/0.1.0-<hash12>/` (hash re-checked each launch). R/Julia get the folder as byte data (`rawToChar(as.raw(..))` / `hex2bytes`), never spliced into code.
- **States:** Starting, Idle, Busy, Restarting, Crashed{reason} (helper silent 20 s then killed, helper exit, kernel died; active run → Failed, queued → Canceled), Detached{reason} (final: folder switched/closed or trust revoked; kernel stopped, runs canceled), Stopped (final).
- **Owners:** UI = User. The agent `run_python` tool creates `Agent{task}` sessions (first ready Python; reused while live; all stopped when the task ends). Only `execute` checks requester = owner. On app exit helpers die with the execution service's job shutdown.
- **Run kinds:** Input `{code}` (non-empty, ≤ 4 MiB); Selection `{document, revision, UTF-16 range}` (revision must be current, else `staleSource`); File `{path}` (open buffer, else hashed saved bytes; under the session's folder; extension `py`/`ipy`/`pyw`, `r`, `jl` must match). One active run per session, FIFO queue; ids `run-<16 hex>`.
- **Run statuses:** Running, Succeeded, Failed{message} (error, kernel died, helper stopped; a manifest left Running reads as "Exposition stopped before this run finished."), Canceled (aborted, restarted, shut down, cancel, trust revoked), Interrupted.
- **Outputs:** Stream (stdout/stderr), Display, Error (ename, evalue, traceback), ClearOutput, Notice; `text/plain` inline (≤ 16 Ki chars, ANSI stripped). **Artifacts:** Figure (first of PNG, JPEG, GIF, SVG per display), Table (first HTML `<table>` → CSV ≤ 500 rows × 50 cols, notice when cut), Data (HTML kept as inert `.html`). Limits: 200 files and 64 MiB/file per run, streams 8 Mi chars/run, live retention 2 MiB / 10,000 entries, output pages 200 (max 500), history ≤ 500 manifests.
- **Storage:** `.exposition/runs/<run>.json` (`exposition.run/1`: inputs + revision + sha256, session/interpreter/kernel/helper provenance, code sha256 + text ≤ 256 KiB, error, output summary); `.exposition/artifacts/<run>/output-N.<png|jpg|gif|svg|html|csv>` + `transcript.json`. Files must be direct children of the run folder; size and sha256 recomputed; bytes served only on request, never in events.
- **Redaction:** streams, errors, notices, logs, and text artifacts (HTML/SVG/CSV/JSON, rewritten on disk with `[REDACTED]`) before registration; images untouched.

| Attach (`artifact_attach`) | Typst `.typ` | LaTeX `.tex`/`.ltx` | Markdown `.md`/`.markdown` |
|---|---|---|---|
| Markers | `// exposition:begin|end <key>` | `% exposition:begin|end <key>` | `<!-- exposition:begin|end <key> -->` |
| Figure | `#figure(image(path, width: 80%), caption) <key>` | `figure`, `\includegraphics[width=0.8\linewidth]`, `\caption`, `\label{key}` | `![cap](path)` |
| Table | `#figure(table(..., table.header(..)), caption) <key>` | `table` + `tabular` with booktabs rules + `\label` | pipe table + `Table: cap` |
| Figure types | png, jpeg, gif, svg | png, jpeg, pdf (SVG refused: emit PNG) | any `image/*` |

- **Insert:** `baseRevision` must be current; offset not inside another block (LaTeX: inside the document body); adds a missing `\usepackage{graphicx|booktabs}` before `\begin{document}` or warns. Key given or derived (`fig-`/`tab-` + 8 hex of the hash; on collision a `-<n>` suffix counting from 2). UI default: end of document, or before `\end{document}`.
- **Update:** exactly one begin + one end marker, same kind; keeps the caption unless a new one is given; replaces the whole block. Artifact sha256 must equal the manifest's; CSV ≤ 4 MiB, UTF-8, ≤ 500 × 50. One undoable transaction (origin `Research{run}`). LaTeX image paths are relative to the file with `\documentclass`, else the project target including it, else the first target (warning), else a scanned main file, else its own folder (warning).
- **Interrupt:** Idle/Busy only. Windows: Python `event`; kernelspec declaring `message` (IJulia's does) `message`; other kernels (IRkernel) `unsupported` + notice to use Cancel. Elsewhere `signal`. **Cancel run:** queued → Canceled at once; active → interrupt, then after 5 s the kernel restarts and the run is Canceled. **Restart:** refused when Detached, Stopped, Starting or Restarting; cancels queued and active runs; failure → Crashed. **Stop:** cancels queued runs, `shutdown` (20 s), waits 5 s, kills the tree.
- **Error codes:** `research.` + `noWorkspace`, `invalidWorkspace`, `notTrusted`, `policyRefused`, `unknownEnvironment`, `environmentUnavailable`, `unknownSession`, `unknownRun`, `unknownArtifact`, `ownerMismatch`, `workspaceMismatch`, `tooManySessions`, `sessionNotReady`, `invalidRequest`, `staleSource`, `launchFailed`, `timeout` (120 s), `helperGone`, `io`, `helper.<code>`. Helper: `protocol.{invalidRequest, handshakeRequired, unknownCommand, versionMismatch}`, `kernel.{alreadyStarted, invalidCwd, notRunning, dead, ipykernelMissing, invalidSpec, noSuchKernel, startFailed}`, `run.{invalidId, invalidCode, invalidOutputDir, duplicate}`, `helper.internal`, `helper.unknown`.
- **Attach codes:** `attach.` + `unsupportedFormat`, `document`, `stale`, `missingMarkers`, `ambiguousMarkers`, `duplicateKey`, `invalidKey`, `invalidPlacement`, `artifact`, `artifactChanged`, `unsupported`, `invalidTable`, `kindMismatch`.

### 4.1 Research page, research space, and the project graph (M7)

- **Research page** (`research`, Mod+2): explorer over the research space (Sources, Data, Notes, Experiments, Results, Workflows read-only, References), Monaco editing of research files in the shared document model (the same model the Editor page uses; closing a research tab asks about unsaved changes unless an Editor tab still shows the file), the shared session/run store (one set of sessions with the bottom Research panel), Run file / Run selection, outputs, and an inspector (provenance, relations, evidence with freshness, impact, entities bound in a file). The Files explorer has a Research subsection that opens the page at an item.
- **Execution labels:** every session is **Trusted native** (runs with the account's privileges; the research folder and sessions organize work but are not a sandbox). Restricted research execution is shown as not available; nothing falls back to or from it.
- **Enable Research** (`research_space_enable`): one `ResearchSpace` entity per project (contained by the project) and `.exposition/research/<rs-id>/{sources,data,notes}`; idempotent; starts nothing; needs no manuscript. Research files are admitted as resources (members of the space) when created, imported, or listed.
- **Import** (`research_space_import`): copy into a group (never overwrites: `name (2).ext`; the copy's sha256 must match; origin recorded as `import.*` attributes) or a read-only `ExternalResource` reference (never copied, crawled, or run). Files inside `.exposition/` are refused.
- **Declared inputs:** `research_execute` accepts `declaredInputs` (at most 32 project files, 1 GiB each); each is pinned by sha256 (open buffer first) in the run's inputs when it is dispatched. `RunManifest.unsavedInputs` lists inputs that were unsaved buffers (their hash is the buffer's; the code itself is kept in the manifest up to 256 KiB).
- **Retention** (`research_retain`, `research_result_state`): Keep / Archive / Discard. Keeping admits the artifact file as one resource (`artifact`; tables `dataset` + `artifact`), verified against the run's recorded sha256 (`research.artifactChanged` otherwise), with service-owned `Computational` evidence (run id, inputs with hashes/revisions/unsaved flag, environment and kernel versions, output hash), `derivedFrom` its run inputs (and the chosen experiment), and membership in the space. Discard changes the lifecycle only (no file, attachment, or evidence is removed); output never kept cannot be discarded into the graph (`research.notRetained`).
- **Experiments** (`research_experiment_save`, `research_experiment`): an `Experiment` entity (objective, procedure) created through the validated change path as the user: `experiment derivedFrom` procedure and declared inputs, `outcome derivedFrom experiment`; attempts are the runs of the procedure (and the runs of its outcomes), listed from run history. Updates need the entity revision (`research.experimentChanged`).
- **Reviewed promotion** (`research_promotion_prepare`, `research_promotion_approve`, `research_promotion_cancel`): prepare verifies the artifact digest and plans the format adapter's exact edits against one document revision (kept in Rust for 30 min); approval re-verifies the digest (`research.artifactChanged`) and the revision (`promotion.stale`), re-plans and compares (`promotion.planChanged`), writes the intent to the operation journal, applies one undoable transaction with `Reject` conflicts, then retains the artifact and records `manuscript usesAsset artifact`. If the graph write fails, the outcome is `pendingReconciliation` and the journal record is `NeedsReconciliation` (shown on the Research page); `research_promotion_reconcile` records the attachment if its markers are in the document (never re-applying the edit) or closes the record. Undo/redo of the insertion removes/restores the `usesAsset` relation research recorded; on project open, interrupted promotions are marked for reconciliation and followed attachments are re-checked. "Insert or update in document..." uses this review.
- **Codes:** `research.notEnabled`, `research.artifactChanged`, `research.notRetained`, `research.notExperiment`, `research.invalidExperiment`, `research.experimentChanged`, `research.runActive`, `promotion.{unknown, expired, projectChanged, stale, planChanged, journal, document, unknownOperation, unreadable}`, plus `graph.*` codes from the graph service.

## 5. Lean

- **Project:** folder with `lakefile.lean`/`lakefile.toml`; other files are standalone (root = open folder, pin = nearest `lean-toolchain`). Scan: depth 6, 20,000 entries, skips dot-folders, `.lake`, `lake-packages`, `node_modules`, `target`; runs nothing.
- **Toolchain:** pin = `lean-toolchain`, else elan `default_toolchain` (`ELAN_HOME` or `~/.elan`); channels pick the newest installed match (with a note). Resolved directly to `<ELAN_HOME>/toolchains/<dir>/bin/{lean,lake}`; elan shims never run; nothing downloads implicitly. Version from `lean --version`.
- **Install:** command "Install Project Toolchain" / Infoview banner → confirm (elan downloads from the internet) → needs trust and elan (settings location re-checked at use, `ELAN_HOME/bin`, PATH) → `elan toolchain install <qualified>` trusted batch job (1 h) → waiting files re-attach.
- **Server:** `lake serve` (or `lean --server` for standalone files), one per (project root, toolchain), TrustedNative interactive job; LSP over stdio, Content-Length framed (header ≤ 8 KiB, body ≤ 64 MiB); `hasWidgets:false` (no RPC/widgets). Crash → auto-restart after 1/2/4/8/16 s; Unavailable retried every 10 s and on edit. Stops 120 s after its last file closes, and on folder switch/close or trust revoke; `lean-toolchain`/`lakefile.*` changes restart it. **Restart Lean Server:** shutdown (3 s) + re-attach. **Restart File:** `didClose` + `didOpen` (rebuilds out-of-date imports).

| Feature | Behaviour |
|---|---|
| Diagnostics | the current version only. Messages ≤ 20,000 bytes, with hints for `sorry` and unsolved goals. Monaco markers and Problems (source "Lean") only when they match the editor revision |
| Hover | synced revision only. Untrusted Markdown, no HTML, ≤ 50,000 chars. Links are copied to the clipboard, never opened |
| Goals | cursor or focus, 120 ms debounce → `plainGoal` + `plainTermGoal`. Plain text: hypotheses / `⊢`, "Goal i of n", "Expected type". A newer query cancels the older one |
| Definition | Ctrl+Click, F12 or Go to Definition; ≤ 50 targets. Opens the workspace file, or notifies "Defined outside the workspace" |
| `sorry` marks | semantic tokens `leanSorryLike`, decorated and listed in the Infoview |
| Abbreviations | 202 entries. `\` gives completion (≤ 60). Auto-expands on space, tab, `()[]{},` or newline. Undoable; `\\` does not expand |
| Syntax | Monarch grammar; `sorry`, `admit` and `stop` tokenized as invalid |

| Status (shown as) | Meaning |
|---|---|
| NotStarted (Not checked) | not yet sent to Lean |
| Unavailable (Lean unavailable) | no folder open, or a bad `lean-toolchain` file |
| ToolchainMissing (Toolchain missing) | the pinned toolchain is not installed, or there is no pin and no default |
| Untrusted (Untrusted) | the folder is not trusted; nothing runs |
| Checking (Starting Lean / Checking / Auditing axioms) | the server is starting, Lean is elaborating (progress line), or the audit is running |
| Checked (Checked) | no errors, no `sorry`, reached the end, the kernel re-checked every dependency, only the toolchain's `propext`/`Classical.choice`/`Quot.sound` |
| CheckedWithSorry (Incomplete (sorry)) | depends on `sorry`, including imported or warning-suppressed uses ("found by the axiom audit") |
| ReliesOnCompiledCode (Relies on compiled code) | native evaluation axioms (`native_decide`) or the toolchain's `ofReduceBool`/`ofReduceNat`/`trustCompiler` |
| CheckedWithAxioms (Checked with axioms) | any other axiom. A non-toolchain axiom with a standard name is shown "(declared outside the toolchain)" |
| PartiallyChecked (Partially checked) | stopped before the end (`#exit` or another terminal command) |
| Unverified (Not verified) | the kernel rejected a declaration on replay (for example `skipKernelTC`); a non-toolchain `prelude` module or a declaration named like a constant the kernel evaluates by name; a declaration the language server indexes for this file is missing from the audit (tripwire); or the audit could not run: unsupported toolchain, modified programs, bad output, timeout |
| Errors (N error(s)) | an error diagnostic or a fatal stop; no audit |
| ServerUnavailable (Lean not started) | the server could not start (for example the job limit); retried |
| ServerCrashed (Server crashed) | the server died; shows attempts and whether it is restarting |

The final status is chosen in this order: Errors, Unverified, Partially checked, Incomplete, Relies on compiled code, Checked with axioms, Checked.

**Audit (two steps):**
1. Runs after Lean reports a revision error-free (or speculatively 800 ms after a stable edit, only if its import lines match a version Lean already finished; a failed speculative audit reruns when Lean is done), with the toolchain's `lean`, its library first on `LEAN_PATH` plus the project path from `lake env`, in `%TEMP%\exposition-lean\audit`; nothing written to the project.
2. **Compile** (runs the file's code): elaborates the exact text as a temporary module, writes its `.olean`, reports from frontend snapshots whether elaboration reached the end; only the one line tagged with a per-run secret (sent on stdin, then closed) counts.
3. **Check** (runs none of the file's code): imports only toolchain modules, reads the module and its non-toolchain imports as `.olean` data, refuses non-toolchain `prelude` modules and kernel-special names, replays every non-toolchain dependency into the kernel (`Kernel.Environment.replay`), lists each declaration's axioms with toolchain origin and prints each checked declaration's statement with the toolchain's notation only.
4. **Tripwire:** names written after declaration keywords that the audit lacks are looked up in the language server's `workspace/symbol` index; any hit → Not verified.
5. Both programs are compiled before the first language server for a toolchain starts, SHA-256-pinned, held open with write/delete denied for the service's lifetime (Windows) and re-hashed around each step (tampering → Not verified, recompiled next time); written for Lean 4.35 APIs; 15 min and 32 MiB log per step.

- **`lean_check`** (agents, e2e): target = open document or path (disk text as a temporary document); `checked` only for Checked of that exact revision; diagnostics, `sorry` locations, axioms and goals (≤ 32 positions) only when finished; timeout 120 s default, 600 s max (agent tool: 300 s).
- **Formal evidence (M7):** the service keeps the audit record of each finished revision (`lean_audit_record`: every audited declaration with its statement, axioms, and its own verdict from those axioms (Checked, Checked with axioms, Incomplete, Relies on compiled code) next to the file's status). `lean_record_evidence(document, declarations)` registers service-owned `FormalCheck` evidence per listed declaration (file and declaration verdict, toolchain, sha256 of the audited text, revision, `dependencies: unavailable` "the Lean audit does not report declaration dependencies", no fingerprints, so freshness is never current); names the audit did not list are refused. The declaration is a `FormalDeclaration` entity (key = qualified name, bound to the `.lean` file, `anchoredIn` it). `lean_associate_declaration({declaration: {kind: "audited", document, name} | {kind: "entity", entity}, target, note})` records the reviewed `formalizes` relation (accepted: a correspondence, not a proof of equivalence). `lean_declaration_evidence(document)` lists the evidence history with freshness against the current text (older audits are stale history). The infoview offers Record evidence and Associate with... on checked and audited declarations.
- **Error codes** (19): `lean.` + `invalidDocument`, `invalidRequest`, `stopped`, `closed`, `cancelled`, `timeout`, `serverStopped`, `server`, `unknownDocument`, `noWorkspace`, `stale`, `notReady`, `invalidOffset`, `toolchainInstalled`, `invalidToolchain`, `untrusted`, `noElan`, `notLean`, `unreadable`.

## 6. Agents

### 6.1 Provider and model
| Item | Value |
|---|---|
| Providers | `anthropic` (Messages API, streaming); `scripted` (deterministic, compiled only in debug builds; a stored `scripted` reads as `anthropic` when not allowed) |
| Settings (`ProviderSettings`) | `provider` (default `anthropic`) · `model` free text, trimmed, 1–100 chars `[A-Za-z0-9.-_:@]`, default `claude-opus-5-5` · `effort` `low`/`medium`/`high`/`xhigh`/`max`, default `medium` · `consent` default for new tasks, default `scopeContent` |
| Storage | JSON in `agents.sqlite3` table `settings`, key `provider`; never the key; corrupt JSON → defaults. Each task copies `{provider, model, effort}` at creation; Continue reuses it |
| Request | `POST https://api.anthropic.com/v1/messages`; `x-api-key`, `anthropic-version: 2026-10-21`, `anthropic-beta: server-side-fallback-2026-07-01` (+ body `fallbacks:"default"`) only for models starting `claude-opus-5`, `claude-fable-5`, `claude-mythos-5`, `claude-sonnet-5-5`; UA `Exposition/0.1.0`; adaptive summarized thinking, `output_config.effort`, strict tools with eager input streaming, `tool_choice:auto`, prompt caching; `max_tokens` = min(remaining output budget, 64 000) |
| Status / network | Redirects never followed (3xx → "rejected the request"); connect timeout 30 s, idle read 300 s; 4 retries (1 s·2^n + jitter, ≤30 s; 429 `retry-after` ≤120 s) for rate-limit, overloaded, 5xx, network; never for auth/invalid/protocol/cancel. Webview sees `ProviderStatus {settings, configured, credentialStore, credentialError, scriptedAvailable}`; `providerChanged` event after every settings/key change |

### 6.2 Task lifecycle and statuses
- **Create** (`CreateTaskRequest {prompt, workspaceRoot, scope, permission, execution, consent?, budgets?}`): prompt trimmed, 1–100 000 chars; folder absolute, existing, not a drive root, and the open project; budgets in range; files scope non-empty; selection document open at that revision → `queued` → first user message = `<task_context>` (project, scope, document changes, process execution, consent, open in-scope documents with revisions) + `<selection>` only with `scopeContent` + prompt → worker starts.
- **Run loop**: `running` → checkpoint once (hash, revision, unsaved flag of every open document) → per turn: budget check → stream → store assistant content → run each tool call in order → all results in one user message → repeat until end of turn, budget, cancel, error or stop.
- **Continue** (follow-up prompt, optional new budgets; UI sends none): any inactive task; refused while active (`agents.taskRunning`) or while the worker still releases services (`agents.taskFinishing`); folder re-checked; unanswered `tool_use` blocks get their recorded result or "Not executed: the previous run stopped before this tool ran."
- **Cancel**: aborts the stream, an approval wait, or the running tool (tool gets 5 s to report). Worker end: terminal status + notice stored → agent research sessions shut down → `TaskChanged`.

| Status | UI label · meaning | Status | UI label · meaning |
|---|---|---|---|
| `queued` | Queued · created/continued, not started | `running` | Agent working · loop active |
| `awaitingApproval` | Waiting for approval · blocked on the user | `completed` | Task completed · `end_turn`/`stop_sequence`, no tool calls |
| `canceled` | Canceled · user cancel; changes stay reviewable | `budgetExceeded{budget, detail}` | Stopped: {turn/output-token/input-token/time/tool-call/command} budget |
| `refused{category?, explanation?}` | Declined by provider · `stop_reason: refusal`; nothing from it stored or run | `failed{message}` | Task failed · provider, storage, checkpoint or folder error |
| `stopped{stopReason, message}` | Stopped: response cut off / context window full / response paused / unexpected response · `max_tokens` (per-turn 64 000), `model_context_window_exceeded`, `pause_turn`, unknown stop; its calls `notRun` | `interrupted` | Interrupted · app closed while active; set at next start |

### 6.3 Tools (12; strict schemas, every property required, unknown fields rejected)
| Tool | Parameters | Effect | Declared effects (exposure · launch · service) |
|---|---|---|---|
| `list_files` | `path` ("" = root), `recursive` | In-scope entries, breadth-first, ≤500 (`truncated`); skips `.git`, `node_modules`, `target`, `.venv`, `__pycache__`, `.lake`; no recursion into links; open docs get `openRevision`, `unsavedChanges` | metadata · none · — |
| `read_file` | `path` | Opens the document; returns `path, revision, unsavedChanges, contentHash, text` (buffer incl. unsaved; ≤512 KiB; disk ≤4 MiB); remembers the revision (last 48) | scoped content · none · — |
| `edit_document` | `path`, `base_revision`, `edits[{old_text,new_text}]` (≥1) | Each `old_text` exactly once in a revision the agent read, no overlaps, no no-ops; one transaction (origin Agent, `RebaseDisjoint`, new undo group, "Agent edit to {path}") on the open buffer, unsaved; overlap with user edits → error + `currentRevision` | metadata · none · — (changes) |
| `create_file` | `path`, `content` | Creates through the workspace service (never overwrites, makes folders), opens it; recorded as Created | metadata · none · — (changes) |
| `build_document` | `entry`, `engine` (`auto`/`pdflatex`/`lualatex`) | `.typ` Typst, `.md`/`.markdown` Markdown print profile, in-process (reads confined to scope unless whole project); `.tex`/`.ltx` TeX service full build (`auto` = project default target's engine if entry matches, else pdfLaTeX); other ext → error. Returns status, pages, durationMs, ≤40 diagnostics with line/column, `diagnosticsOmitted`, LaTeX `passes` | Typst/MD: scoped content · none · typesetting; LaTeX: unconfined · native-only · typesetting |
| `insert_artifact` | `path`, `run_id`, `artifact_id`, `after_text`, `caption`, `update_key` | Research artifact as format-owned figure/table markup in a `.typ`/`.tex`/`.md` document (`after_text` unique, "" = end; `update_key` replaces an earlier insertion); returns `revision, attachmentKey, warnings`; artifact file must be in scope | metadata · none · research (changes) |
| `run_python` | `code` | Ask mode: agent-owned research session `agent-{task}` (never the user's); stdout/stderr tail 16 KiB, displays, error with last 12 traceback lines, artifacts, provenance `.exposition/runs/{run}.json`. Restricted: Docker job `python -` with code on stdin | unconfined · native or restricted · research |
| `check_lean` | `path` | Lean service check (project toolchain, `lake serve`), 300 s; report + note; error result unless `checked` | unconfined · native-only · lean |
| `graph_query` | `query` (`search`/`entity`/`neighborhood`/`inspect`/`impact`), `text`, `kinds[]`, `entity`, `path`, `depth` 1–3, `limit` 1–100 | Bounded reads of the project graph filtered to the task (§6.14); never returns, traverses through, or text-matches invisible records; result ≤64 KiB with `graph {project, revision, freshness}` | consent-filtered · none · graph |
| `graph_propose` | `summary`, `base_revision`, `operations[]` (one strict object per `op`: `create_entity`, `add_relation`, `update_entity`, `set_lifecycle`, `remove_relation`, `bind_source`; `""`/`[]`/`-1` for unused fields) | Validated by the graph service as `Origin::Agent{task}`; applied as one graph change (agent relations `suggested`); returns `applied, revision, created[{tempId,id}], delta, effects`; invalid → `agents.graphInvalid` + diagnostics (messages only with `scopeContent`); conflicting → `agents.graphConflict` | consent-filtered · none · graph (changes; bound paths scope-checked) |
| `workflow_propose` | `workflow_id` ("" = new, or `wf-…` for a new revision), `expected_revision` (-1 = none), `title`, `description`, `preset`, `inputs[]` (`id, label, port_type, path` "" = unset, `pinned_hash`), `steps[]` (`id, label, operation` (registered), `version`, `parameters[{name, kind, text, integer, boolean}]`, `payload` none/code/prompt), `connections[]` (data/prerequisite/annotation), `outcomes[]`, `ceiling {execution, model_requests, retain_results, propose_changes}` | Validated by `validate_workflow` (bounded by the contract's limits) and saved by `GraphService::save_workflow` as `Origin::Agent{task}` (a `Workflow` entity); incomplete drafts are saved with diagnostics; returns `saved, workflow, revision, entity, state, errors, warnings, diagnostics, runnable:false, runnableReason`; never executes anything | consent-filtered · none · graph (changes; input paths scope-checked; whole project only) |
| `run_command` | `program`, `args[]`, `working_directory` ("" = root) | Execution job, no shell, empty environment, 10 min timeout, 256 KiB log/stream, 16 KiB tail/stream in result; label "Agent: {program} {args}" ≤120 chars; restricted note: project read-only at `/workspace`, `/out` kept in job folder | unconfined · native or restricted · execution |

Process launches (count against `maxCommands`): LaTeX build, `run_python`, `check_lean`, `run_command`. Invalid streamed JSON → error result "not valid JSON", not counted. Results to the model are JSON (`{"error", "code"?}` on failure); transcript copy ≤4000 bytes.

### 6.4 Central gate (`tools/exec.rs`; applied to declared effects, in order; refusals carry a stable `code`)
| # | Rule | Refusal code(s) |
|---|---|---|
| 1 | Task folder is the open project | plain error (no code) |
| 2 | A change in a `readOnly` task | `agents.readOnly` |
| 3 | Each named path (and an inserted artifact's file) parses and its links resolve; requested and resolved path (links, junctions, case/8.3 aliases) in scope; resolved path inside the project | `agents.invalidPath`, `agents.outOfScope`, `agents.outsideProject` |
| 4 | Unconfined tools need a whole-project scope; scoped or unconfined content needs `scopeContent` | `agents.wholeProjectOnly`, `agents.metadataOnly` |
| 5 | Launch with `readOnly` or execution `disabled`; native-only tool in a `restricted` task | `agents.executionDisabled`, `agents.nativeNotAllowed` |
| 6 | Backing service present | `agents.serviceUnavailable` |
| 7 | Restricted launch needs a configured Docker image (never downgraded); Ask launch needs a trusted folder, then approval of the full payload | `agents.restrictedUnavailable`, `agents.nativeRefused` |
| 3a | Path policy: the graph's own storage is never a file-tool target; run records, generated artifacts, and other `.exposition/` metadata (except `.exposition/research/` and `project.json`) are never written or created | `agents.graphStorage`, `agents.serviceOwned` |
| 6a | The project graph is usable (attached or temporary), of the task's folder, with the identity recorded with the task; every target path is not pending reconciliation (`coverage = pending`, binding `missing`) or ambiguous — after a bounded wait (3 s) for reconciliation | `agents.graphUnavailable`, `agents.projectChanged`, `agents.graphPending`, `agents.graphAmbiguous` (calls recorded `notRun`) |
| 7a | `graph_propose`: every referenced entity/relation/bound file visible to the task; narrower scopes bind new entities to a file in scope; no investigations/workflows; the graph validator accepts the operations (approval card = every effect + every operation) | `agents.outOfScope`, `agents.graphReservedKind`, `agents.graphInvalid` |
| 7b | `workflow_propose`: whole-project task; the definition is well formed; its authority ceiling fits the task's (execution: disabled → none, restricted → restricted, ask-each → trusted native; model requests only with `scopeContent`; retain/propose only when not read-only); nothing wired into a service-owned result (rule 6); approval card = title, validation summary + every diagnostic, "not runnable in this version", the complete definition | `agents.wholeProjectOnly`, `agents.workflowInvalid`, `agents.workflowCeiling`, `agents.workflowServiceOwned` |
| 8 | After approval: same project folder still open; gate re-run yields the identical payload | `agents.projectChanged`, `agents.changedSinceApproval` |

| Tool | Execution off | Ask before each | Restricted (Docker) | Scope and consent |
|---|---|---|---|---|
| `list_files`, `read_file` | allowed | allowed | allowed | path in scope; read needs `scopeContent` |
| `edit_document`, `create_file`, `insert_artifact` | allowed¹ | allowed¹ | allowed¹ | path (and artifact file) in scope |
| `build_document` Typst/Markdown | allowed | allowed | allowed | entry in scope; build reads only scope; `scopeContent` |
| `build_document` LaTeX, `check_lean` | refused | trusted folder + approval | refused (`nativeNotAllowed`) | whole project + `scopeContent` |
| `run_python`, `run_command` | refused | trusted folder + approval | unattended in container | whole project + `scopeContent`; working folder in scope |

¹ `readOnly`: refused; `reviewEdits`: approval first; `autoApply`: applied directly. Process tools in a `readOnly` task: `agents.executionDisabled`. Verified by `tests/central_gate.rs` (every tool × 3 modes × 3 profiles × 3 scopes × 2 consents, with the project graph attached) and `tests/graph_agents.rs` (`graph_query`/`graph_propose` × the same matrix; `graph_query` is allowed in every mode, an `inspect` path must be in scope; `graph_propose` follows the document-change column).

### 6.5 Task settings
| Setting | Values (composer label) | Meaning |
|---|---|---|
| Permission | `reviewEdits` (Ask before each change, default) · `autoApply` (Apply changes, review afterwards) · `readOnly` (Read only) | how documents may change |
| Execution | `disabled` (No commands, default) · `approveEach` (Ask before each command (runs natively)) · `restricted` (Restricted sandbox only (unattended)) | how processes may run |
| Scope | `workspace` (Whole project) · `document{path}` (One document, defaults to active editor) · `files{paths}` (Selected files and folders; exact path or under a folder) · `selection{path, range, revision}` (IPC only, not in composer) | paths the agent may use |
| Consent | `scopeContent` (File contents in scope) · `metadataOnly` (File names only) | what content may reach the provider |

### 6.6 Budgets (`agents.invalidBudgets` when out of range)
| Budget | Default | Min–max | Enforced |
|---|---|---|---|
| `maxTurns` | 40 | 1–500 | before each request |
| `maxOutputTokens` | 400 000 | 1 024–20 000 000 | before each request (<1 024 left); budget-limited `max_tokens` stop → `budgetExceeded` |
| `maxInputTokens` | 4 000 000 | 1 024–200 000 000 | before each request (input + cache read + cache write); not editable in UI |
| `maxWallMs` | 1 800 000 | 1 000–86 400 000 | races stream and tools; approval waits excluded; UI field "Minutes" |
| `maxToolCalls` | 150 | 0–5 000 | per call; rest of the batch `notRun` |
| `maxCommands` | 20 | 0–1 000 | per process launch |

### 6.7 Approvals
- Raised for native launches (Ask mode) and changes in `reviewEdits`: `ApprovalRequest {id "approval-{uuid}", taskId, toolUseId, tool, title, detail}`; memory only; no timeout. Detail is the complete payload (nothing truncated); invisible and direction-changing characters shown as `⟨U+XXXX⟩` with a note; native launches end "Runs natively with your account's privileges."
- Titles: "Apply edits to {path}" (each replacement −/+), "Create {path}" (bytes + content), "Insert artifact {a} of run {r} into {path}" / "Update attachment {key} in {path} with artifact {a} of run {r}" (placement + exact text + key), "Build {entry} with LaTeX", "Run Python code", "Check {path} with Lean", "Run {program}" (Program / Arguments / Working folder).
- Approve → `confirm` re-runs the gate (rule 8; revoked trust → `nativeRefused`); a refused call is recorded `failed` with its code; an insertion applies exactly the shown text or nothing. Decline → `declined`, loop continues. Cancel while waiting → `ApprovalResolved(false)`, remaining calls `notRun`, task `canceled`. Unknown id → `agents.noApproval`.

### 6.8 Tool-call states
| State | Label · meaning | State | Label · meaning |
|---|---|---|---|
| `pending` | Pending · in this response; an earlier call runs first | `running` | Running |
| `awaitingApproval` | Waiting for approval · has not run | `succeeded` | Done |
| `failed` | Failed (Unavailable if "not available in this build") · error result, incl. gate refusals | `declined` | Declined · user declined ("Do not retry it unchanged.") |
| `canceled` | Canceled · running when the task was canceled or its time ran out; result says how far | `notRun` | Not run · budget reached, response cut off, task stopped first, or app restarted |

### 6.9 Review, accept, revert
- Per file: checkpoint text, baseline (file without this task's changes; concurrent user edits rebased in), tip; `entangled` when a concurrent edit overlapped. States: `pending` (Pending review), `accepted` (Kept), `reverted` (Reverted), `undone` (Undone in editor: derived from current content, never stored; edited change absent or created file missing; redo makes it pending again; an undone revert shows pending).
- Keep / Keep all: refused while active (`agents.taskRunning`); all = effective-pending files only; reverted → "already reverted"; undone → "nothing to keep"; else "Kept."
- Revert / Revert all: refused while active; task folder must be open (`agents.workspaceNotOpen`); edited file: inverse diff as undoable transaction "Revert agent changes" (rebased over later edits, ≤3 retries), refused on overlap or entangled → "Reverted in the open document (not saved)."; created file: deleted only if buffer and disk hold only agent text → "Deleted the file the agent created."
- Diff: read-only Monaco diff, left = without this task's changes, right = after its latest change; side-by-side, inline below 700 px.

### 6.10 Persistence and crash behaviour
- `<data dir>/agents.sqlite3` (WAL, foreign keys, 5 s busy timeout, schema v2; newer schema refused). Tables: `tasks`, `transcript` (full provider history incl. file text read), `tool_calls`, `checkpoints`, `task_files`, `changes` (patches + inverse), `settings`; v2 adds `task_graph` (bound project graph id, kept investigation), `task_graph_contexts` (one per run), `task_targets`, `task_proposals`. No delete or prune: tasks kept indefinitely. Startup: active tasks → `interrupted`; `pending`/`running`/`awaitingApproval` calls → `notRun`; warning notice "Exposition closed while this task was running…". Pending approvals are lost (memory only). Continue resumes.
- Events: `agents:event` (13 kinds: taskChanged, transcriptChanged, streamStarted, textDelta, thinkingDelta, streamReset, toolCallStarted, toolCallFinished, approvalRequested, approvalResolved, fallback, reviewChanged, providerChanged); broadcast capacity 4096; lag → `agents:resync`.

### 6.11 Credentials
- Windows: generic credential target `anthropic-api-key.org.exposition.desktop` (user `anthropic-api-key`), persistence local machine (not roaming). Persistence is never changed in place (Credential Manager would keep the old copy in its other store and bring it back on the next write of that persistence by any program): a roaming copy is deleted before the local one is written, and every save, move, and removal is checked after both stores reload (a throwaway roaming and a throwaway local credential `<target>.settle-roaming` / `.settle-local`, no secret, written and deleted at once), up to 4 attempts. A key saved with roaming persistence by an earlier version is moved to local-machine on first read; one moved in place by an earlier build has its leftover roaming copy removed on first read, keeping the local bytes. macOS Keychain / Linux keyutils via `keyring` (unverified). Debug builds: entry `org.exposition.desktop.development` unless `EXPOSITION_CREDENTIAL_NAMESPACE` (≤64 `[A-Za-z0-9-]`) or `EXPOSITION_USE_REAL_CREDENTIAL=1`; release builds ignore both and use the real entry.
- Key: trimmed, 16–512 ASCII graphic chars (IPC rejects >1 024 bytes); never returned, logged, stored in SQLite or project files; webview clears the field before saving (restored only on failure). Agent jobs start from an empty environment; provider errors replace the key with `<redacted>`; process output and text artifacts pass the execution redactor, which matches the stored key by value (registered when read or saved, removed when cleared) as well as by token shape. Trusted native code runs as the user and could read the stored key.

### 6.12 Agent error codes (22 + graph)
- Provider/storage: `agents.storage` (SQLite/store or serialization), `agents.credentials` (credential store read/write/delete failed), `agents.notConfigured` (no API key), `agents.provider` (HTTP client creation failed), `agents.invalidModel`, `agents.invalidKey` (fails validation or > 1 024 bytes), `agents.scriptedUnavailable` (scripted provider in release/disabled), `agents.invalidScript` (dev script invalid or > 1 MiB), `agents.noScript` (no dev script queued).
- Task requests: `agents.emptyPrompt`, `agents.promptTooLong` (> 100 000 chars), `agents.invalidWorkspace` (not absolute/existing/folder, or a root), `agents.workspaceNotOpen` (task folder not the open project), `agents.invalidBudgets`, `agents.invalidScope` (empty files scope; selection document not open or bad range), `agents.staleSelection`, `agents.invalidTask` (id not `task-[A-Za-z0-9-]` ≤ 64), `agents.invalidApproval` (approval id > 128 chars).
- Lifecycle: `agents.taskRunning` (continue/accept/revert while active), `agents.taskFinishing` (continue while the worker releases services), `agents.noApproval` (approval no longer pending), `agents.notFound` (unknown task, or file not changed by it).
- Project graph: `agents.graphRequired` (service configured with a workspace but no graph), `agents.graphUnavailable`, `agents.graphPending`, `agents.graphAmbiguous`, `agents.graphStorage`, `agents.serviceOwned`, `agents.graphInvalid`, `agents.graphConflict`, `agents.graphReservedKind`, `agents.workflowInvalid`, `agents.workflowCeiling`, `agents.workflowServiceOwned`; `agents.projectChanged` also when the folder's graph identity is not the task's.

### 6.13 UI surfaces
- **Agent view** (right panel, icon Bot): toolbar New task / Tasks / provider settings; provider chip "Claude · {model}" / "Claude · no API key configured" / "Claude · credential store error" / "Scripted provider — development only"; error banner "Agent action failed" (Dismiss). **Task list**: newest first; title, status pill, "{n} to review", relative time; empty "No agent tasks yet".
- **Composer**: Request; project folder (or "No project folder is open" + Open Folder…); Scope; Document changes; Commands and code; Content sent to the model provider; collapsed Budgets (Model turns, Tool calls, Commands, Output tokens, Minutes); Cancel / Start task (enabled with prompt, open folder, complete scope, ready provider).
- **Task view**: Back, title, status pill, Cancel (active), "Review changes (N)", spec line (permission · execution · consent · model/effort · served by), outcome banner, approval cards, live transcript (You, Agent, Reasoning summary, tool calls with summary/state/input/result, notices, streaming draft "Thinking" / "Running: {tool}" / "Waiting for approval: {tool}"), budget meter (6 bars), Follow-up + Continue task.
- **Approval card**: banner with title, "Nothing is applied until you approve…", "{lines} · {bytes} · complete, nothing omitted", scrollable payload (a Tab stop only when it scrolls), Approve / Decline. **Attention**: status-bar item (right) "Agent: waiting for approval" / "Agent: N waiting for approval" button → oldest pending approval; else "Agent: N running"; else "Agent: N to review". Persistent notification "Agent needs your approval" ("{title} · Task: {task}", action Open approval), not shown when the card is visible, retired on answer/task end. Opening focuses the card. Finished notifications: completed (success, files to review), budgetExceeded/stopped/interrupted (warning), failed/refused (danger), canceled (none).
- **Palette:** 4 commands in category Agent, no keybindings (§2); Open Pending Agent Approval is enabled only while a task waits and focuses the first pending card.
- **Review**: Keep all, Revert all, Open in editor area (editor `agents-review:{id}`, "Review: {title}"), "The task is still running" banner (actions disabled), per-file rows (state pill, "Overlapping edits", "N changes" / "created by the agent", "· edited since", Keep, Revert), results "Done"/"Refused", diff.
- **Agents page** (top-level page `agents`, order 30, Mod+3): task list (filters All/Running/Needs you/Kept) and "Kept in the project graph" (investigations, workflow definitions); the selected task's header (status, Cancel, Keep as investigation, Show in Agent panel) and tabs Run (the shared task view: approvals, transcript, budgets, follow-up), Graph context, Proposals, Changes (review). Same store as the Agent view: one task list, one approval answerable from either; the panel's task view links here ("Open in Agents page"); a pending approval opens where the user is.
- **Provider dialog** "Agent provider": Provider select; key status + "stored in {store}"; credential error banner; API key / Replace API key + Save key; Remove (confirm "Remove the API key?" Keep key / Remove key); Model; Effort; Default content consent; Close / Save settings. Settings › Agents shows provider, model, key status, "Provider settings…".

### 6.14 Project graph (M7)
- **Mandatory context**: create and continue attach a scoped graph context (refused with the graph codes when the graph cannot be used): project id + revision, freshness, in-scope pending files, coverage, counts by kind, ≤160 entities (`id, kind, roles, label?, path, lifecycle, revision, origin kind, source version, binding, coverage, evidence kind+freshness`), ≤300 relations among them (`kind, status, origin kind, note?`), authorized operations; ≤48 KiB; `truncated` when summarized. Whole project: project entity, research/mathematical entities first, then files. Narrower scopes: files in scope, entities whose every binding is in scope, relations whose both endpoints are listed; unanchored entities only for whole-project tasks. Metadata-only: labels of anything but files/folders/project, notes, attributes, reasons, diagnostics withheld. Sent as `<graph_context>` JSON (`<`/`>` escaped), hidden from the transcript, recorded with the task; the first context binds the task to that graph identity.
- **Targets**: paths of `read_file`, `edit_document`, `create_file`, `build_document`, `insert_artifact`, `check_lean` resolve to resource identities (`ensure_resource`; generated outputs and excluded files such as logs and run records stay operational references, never admitted); results carry `graph {resource, admitted?, revision, freshness, note?}`; `create_file` journals a provisional identity (`CrossService` operation) before writing and completes it after admission (failed admission → needs reconciliation). Targets are recorded per task (uses: read, edit, create, build, check, insert). `insert_artifact` also records the attachment like a reviewed promotion (`ResearchProject::record_external_attachment`: artifact retained with computational evidence, `usesAsset`, journaled, followed through undo/redo); `graph.attachment.state` is `recorded`, `pendingReconciliation` (code `agents.graphPending`, plus a `graphNote`), or `notRecorded`.
- **Workflow drafts** (`workflow_propose`): saving is a deliberate admission of a `Workflow` entity, gated like a graph change (read-only refused, review-edits approval, auto-apply direct); drafts are never runnable in this version and nothing runs; forged service-owned outputs and ceilings above the task's are refused and nothing is saved; listed under "Kept in the project graph" on the Agents page.
- **Research space**: the context lists the project's research space (whole-project tasks; it is not bound to a file) with its `research.path` attribute even under metadata-only consent (a path); other attributes only with `scopeContent`.
- **Keep as investigation** (`agents_task_keep_investigation`, stopped tasks): the user's admission of one `investigation` entity (attributes `agentTask`, `taskStatus`; `references` to the task's admitted targets) plus `review` evidence referencing `agentTask:{id}`; idempotent.
- **Project generation** (`agents_task_create_generation {objective, workspaceRoot, permission, execution?, consent?, budgets?}`): an ordinary whole-project task, execution disabled unless given, whose request is the objective plus a generation brief; TS helper `startProjectGeneration` (`src/features/agents/generation.ts`).

## 7. Settings

`<data>` = `EXPOSITION_DATA_DIR`, else `%LOCALAPPDATA%\org.exposition.desktop`. Settings editor sections: Appearance, Editor, Project, Trusted folders, Execution, Agents, Research, About.

### App (`<data>/settings.json`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `version` | u32 | 1 | — (must be 1) |
| `files.reopenLastProject` | bool | false | Settings › Project › Startup "Reopen the last project" |

### Editor (same file, `editor.*`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `editor.fontSize` | u32 (8–40) | 13 | Settings › Editor › Font size (10–24 px list) |
| `editor.tabSize` | u32 (1–16) | 2 | Settings › Editor › Tab size (2, 3, 4, 8) |
| `editor.insertSpaces` | bool | true | Settings › Editor › Indent with spaces |
| `editor.wordWrap` | bool | true | Settings › Editor › Wrap long lines |
| `editor.lineNumbers` | bool | true | Settings › Editor › Line numbers |
| `editor.minimap` | bool | false | Settings › Editor › Minimap |
| `editor.defaultEol` | `lf` \| `crlf` | `lf` | Settings › Editor › Line ending for new lines (files without line endings only) |

- Missing file → defaults; missing fields → defaults; invalid file → copied to `settings.json.invalid-<ms>` (original removed only if the copy verifies) and defaults used; startup notification "Invalid application settings were reset". Writes atomic (fsync + rename). Recent folders live separately in `<data>/recent-projects.json` (max 12; Remove from Recent).
- Reopen last project: once per application start (sessionStorage flag survives webview reloads); skipped if a folder is already open; failure → "Could not reopen <name>".

### Project (`<project>/.exposition/project.json`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `version` | u32 | 1 | — (must be 1) |
| `entry` | workspace path \| null | null | — (used by `tex_build` with no entry when no default target) |
| `targets[]` | `{name, format, engine, entry}` | [] | Settings › Project › LaTeX targets (Add target…, Edit…, Remove, per-row Engine select); palette `latex.*` |
| `defaultTarget` | target name \| null | null | Settings › Project › Default target (None or any target) |

- Rules: names trimmed, 1–64 chars, unique; pairs typst/typst, latex/pdfLatex, latex/luaLatex, markdown/markdownPrint; default must name a target. UI adds: main file relative, inside the folder, valid Windows name, ends `.tex`. A `.tex` file without a target builds with pdfLaTeX. Typst/Markdown targets are kept but not editable in the UI.
- Invalid file → `project.json.invalid-<ms>` beside it, defaults, banner in Settings › Project + notification (also via the workspace event when the file changes). No folder open → "No folder open" state.

### Execution (`<data>/execution/settings.json`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `toolPaths` | map tool id → absolute path | {} | Settings › Execution › Tool locations (Set…/Change…, Reset, Re-check tools) |
| `docker.image` | string \| null | null | Settings › Execution › Container image (local images only; Clear, Reload images) |
| `docker.memory` | string | `2g` | — (file only; ≥6m) |
| `docker.cpus` | string | `2` | — (file only; 0.01–1024) |
| `docker.pidsLimit` | u32 | 256 | — (file only; 1–4194304) |
| `docker.user` | `uid:gid` | `65534:65534` | — (file only; numeric, non-root) |
| `maxConcurrentJobs` | u32 \| null | null (= CPUs/2 clamped 2–8) | — (file only; 1–64) |

- Webview can change only `dockerImage` and `toolPath`. Image: Docker reference grammar, ≤512 chars; presence not required to save (missing shows "Not present locally"). Tool path: catalog tool (15 overridable: pdflatex, lualatex, latexmk, biber, bibtex, synctex, python, rscript, r, julia, lean, lake, elan, docker, git; kernel components follow their interpreter), absolute, no `..`, existing file, `.exe` on Windows (`.bat`/`.cmd`/`cmd.exe`/`.ps1` refused), passes the launch gate, not inside an open project or `<data>/execution`; re-checked at every use.
- Load: unparsable file or invalid limits/user/job limit → each bad value replaced by its default, original kept as `settings.json.invalid-<ms>`, corrected file written; if the backup fails the original stays until a backup succeeds. Banner in Settings › Execution + startup notification "Invalid execution settings were reset". Writes atomic (fsync + rename).

### Provider (`<data>/agents.sqlite3`, table `settings`, key `provider`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `provider` | `anthropic` \| `scripted` | `anthropic` | Agent provider dialog › Provider (scripted only in debug builds; stored scripted reads as anthropic in release) |
| `model` | string (1–100 of `A-Za-z0-9 . - _ : @`) | `claude-opus-5-5` | Agent provider dialog › Model |
| `effort` | `low` \| `medium` \| `high` \| `xhigh` \| `max` | `medium` | Agent provider dialog › Effort |
| `consent` | `metadataOnly` \| `scopeContent` | `scopeContent` | Agent provider dialog › Default content consent |
| API key (secret) | 16–512 printable ASCII | none | Agent provider dialog › Save key / Remove; Windows Credential Manager `anthropic-api-key.org.exposition.desktop`, local-machine persistence; debug builds use `….development` unless `EXPOSITION_CREDENTIAL_NAMESPACE` or `EXPOSITION_USE_REAL_CREDENTIAL=1`; never returned to the webview |

- Corrupt stored JSON → defaults. Settings › Agents shows a read-only summary (provider, model, key Configured / Not configured, credential error) and "Provider settings…".

### Trust (`<data>/execution/trust.json`)
| Key | Type | Default | UI location |
|---|---|---|---|
| `version` | u32 | 1 | — |
| `roots` | list of canonical folder paths | [] | Settings › Trusted folders (list, Refresh, Revoke…); granted only via the "Trust this workspace?" dialog |
| `granted_at_ms` | map path → ms | {} | Shown as "Trusted <date>" / "Trusted (date not recorded)" |

- Entries show "Folder no longer exists" and "Ignored: too broad" pills; "What can be trusted" lists personal and system folders. Unreadable file → nothing trusted. Writes atomic.

### Frontend-only state
| Key | Storage | Default | UI location |
|---|---|---|---|
| `exposition.theme` | localStorage | `"system"` | Settings › Appearance › Color theme; Manage menu; theme palette commands |
| `exposition.workbench.layout` | localStorage | sidebar 260 px, panel 240 px, auxiliary 320 px (all visible), split 50 % | Drag/keyboard resize, toggles, View: Reset Layout |
| `exposition.startupReopenAttempted` | sessionStorage | absent | — (one reopen attempt per application start) |

In-memory only: Jobs "Show tool probes", selected Settings section (default Appearance), Files tree expansion, job output buffers.

## 8. IPC endpoints (webview → core)

Manifest `src-tauri/build.rs` (`COMMANDS` 90 release, `DEV_COMMANDS` 3 debug-only); handlers `src-tauri/src/lib.rs` `generate_handler!` (93 = 90 + 3 `#[cfg(debug_assertions)]`); grants `src-tauri/capabilities/default.json` (capability `main-window`, window `main`: exactly the 90 `allow-*` plus `core:event:default`, `core:window:allow-set-title`, `core:window:allow-destroy`, `dialog:allow-open`). Args are camelCase over IPC. Errors: `CommandError{code,message}`; document commands and `page_edit` reject with `TransactionError{kind}`; Lean commands with `LeanError{code,message,currentRevision}`. † = no UI caller (e2e or agent path only).
### App and documents (6)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| app_info | — | AppInfo | Name, version, OS, arch, debug flag, data dir, Typst compiler, Markdown profile |
| document_list | — | DocumentInfo[] | Documents open in the document service |
| document_state | document: DocumentId | DocumentState | Full text and info of one open document |
| document_apply | request: TransactionRequest | TransactionOutcome | Apply revisioned edit; refused e.g. `mixedLineEndings`, `readOnly`, `conflict`, `staleRevision` |
| document_undo | document: DocumentId, origin: EditOrigin | DocumentChanged | Undo one step of the shared history (`nothingToUndo`) |
| document_redo | document: DocumentId, origin: EditOrigin | DocumentChanged | Redo one step of the shared history |
### Workspace, files, recovery, settings (20)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| workspace_open | path: String (absolute) | WorkspaceInfo | Open folder, register execution root; refuses too-broad, data folder, admin-share aliases |
| workspace_close | — | () | Close folder; unregister execution root |
| workspace_current | — | WorkspaceInfo \| null | Open folder: root, name, pending recovery entries |
| workspace_recent | — | RecentProject[] | Recent folders |
| workspace_forget_recent | path: String | RecentProject[] | Remove one entry (Remove from Recent) |
| workspace_list_dir | path: WorkspacePath \| null | FileEntry[] | List one folder; dirs first; hides `.git`, `.lake`, `node_modules`, `target` |
| document_open | path: WorkspacePath | DocumentOpened | Open file as document (same physical file → same document); refuses binary, undecodable, >10 MiB |
| document_close | document: DocumentId | () | Close; discard unsaved changes and their recovery journal |
| document_save | document: DocumentId, overwrite: bool | DocumentInfo | Atomic save; `file.modifiedOnDisk` unless overwrite |
| document_save_all | — | SaveAllOutcome | Save every dirty document; failures listed per file |
| document_reload | document: DocumentId | DocumentInfo | Replace buffer with disk text (undoable `external` edit) |
| document_keep_buffer | document: DocumentId | () | Resolve disk conflict keeping the buffer |
| document_normalize_line_endings | document: DocumentId, lineEnding: LineEnding | TransactionOutcome \| null | Normalize to LF or CRLF (else `document.invalidLineEnding`); null if unchanged |
| recovery_list | — | RecoveryEntry[] | Recovery journal entries of the open folder |
| recovery_restore | id: String | RestoreOutcome | Restore journaled unsaved text (`recovery.notFound`/`unreadable`/`bufferDirty`/`io`) |
| recovery_discard | id: String | RecoveryEntry[] | Delete one recovery entry |
| settings_get | — | AppSettingsState | App settings plus load warning |
| settings_update | settings: AppSettings | AppSettings | Write app settings (`settings.invalid`, `settings.writeFailed`) |
| project_settings_get | — | ProjectSettingsState | `.exposition/project.json` plus exists/warning; invalid file set aside |
| project_settings_update | settings: ProjectSettings | ProjectSettings | Save targets/default target; rebuilds watched LaTeX entries at once |
### Execution, trust, execution settings (12)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| job_list | — | JobSummary[] | Retained jobs |
| job_output | job: JobId, after: u64?, maxBytes: u32? | OutputPage | Output chunks after seq; default 64 KiB, clamped to 1 MiB |
| job_cancel | job: JobId | bool | Cancel job process tree; false if already finished |
| toolchains_list | — | ToolchainInfo[] | Cached toolchain detection |
| toolchains_refresh | — | ToolchainInfo[] | Re-detect toolchains |
| trust_status | root: String (≤4096, no NUL) | TrustStatus | Trust state of a folder (`trusted`, `trustedVia`) |
| trust_grant | root: String | TrustStatus | Grant trust (UI: trust dialog only); refuses too-broad, data folder, aliases |
| trust_revoke | root: String | TrustStatus | Revoke; first stops the folder's trusted processes (bounded wait) |
| execution_settings_get | — | ExecutionSettingsState | Tool locations, Docker settings, overridable tools, load warning |
| execution_settings_update | change: ExecutionSettingsChange | ExecutionSettingsState | One validated change: `dockerImage{image?}` or `toolPath{tool,path?}` |
| docker_images_list | — | DockerImage[] | Local tagged images via `docker image ls`; never pulls |
| trust_list | — | TrustList | Trusted folders with grant times, plus trust rules |
### Lean (9)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| lean_documents | — | LeanDocumentState[] | Status and diagnostics of open Lean documents |
| lean_projects † | — | LeanProjectInfo[] | Detected Lean projects with toolchain/server state |
| lean_goals | document: DocumentId, revision: u64, offset: u32 | LeanGoals | Goals at UTF-16 offset of exactly that revision (`lean.stale`) |
| lean_hover | document: DocumentId, revision: u64, offset: u32 | LeanHover \| null | Hover Markdown at exact revision |
| lean_definition | document: DocumentId, revision: u64, offset: u32 | LeanLocation[] | Go-to-definition targets |
| lean_restart | document: DocumentId | () | Restart project server; re-checks trust and toolchain |
| lean_restart_file | document: DocumentId | () | Re-elaborate file, rebuilding out-of-date imports |
| lean_install_toolchain | document: DocumentId | JobId | Install project's pinned toolchain via elan (trusted folder only) |
| lean_check † | request: LeanCheckRequest | LeanCheckReport | Check exact text, audited verdict; ≤32 goal positions; timeout ≤600 s (default 120 s) |
### LaTeX build service (5, all †; the UI uses `typesetting_*`/`page_*`)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| tex_build † | target: BuildTarget?, mode: TexBuildMode | TexTargetRef | Queue build (default project target); coalesces; newer cancels running |
| tex_status † | target: BuildTarget? | TexTargetStatus \| null | Running, pending, last, lastGood, stale of a target |
| tex_cancel † | target: BuildTarget? | bool | Cancel running build and drop pending request |
| tex_page_image † | build: BuildId (`tex-…`), page: u32 (<10000), scale: f32 | PNG bytes | Page raster, capped at 32 Mi pixels; open-folder builds only |
| tex_page_map † | build: BuildId, page: u32 | TexPageMapping | Glyph-level mapping of one page |
### Agents (16)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| agents_provider_status | — | ProviderStatus | Settings, key configured, credential store name/error, scripted available |
| agents_provider_update | settings: ProviderSettings | ProviderStatus | Save provider, model, effort, default consent; emits `providerChanged` |
| agents_provider_set_key | key: String (≤1024) | ProviderStatus | Store API key in Credential Manager; never returned |
| agents_provider_clear_key | — | ProviderStatus | Delete the stored key |
| agents_task_list | — | TaskSummary[] | All tasks |
| agents_task_detail | task: TaskId | TaskDetail | Summary, transcript, pending approvals, graph record (context, targets, proposals, investigation) |
| agents_task_create | request: CreateTaskRequest | TaskSummary | Validate and start a task (prompt ≤100,000 chars, open folder only) |
| agents_task_continue | task: TaskId, prompt: String, budgets: Budgets? | TaskSummary | Follow-up on a stopped task, optional new budgets |
| agents_task_cancel | task: TaskId | TaskSummary | Cancel the stream or running tool |
| agents_approval_resolve | task: TaskId, approval: String (≤128), approved: bool | () | Approve or deny a pending tool call (`agents.noApproval`) |
| agents_review | task: TaskId | TaskReview | Checkpoint and changed files with review state |
| agents_review_diff | task: TaskId, path: WorkspacePath | FileDiff | Original, modified, checkpoint texts of one file |
| agents_review_revert | task: TaskId, path: WorkspacePath? | ReviewActionResult[] | Revert one or all files; `agents.taskRunning` while active |
| agents_review_accept | task: TaskId, path: WorkspacePath? | ReviewActionResult[] | Accept one or all pending files; `agents.taskRunning` while active |
| agents_task_keep_investigation | task: TaskId | EntityId | Keep a stopped task as an investigation in the project graph (idempotent) |
| agents_task_create_generation | request: GenerationTaskRequest | TaskSummary | Start a new project's generation task (whole project, execution disabled by default) |
### Research and artifact attachment (14)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| research_workspace | — | WorkspaceStatus \| null | Open folder root and trust (synced from workspace service) |
| research_environments | refresh: bool | EnvironmentReport | Discovered Python, R, Julia environments |
| research_session_start | request: ResearchStartRequest{environmentId} | SessionInfo | Start kernel; owner forced to user; trusted folder; ≤8 user sessions |
| research_sessions | — | SessionInfo[] | All sessions |
| research_session_interrupt | sessionId: String | SessionInfo | Interrupt the kernel |
| research_session_restart | sessionId: String | SessionInfo | Restart the kernel |
| research_session_shutdown | sessionId: String | () | Stop the session |
| research_execute | request: ResearchExecuteRequest{sessionId, source: RunSource} | RunSummary | Queue run (input, selection, or file); requester forced to user |
| research_run_cancel | runId: String | () | Cancel a run |
| research_runs | — | RunSummary[] | Runs of the open folder |
| research_run | runId: String | RunManifest | Stored manifest `.exposition/runs/<id>.json` |
| research_run_output | runId: String, after: u32?, limit: u32? (200) | RunOutputPage | Output entries after seq |
| research_artifact | runId: String, artifactId: String | raw bytes | Artifact bytes on demand |
| artifact_attach | request: AttachRunArtifactRequest | AttachArtifactResult | Insert/update a run's figure or table in an open document |
### Typesetting and exact-page editing (10)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| typesetting_watch | entry: WorkspacePath | BuildEvent \| null | Build now and on each change; `.tex` via TeX (draft mode) |
| typesetting_unwatch | entry: WorkspacePath | () | One view stops watching (per-view count) |
| typesetting_build | entry: WorkspacePath | BuildEvent | Build now and wait; `.tex` full mode, ≤600 s |
| page_image | renderRef: String (1–128 `[A-Za-z0-9_-]`) | raw bytes (SVG/PNG) | Rendered page of an open-folder build |
| typesetting_export_pdf | build: String, destination: String? | ExportArtifact | Export PDF (default next to entry); `tex-` ids go to TeX |
| page_hit_test | build: String, page: u32, x: f64, y: f64 | PageHit | Point (pt from top-left) → caret or non-editable reason |
| page_caret | build: String, path: WorkspacePath, offset: u32, affinity: Affinity | CaretGeometry \| null | Caret geometry at a UTF-16 offset |
| page_navigate | build, path, offset, affinity, motion: Motion, goalX: f64? | NavigationResult \| null | Caret motion on the page |
| page_selection | build: String, path: WorkspacePath, start: u32, end: u32 | SelectionGeometry | Highlight rects and rendered text of a range |
| page_edit | request: PageEditRequest | PageEditOutcome | Insert, delete back/forward, paragraph break; origin `page` |
### Dev-only (3; debug builds only, granted by runtime capabilities `dev-execution-probe`, `dev-agents-scripted`)
| Command | Args | Returns | Purpose |
|---|---|---|---|
| dev_start_probe_job † (dev) | — | JobId | Fixed Python process tree for cancellation evidence |
| dev_start_workspace_job † (dev) | root: String, holdSeconds: u32? (≤600) | JobId | Register root; trust-gated job writes `exposition-dev-ran.txt` |
| agents_dev_load_script † (dev) | script: JSON (≤1 MiB) | () | Queue a script for the scripted provider |

Typesetting rules: in-process targets by extension only (`.typ` Typst, `.md`/`.markdown` Markdown print profile; anything else `typesetting.unsupported`). Builds, pages, maps and exports are served only for the folder open now. Export destination: an absolute `.pdf` path in an existing folder; an existing file there must be a regular file starting `%PDF-`, not a build input, not an open document (`typesetting.export`). Not called by the UI (release): `lean_projects`, `lean_check`, `tex_build`, `tex_status`, `tex_cancel`, `tex_page_image`, `tex_page_map` (7).

## 9. Events (core → webview)

16 channels; forwarders read broadcast channels and emit `*:resync` on lag (workspace instead re-emits `filesChanged{changes:[], rescan:true}`; trust emits `null`).

| Channel | Payload variants | When |
|---|---|---|
| `documents:event` | `DocumentEvent`: `changed` (`DocumentChanged{document, transaction?, origin, change: edit\|undo\|redo, previousRevision, revision, patches, selection?, canUndo, canRedo}`), `opened{info}`, `closed{document}`, `infoChanged{info}` | Every document-service change, in revision order |
| `documents:resync` | `()` | Lag; editor re-reads attached documents |
| `workspace:event` | `WorkspaceEvent`: `opened{info}`, `closed`, `filesChanged{changes: {path, kind: created\|removed\|modified}[], rescan}`, `documentReloaded{document, path}`, `conflict{conflict: {document, path, kind: modified\|deleted\|unreadable, message}}`, `conflictResolved{document}`, `recoveryChanged{entries}`, `journalFailed{path, message}`, `projectSettingsInvalid{message}` | Folder open/close, watcher batches, auto-reload, disk conflicts, recovery changes, journal failure, invalid `project.json` set aside |
| `jobs:event` | `JobEvent`: `queued{id, label}`, `started{id, pid?}`, `output{id, stream: stdout\|stderr, text}`, `cancelRequested{id}`, `finished{result}` | Every job lifecycle step, in order (`finished` last, also for refusals) |
| `jobs:resync` | `()` | Lag; re-read job list |
| `trust:changed` | `TrustChanged{root, granted: bool}` or `null` | Any trust grant/revoke; before emitting, watched LaTeX entries rebuild if the change covers the open folder |
| `lean:event` | `LeanEvent`: `document{state: LeanDocumentState}`, `removed{document}` | Lean document status/diagnostics change; document no longer tracked |
| `lean:resync` | `()` | Lag |
| `tex:event` | `TexEvent`: `queued{target, mode}`, `started{target, build, mode, automatic}`, `finished{target, summary: TexBuildSummary}` | Every LaTeX build step (no UI listener) |
| `tex:resync` | `()` | Lag (no UI listener) |
| `typesetting:build` | `BuildEvent{result, timings, lastGood?, refusal?}` | Finished build of a watched Typst/Markdown entry; finished non-canceled TeX build |
| `typesetting:status` | `BuildStatusEvent{entry, activity: queued\|building\|idle}` | Entry queued, build started, build ended (TeX: `queued` if more pending) |
| `agents:event` | `AgentEvent`: `taskChanged{task}`, `transcriptChanged{taskId}`, `streamStarted{taskId, turn}`, `textDelta{taskId, turn, text}`, `thinkingDelta{taskId, turn, text}`, `streamReset{taskId, turn, reason}`, `toolCallStarted{taskId, toolUseId, name}`, `toolCallFinished{taskId, toolUseId, isError}`, `approvalRequested{approval}`, `approvalResolved{taskId, approvalId, approved}`, `fallback{taskId, from, to}`, `reviewChanged{taskId}`, `providerChanged{status}` | Task status/usage, transcript, streaming, tool calls, approvals, model fallback, review, provider settings or key changed (never the key) |
| `agents:resync` | `()` | Lag |
| `research:event` | `ResearchEvent`: `sessionChanged{session}`, `sessionRemoved{sessionId}`, `runQueued{run}`, `runStarted{runId, sessionId, executionCount?}`, `output{runId, entry}`, `runFinished{run}` | Session and run lifecycle, bounded run output |
| `research:resync` | `()` | Lag; also after workspace `opened`/`closed` |

Listeners (`src/lib/ipc.ts` `events`): documents → editor, page, agents; workspace → workspace, editor, agents; jobs → jobs; trust → page, research; lean → lean; typesetting:build → page, typesetting; typesetting:status → typesetting; agents → agents; research → research. The window's close request (Tauri built-in) is handled by the editor's unsaved-changes guard. Backend-internal (no webview event): TeX and typesetting rebuild watched entries on `changed` documents and `filesChanged`; workspace `closed` forgets watched entries (typesetting also drops its retained builds; TeX builds of a closed folder are no longer served).

## 10. Connections and processes

| Network endpoint (the only one in app code) | Request | Rules |
|---|---|---|
| `https://api.anthropic.com/v1/messages` (agents) | `POST`, SSE; headers `x-api-key`, `anthropic-version: 2026-10-21`, `anthropic-beta: server-side-fallback-2026-07-01` (model ids starting `claude-opus-5`, `claude-fable-5`, `claude-mythos-5`, `claude-sonnet-5-5`), `content-type: application/json`, `accept: text/event-stream`, `User-Agent: Exposition/<version>` | base URL fixed; connect 30 s, idle read 300 s; redirects never followed (3xx → error); system/env proxy honoured; reqwest `default-tls` |

- **Network by external programs, not app code:** `elan toolchain install` (explicit, confirmed, trusted) downloads Lean toolchains; trusted native code (kernels, approved commands/Python, LaTeX, Lake) has the user's network access; a TeX distribution's on-the-fly package install is not addressed in code.
- **Never contacted (code-backed):** no telemetry, analytics, crash reporting or updater (only plugin `tauri-plugin-dialog`); CSP `connect-src ipc: http://ipc.localhost`; Monaco, workers, fonts bundled; Typst packages only from local `typst/packages` dirs (`typst-kit` without downloads); Markdown refuses URL images; editor links copied, never opened; no runtime/package installs; Docker images never pulled; containers `--network=none`.

**Docker** (local engine via the `docker` CLI; client runs in a job object with only `DOCKER_HOST`, `DOCKER_CONTEXT`, `DOCKER_CONFIG`, `DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY`, cwd `<data>/execution/probe`)

| When | Invocation | Timeout |
|---|---|---|
| Settings › Execution image list | `docker image ls --format {{json .}}` | 30 s |
| App start (background, only if an image is configured); retried before the first restricted job | `docker ps -aq --filter label=org.exposition.instance=<FNV-1a-64 of lowercased <data>/execution>`, then `docker rm -f <hex id>` each | 20 s each |
| Each restricted job | preflight `docker image inspect --format {{.Id}} <image>` (20 s), then `docker run --rm --pull=never --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --pids-limit=<256> --memory=<2g> --memory-swap=<same> --cpus=<2> --user=<65534:65534> --init --hostname=sandbox --tmpfs=/tmp:rw,nosuid,nodev,size=256m --mount=type=bind,source=<root>,target=/workspace,readonly --mount=type=bind,source=<job>/out,target=/out [--mount=…<job>/in,target=/exposition-input,readonly] --workdir=/workspace/<rel> --env-file=<job>/container.env --name=exposition-<job> --label=org.exposition.{managed=1,job=<job>,instance=<instance>} --log-driver=none -i --entrypoint=/bin/sh <image> -c <supervisor script> exposition-job <program> <args…>` | job timeout |
| Cancel / timeout | `docker rm -f exposition-<job>` (tests/diagnostics also `docker ps -aq --filter name=^exposition-<job>$`) | 20 s |

**External programs** (all via the shared execution service: job object, constructed env, argument array, no shell)

| Program and arguments | When | Policy, timeout |
|---|---|---|
| 18 catalog tools (pdflatex, lualatex, latexmk, biber, bibtex, synctex, python, python-jupyter, Rscript, R, irkernel, julia, ijulia, lean, lake, elan, docker, git): `--version` (latexmk `-v`, synctex `help`, python `-E -c …`, docker `version --format …`, IRkernel/IJulia package checks) | Toolchains view load/refresh, one per located tool | Probe (no trust), 90 s |
| `pdflatex`/`lualatex -synctex=1 -interaction=nonstopmode -file-line-error -no-shell-escape -recorder -output-directory=<out> <entry>`; `--version` (cached) | LaTeX build passes and reruns | TrustedNative, 180 s (version 20 s) |
| `bibtex <job>`; `biber --input-directory <mirror> --input-directory <project> <job>` | bibliography pass when needed | TrustedNative, 180 s |
| `py.exe -0p`; `python -c <probe>` (project `.venv`/`venv` only when trusted: `-I -c`); `Rscript --vanilla -e <probe>`; `julia --startup-file=no --history-file=no -e <probe>` | Research environment discovery | Probe / project job |
| `python -u -X utf8 <data>/research/kernel-host/0.1.0-<sha12>/exposition_kernel_host.py` | Research session start | TrustedNative, interactive |
| helper children: Python kernel `<python> <helper.py> --ipykernel -f {connection_file} --HistoryManager.enabled=False`; IRkernel/IJulia argv from `kernel.json`; `taskkill.exe /PID <pid> /T /F` on restart | Session run / restart | inside the helper's kill-on-close job |
| `<toolchain>/bin/lake serve` (Lake project) or `lean --server` (standalone); `lean --version` (60 s) | Lean file opened in a trusted folder | TrustedNative, no timeout |
| `lean -R <dir> -o <olean> <src>` ×2 (Compile, Check programs, once per toolchain and session); `lake env` (Lake projects) | Lean audit preparation | TrustedNative, 5 min |
| `lean --run RunCompile.lean <src> <olean> <module>` (secret on stdin); `lean --run RunCheck.lean <tag> <lib> <module>` | Lean audit steps 1 and 2 | TrustedNative, 15 min |
| `elan toolchain install <qualified name>` | explicit "Install toolchain" | TrustedNative |
| approved program + args; container `python -` (code from `/exposition-input/stdin`) | agent `run_command` / `run_python` | TrustedNative after approval or Restricted, 10 min |

Not launched: latexmk, makeindex, kpsewhich, synctex CLI, Typst CLI, any rasterizer (Typst, Markdown and PDF rendering run in-process); `exposition-testproc` is a test fixture.

| IPC channel | Transport and rules |
|---|---|
| Webview ↔ core | Tauri IPC (`ipc:` / `http://ipc.localhost`) + events; ACL-gated (§8–9) |
| Research supervisor ↔ kernel host | stdio, one JSON object per line; protocol version 1 handshake; protocol stdout reserved by fd dup; malformed lines ignored |
| Kernel host ↔ Jupyter kernel | ZeroMQ, jupyter_client default (TCP 127.0.0.1), HMAC-signed; CurveZMQ only for Python kernels when pyzmq, jupyter_client and ipykernel support it, else `transportEncryption: "none"` |
| Lean server; Lean audit | LSP JSON-RPC over stdio, Content-Length framing, header ≤ 8 KiB, body ≤ 64 MiB, no widgets/RPC; audit results read from stdout lines tagged with a per-run secret/nonce |

| OS facility | Use |
|---|---|
| Windows Credential Manager | API key, generic credential `anthropic-api-key.org.exposition.desktop` (debug default `….development`), `CRED_PERSIST_LOCAL_MACHINE` (roaming entries moved on first read: roaming copy deleted, local copy written); Keychain/keyutils via `keyring` elsewhere (unverified) |
| Job Objects | every child: `KILL_ON_JOB_CLOSE`, `DIE_ON_UNHANDLED_EXCEPTION`, no breakaway; `CREATE_SUSPENDED \| CREATE_NO_WINDOW` → assign → resume; tree killed when the primary exits; app exit runs `tex.shutdown()` + `execution.shutdown()` (≤ 5 s); crash → handle close kills trees |
| File watching | `notify` (ReadDirectoryChangesW), recursive on the open root; batch handled 150 ms after its first event; journals throttled to 300 ms |
| WebView2 / dialog | Tauri defaults, no extra browser args; window `main` 1440×900, min 720×480, drag-drop off; native folder picker via `dialog:allow-open` |

## 11. Data and storage

`<data>` = `EXPOSITION_DATA_DIR` if non-empty, else `app_local_data_dir()` (Windows `%LOCALAPPDATA%\org.exposition.desktop`); never a project, never trustable.

```
<data>/settings.json            AppSettings; invalid → kept as settings.json.invalid-<ms> (original removed only after a byte-identical copy), defaults + one warning
<data>/recent-projects.json     {version:1, projects:[{path,name,openedAt}]}, ≤12, newest first (+ .invalid-<ms>)
<data>/agents.sqlite3 (-wal, -shm)   tables tasks, transcript, tool_calls, checkpoints, task_files, changes, settings
<data>/recovery/<wskey>/        live-<pkey>.json (dirty buffer), rec-<pkey>-<ms>.json (earlier session), bad-<pkey>-<ms>.json + .reason (quarantined); keys = 16 hex of SHA-256
<data>/execution/               settings.json (+ .tmp, .invalid-<ms>; bad values reset with warning), trust.json (+ .tmp), probe/, cache/par/ (PAR_GLOBAL_TEMP)
<data>/execution/jobs/<job>/    tmp/ (removed), out/ (/out, kept if non-empty), container.env (removed), in/ (restricted batch input, removed when the job ends)
<data>/tex/<sha16(root)>/       mirror-<sha16>/ (project copy), <key>/out/ (aux, log, pdf, synctex.gz), <key>/builds/<build-id>/ (frozen; 4 kept)
<data>/research/                kernel-host/0.1.0-<sha12>/exposition_kernel_host.py, runtime/ (connection files, HMAC keys), probe/
<data>/EBWebView/               WebView2 profile (Tauri default)
%TEMP%\exposition-lean\audit\   runs/<id>/ (removed after each audit), tools-<session>/ (removed when the service drops)
```

- **Project-side:** `.exposition/project.json` (+ `.invalid-<ms>`, `ProjectSettingsInvalid` event); `.exposition/runs/<run-id>.json`; `.exposition/artifacts/<run-id>/output-N.{png,jpg,gif,svg,html,csv}` + `transcript.json` (text artifacts redacted).
- **Project-side, transient or tool-owned:** `.<name>.exposition-save-<hex>.tmp` (atomic save, hidden); exported `<stem>.pdf` via `<stem>.pdf.<pid>.tmp`; `.lake/` (Lake); `exposition-dev-ran.txt` (debug dev job).
- **Browser storage:** localStorage `exposition.theme`, `exposition.workbench.layout`; sessionStorage `exposition.startupReopenAttempted`; never document content or secrets. Credential: OS store (§10).

| Environment variable(s) read | Purpose | Build |
|---|---|---|
| `EXPOSITION_DATA_DIR`; `EXPOSITION_TOOL_CACHE_DIR` | data dir override; shared tool cache (`<dir>/par`) | all |
| `EXPOSITION_CREDENTIAL_NAMESPACE` (≤64 `[A-Za-z0-9-]`); `EXPOSITION_USE_REAL_CREDENTIAL` (`1`) | credential entry `org.exposition.desktop.<ns>`; opt into the real entry | **debug only** |
| `PATH`, `APPDATA`, `LOCALAPPDATA`, `USERPROFILE`, `ProgramFiles`, `PROGRAMDATA`, `SystemRoot`, `ELAN_HOME`, `HOME` | tool, environment and elan discovery (host `PATH` never passed on) | all |
| `SystemDrive`, `PUBLIC`, `TEMP`, `TMP`, `OneDrive`, `windir`, `ProgramFiles(x86)`, `ProgramW6432`, `ProgramData`, `ALLUSERSPROFILE` (+ above; Unix `TMPDIR`) | too-broad folder rules; `TEMP`/`TMP` also Lean scratch | all |
| `DOCKER_HOST`, `DOCKER_CONTEXT`, `DOCKER_CONFIG`, `DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY` | docker client only | all |
| all host variables | credential-named values become redaction literals; 28-name base list copied to children (`SystemRoot`, `windir`, `SystemDrive`, `ComSpec`, `PATHEXT`, `OS`, `PROCESSOR_ARCHITECTURE`, `PROCESSOR_IDENTIFIER`, `PROCESSOR_LEVEL`, `PROCESSOR_REVISION`, `NUMBER_OF_PROCESSORS`, `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles`, `ProgramFiles(x86)`, `ProgramW6432`, `CommonProgramFiles`, `CommonProgramFiles(x86)`, `CommonProgramW6432`, `PUBLIC`, `ALLUSERSPROFILE`, `USERNAME`, `USERDOMAIN`, `COMPUTERNAME`) | all |
| `HTTP(S)_PROXY`, `ALL_PROXY`, `NO_PROXY` | provider proxy (reqwest, implicit) | all |
| `PROFILE`; `CARGO_PKG_VERSION` | dev commands in ACL manifest; version string | build time |
| `EXPOSITION_E2E_EXECUTABLE`, `WEBVIEW2_USER_DATA_FOLDER`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, `EXPOSITION_DEV_HOME`, `EXPOSITION_REQUIRE_LEAN`, `ELAN_HOME` (e2e); `EXPOSITION_REQUIRE_TEX`, `EXPOSITION_TEX_BIN`, `EXPOSITION_REQUIRE_RESEARCH_PYTHON`, `EXPOSITION_REQUIRE_RESEARCH_KERNELS`, `EXPOSITION_PROBE_DIR`, `EXPOSITION_TEST_DOCKER_IMAGE`, `EXPOSITION_TEST_DOCKER_PYTHON_IMAGE`, `EXPOSITION_TEST_HOST_ONLY`, `EXPOSITION_LIVE_ANTHROPIC_KEY`, `EXPOSITION_LIVE_MODEL`, `SWEEP_VERBOSE` (cargo tests) | test harnesses | test only |

Set for children: job `TEMP`/`TMP`, constructed `PATH`, `PAR_GLOBAL_TEMP`; research `PYTHONUTF8`, `PYTHONIOENCODING`, `PYTHONUNBUFFERED`, `PYTHONDONTWRITEBYTECODE`, `PYDEVD_DISABLE_FILE_VALIDATION`, `JUPYTER_RUNTIME_DIR`, `R_LIBS_USER`/`JULIA_DEPOT_PATH`; TeX `TEXINPUTS`, `BIBINPUTS`, `BSTINPUTS`, `max_print_line`, `error_line`, `half_error_line`, `openout_any=p`, `openin_any=p`, `shell_escape=f`; Lean audit `LEAN_PATH`; containers `PATH`, `HOME=/tmp`, `TMPDIR=/tmp`, `LANG=C.UTF-8`.

## 12. Security boundaries

- **CSP:** `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; worker-src 'self' blob:; connect-src ipc: http://ipc.localhost; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'`.
- **ACL:** one capability `main-window` (window `main`): `core:event:default`, `core:window:allow-set-title`, `core:window:allow-destroy`, `dialog:allow-open` + 90 `allow-<command>`; no `core:default`, no fs/shell/http/opener plugin; debug builds add `dev-execution-probe` and `dev-agents-scripted` at runtime.
- **Check-permissions gate:** `npm run check` fails when any `src-tauri/permissions/autogenerated/*.toml` matches no `COMMANDS`/`DEV_COMMANDS` entry (now 93 files = 90 + 3); the same pipeline runs the private-path guard and bindings drift check.
- **IPC boundary:** no command runs an arbitrary program; ids, sizes, page indexes, coordinates validated; `WorkspacePath` rejects absolute, `..`, reserved names and invalid characters during deserialization.
- **Trust model:** trust lives in `<data>/execution/trust.json`, never in the project; opening never runs code or grants trust; native jobs (TeX, kernels, Lean, commands) need a trusted folder, re-checked at spawn; revoke cancels the folder's jobs (≤10 s each) and broadcasts `trust:changed` to every view.
- **Too-broad folders:** drive/share roots, personal folders and their ancestors, system folders and anything inside them cannot be opened or trusted; compared by canonical text and by file identity (volume serial + file index), so `\\localhost\C$\…`, `\\127.0.0.1\C$\…`, `\\?\UNC\localhost\C$\…`, `\\localhost\ADMIN$` aliases are refused.
- **Data folder:** never a project (is, inside, or contains `<data>` → `workspace.tooBroad`) and never trustable (nor anything above or inside it), by name and identity.
- **Tool-location re-check:** a configured tool path inside an open project or the execution data folder (name or identity) is refused when set and at every use (jobs, probes, TeX, Docker, sweep) → `RefusedOverride` / `programRefused`.
- **Program validation:** absolute path or catalog id; Windows runs `.exe` only (`.bat`, `.cmd`, `cmd.exe` refused); no `..`; argument arrays, no shell; NUL refused.
- **Environment scrubbing:** child env built from empty (28-name base list, constructed `PATH`, job allowlist); credential-like names or secret-looking values make the job fail (`environmentRefused`), never silently filtered.
- **Restricted backend:** Docker only; explicit, locally present image checked by one reference grammar in settings and at launch; flags in §10; settings refuse root `uid:gid`, invalid memory/cpus/pids; never a native fallback.
- **Container lifetime:** batch jobs hold stdin as a liveness pipe, so the container kills itself when the app exits or crashes; in-container `sleep <limit+5 s> && kill -9`; `--rm`; cancel/timeout `docker rm -f`; leftovers of this data folder swept at startup and before the first restricted job.
- **Redaction:** job output, events and summaries redact literal values of credential-named host variables (≥8 chars) and token shapes (`sk-ant-`, `sk-`, `gh[pousr]_`, `github_pat_`, `glpat-`, `npm_`, `hf_`, `xox?-`, `AKIA|ASIA`, `AIza`, `bearer`, PEM keys) to `[REDACTED]`; kernel stream/display/error output redacted; text artifacts (HTML, SVG, CSV, JSON) redacted and rewritten before registration; provider errors replace the key with `<redacted>`.
- **Credentials:** the key lives only in the OS store, kept machine-local on Windows; the webview learns only "configured"; it never enters child envs, the database or logs; validation errors never echo it; debug builds use a `development` entry unless told otherwise; output redaction matches it by its value (registered when read or saved) and by token shape; it is sent only to the fixed endpoint, redirects never followed; code trusted to run as the user can still read it.
- **Project path rules:** reads and writes canonicalize and must stay inside the root (`path.outsideWorkspace`, `path.brokenLink`); aliases (case, 8.3, links, hard links) map to one document; link-following listing stays inside the root; resolve-then-act is not atomic (TOCTOU).
- **Saves:** temp file + fsync + rename, original untouched on failure; refused if the disk hash changed (unless Overwrite), read-only, a folder, too large, or binary; rename breaks hard links, so a file outside the folder is never written.
- **Rendering:** Typst pages as SVG `<img>`; kernel HTML parsed inert with handlers neutralized; Lean hovers untrusted (no HTML, no command links), goals plain text; editor links copied, never opened.
- **TeX:** builds require trust; `-no-shell-escape`, `shell_escape=f`, `openout_any=p`; reads are not confined, hence the trust requirement; the engine writes only to the app-owned mirror/out dirs; rendered pages capped at 32 Mi pixels.
- **Builds and exports:** builds of a folder no longer open are never served or exported; export refuses an existing destination that is not a file or not a PDF, a source, an open document, or a build input.
- **Research:** sessions need trust and are bound to one root; only discovered environment ids launch; project venvs probed only when trusted (`-I`); the folder reaches R/Julia as bytes, never spliced into code; webview sessions are owner `User`; interrupt/restart/shutdown/cancel have no owner check (the user may stop agent sessions).
- **Lean:** server, audit and install run TrustedNative; elan shims never run; no implicit downloads; audit step 2 runs none of the file's code; audit programs SHA-256-pinned and held open deny-write; outputs tagged with per-run secret/nonce; name tripwire against the server's index.
- **Agents:** one central gate checks each tool call's declared reads/changes/runs against scope, consent, profile, execution mode and budgets before anything happens (§6).

## 13. Dev/test-only features

**Compiled out of release builds** (`cfg(debug_assertions)`; ACL entries only when `PROFILE=debug`)

| Feature | Where | Notes |
|---|---|---|
| `dev_start_probe_job` | `src-tauri/src/execution.rs` | "Dev probe: process tree": `python -E -u -c` spawns a sleeping grandchild, 400 burst lines, ticks every 0.5 s; Application kind, 10 min |
| `dev_start_workspace_job(root, holdSeconds?)` | same | registers `root` directly, Batch python writes `exposition-dev-ran.txt`; `holdSeconds` ≤600 adds a two-process tree; trust still required |
| `agents_dev_load_script` (≤1 MiB) | `src-tauri/src/agents.rs` | loads a scripted provider turn queue |
| Runtime capabilities `dev-execution-probe`, `dev-agents-scripted` | `execution::add_dev_capability`, `agents::add_dev_capability` | added in `setup` |
| Scripted provider (`provider::scripted`, `allow_scripted`) | `crates/exposition-agents` | release stub → `agents.scriptedUnavailable`; UI shows "Scripted provider — development only" only when `scriptedAvailable` |
| Credential namespace | `src-tauri/src/state.rs` | debug default entry `org.exposition.desktop.development`; `EXPOSITION_CREDENTIAL_NAMESPACE`; `EXPOSITION_USE_REAL_CREDENTIAL=1`; release always uses the real entry |
| Console window | `src-tauri/src/main.rs` | release sets `windows_subsystem = "windows"` |
| "(debug)" / "(debug build)" labels | status bar, Settings › About | from `AppInfo.debugBuild` |
| Debug-gated test files | `crates/exposition-agents/tests/{central_gate,execution_tools,lean_integration,review_actions,review_content,review_m5,services_integration,stop_reasons,tool_call_states,task_loop,workspace_integration}.rs`, `exposition-markdown/tests/typst_print.rs`, `exposition-typesetting/tests/latency.rs` | use `cfg(debug_assertions)` |

**Present in release although developer-oriented:** `EXPOSITION_DATA_DIR`, `EXPOSITION_TOOL_CACHE_DIR`; palette command "Developer: Show UI Reference" and the activity-bar menu item "UI Reference" (`ui-reference:` component gallery); `data-testid`, `data-offset`, `data-anchor` attributes; test hooks `queueEventForTest`/`flushEvents` (research store), `PageSession.caretPosition()`.

**Test-only code:** `exposition-testproc` binary (never launched by the app); `LocalProcessRunner`, `TexToolchain::discover`, `Fonts::embedded_only`; `crates/exposition-tex/examples/dump_glyphs.rs`; ignored measurements (`typesetting/tests/{dump,latency}.rs`, `tex/tests/latency.rs`, `lean/tests/{lean_audit_probe,review_m4_lean}.rs`); WMI-escape limitation test `execution/tests/review_exec.rs`.

**Checks** (`npm run check`): private paths → permission files → typecheck → vitest → Vite build → rustfmt → clippy `-D warnings` → `cargo test --workspace` → bindings drift (restores the previous bindings if regeneration fails).

**e2e harness** (`npm run e2e [-- <names>]`, `scripts/e2e/run.mjs`): launches `EXPOSITION_E2E_EXECUTABLE` or `target/debug/exposition.exe` with isolated `EXPOSITION_DATA_DIR`, `WEBVIEW2_USER_DATA_FOLDER`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<free port>`; connects with Playwright `connectOverCDP`; raw IPC via `window.__TAURI_INTERNALS__.invoke`; strips `EXPOSITION_USE_REAL_CREDENTIAL` except for `live-*`; artifacts and last-state screenshot/console in `test-results/e2e/<name>/`. Helpers: `agent-scope-check.mjs`, `app.mjs`, `app-env.mjs`, `palette.mjs`, `processes.mjs`, `wait.mjs`, `workspace-helpers.mjs`, `repro/page-race-keystrokes.mjs`.

**32 scenarios** (`scripts/e2e/scenarios/*.mjs`): agents-attention, agents-harness, consistency, execution-jobs, lean-check, live-agents, live-restricted, m6-paper-project, page-latex, page-markdown, page-typst, research-attach-build, research-isolation, research-output, research-python, review-m4-lean-ui, review-m5-agents, review-sec-acl, review-sec-exec, review-sec-ipc, review-sec-render, review-sec-secrets, settings, smoke, tex-build, trust-propagation, ui-shell, workspace-external, workspace-files, workspace-recovery, workspace-save-failure, workspace-window-close.
- Excluded from the default run: `live-agents` and `live-restricted` (runner skips `live-*`; real key, spends API credit). `live-restricted` uses no development-only command, so it also runs against release/installed builds: an unattended task's `run_python` in the Docker sandbox, then the same request refused with no image configured.
- Self-skip unless named: `review-m5-agents`, `review-sec-acl`, `review-sec-exec`, `review-sec-ipc`, `review-sec-render`, `review-sec-secrets`.
- Need a debug build (scripted provider, dev commands or credential namespace): `agents-attention`, `agents-harness`, `execution-jobs`, `review-m5-agents`, `review-sec-exec`, `review-sec-render`, `review-sec-secrets`, `settings`, `trust-propagation`; `m6-paper-project` runs its agent step only in debug builds.
- Report SKIPPED without Lean `leanprover/lean4:v4.35.0` via elan (fail instead with `EXPOSITION_REQUIRE_LEAN=1`): `lean-check`, `review-m4-lean-ui`, the Lean step of `agents-harness`.

## 14. Known limitations

From `docs/capabilities.md` (verified where code allows) plus code findings (marked *code*).
- Windows 11 x64 is the only verified platform; macOS/Linux build but are unverified (Unix process-group containment unverified).
- The NSIS installer (per user, no admin) is not code-signed; SmartScreen warns.
- Page editing refuses generated text: numbering, references, citations, bibliography entries, list markers, footnote marks, line-break hyphens.
- Page editing refuses code/macro output: Typst code expressions, `#let` and function bodies, loops, `#set`, imported modules, `#show` replacements; LaTeX user macros; raw/verbatim text; package content ("not editable here — edit in source").
- Structure-changing page edits (e.g. deleting a `\left`/`\right` delimiter, typing a character that opens markup) are refused, not guessed.
- LaTeX character mapping is inferred and checked glyph by glyph; unmapped text is not page-editable; exotic catcodes and text-rewriting packages (`siunitx`, `cleveref` names, TikZ) stay source-only.
- LaTeX navigation is line level (SyncTeX) outside mapped text.
- pdfLaTeX: typed `"` becomes ` `` `/`''`; non-Latin text and combining marks are LaTeX errors. LuaLaTeX: missing glyphs give one warning per font.
- Mixed-direction lines: caret approximate exactly at a direction boundary.
- Typst content included twice is not detected as duplicated and stays editable in both places.
- If concurrent edits (source editor, agent) make page keystrokes unplaceable, typing pauses with a message; nothing is applied at a guessed position.
- Edit-to-page time (debug, Ryzen 7 9800X3D): Typst ~2–15 ms, Markdown ~16–26 ms, pdfLaTeX ~0.3–0.45 s, LuaLaTeX ~0.8–1 s.
- Markdown math is a declared LaTeX-math subset; unsupported commands show a marker and a diagnostic.
- TeX can read any file the account can read (`openin_any=p` does not confine absolute reads), so LaTeX builds require a trusted folder.
- Typst packages are never downloaded; they must already be in the local package directory.
- R interrupt is unsupported on Windows (IRkernel ignores it); Cancel restarts the kernel.
- Exposition never installs runtimes or packages (needs ipykernel + jupyter_client, IRkernel, IJulia).
- R/Julia kernel messages are HMAC-signed but unencrypted over loopback TCP; Python uses CurveZMQ only when pyzmq, jupyter_client and ipykernel support it, else also unencrypted.
- Research sessions need a trusted folder; switching or closing the folder, or revoking trust, detaches them.
- Lean "Checked" certifies the listed declarations as kernel-checked in the audit's own elaboration, not how the text reads (a file can redefine its syntax). A file written to behave differently under the audit is caught only when it drops a declaration the language server indexes; otherwise the list shows what was checked.
- A trusted Lean file's code runs with the user's privileges while compiled; deliberate tampering with the toolchain or other processes is not contained.
- Lean audit programs target Lean 4.35 APIs; toolchains they do not compile on report Not verified; kernel re-checking large dependencies (Mathlib) is slow.
- Lean goals are plain text; third-party infoview widgets are not executed.
- Agents: one provider (Anthropic Messages API); live behaviour is covered only by the opt-in `live-agents` and `live-restricted` scenarios (spend credit).
- Trusted native code (kernels, approved commands, Python, LaTeX, Lean) runs as the user and can read the stored API key.
- Restricted execution needs Docker with a locally present image providing `/bin/sh` (`sleep` for the in-container time limit); no pull, no native fallback.
- the startup container sweep runs only when an image is configured; a sweep failure is only printed to stderr (the first restricted job retries).
- the liveness pipe covers batch containers only; interactive restricted containers would rely on the time limit alone (the app starts none).
- Processes created through system services (WMI, Task Scheduler, COM) can outlive a job.
- Path resolution and the following file operation are separate steps (TOCTOU against hostile local processes).
- Files with mixed line endings open read-only until normalized.
- Binary, legacy-encoded (not UTF-8 / UTF-16 with BOM) and >10 MiB files are not opened as text.
- Editor links are never opened; they are copied to the clipboard.

## Verification

All counts were read from `c4669a5` (working tree clean); the two later merges (Lean audit hardening, final agents fixes) add no IPC commands, events, palette commands, tools or settings keys.

| Count | Value | How obtained |
|---|---|---|
| Release IPC commands | 90 | `sed -n '/^const COMMANDS/,/^];/p' src-tauri/build.rs \| grep -c '^    "'`; equals the 90 `allow-*` grants in `src-tauri/capabilities/default.json` |
| Dev-only IPC commands | 3 | `DEV_COMMANDS` in `build.rs`; `generate_handler!` in `src-tauri/src/lib.rs` lists 93 (3 under `#[cfg(debug_assertions)]`); `permissions/autogenerated/` holds 93 files |
| §8 rows | 93 | Row names sorted and diffed against `COMMANDS` + `DEV_COMMANDS`: identical |
| Event channels | 16 | `pub const …: &str = "<x>:<y>"` in `src-tauri/src/*.rs` (16) = `listen(` calls in `src/lib/ipc.ts` (16) |
| Event variants | 42 | Distinct `kind` tags in `src/bindings/{Document,Workspace,Job,Lean,Tex,Agent,Research}Event.ts`: 4+9+5+2+3+13+6 |
| Palette commands | 48 | 17 `id:` entries in `builtinCommands` (`src/app/shell/builtins.tsx`) + 31 `registry.registerCommand(` in `src/features/*/register.ts(x)` (agents 4, editor 2, jobs 4, lean 6, research 7, settings 6, workspace 2); 14 `keybinding:` on commands |
| Views / editors / status items | 7 / 7 / 6 | `registry.registerView` / `registerEditor` / `registerStatusItem` calls (non-test sources) |
| Agent tools | 9 | `grep -c 'ToolDefinition {' crates/exposition-agents/src/tools/mod.rs` = 9 tool-name constants |
| Gate refusal codes / agent error codes | 13 / 22 | `pub const` in `pub mod codes` (`tools/exec.rs`); unique `"agents.*"` strings in `service.rs` + `src-tauri/src/agents.rs` |
| Settings keys | 27 | Fields of `AppSettings`/`FilesSettings`/`EditorSettings`, `ProjectSettings`, `ExecutionSettings`/`DockerSettings`, `ProviderSettings`, trust file; frontend keys from `readStored`/`writeStored`/`sessionStorage` callers |
| e2e scenarios | 32 | `ls scripts/e2e/scenarios/*.mjs`; `run.mjs` loads all and skips `live-*` unless named |

**Final acceptance run (main `c8fdca6`, 2026-11-12, Windows 11 x64).** `npm run check` 9/9 (1033 Rust tests, 9 ignored; 231 frontend tests). Debug app: default e2e 24/24 including `m6-paper-project` 16/16; reviewer scenarios by name all pass with no defect (`review-m5-agents`, `review-sec-*`, the M2/M3 reviewer's seven scenarios at `f6404c2`). Release: NSIS installer 26.63 MiB; silent per-user install (`/S /D=…`) in 3.2 s without elevation. Against the installed app: 23 of 26 scenarios pass; the 3 that fail need development-only commands or the scripted provider, which the release refuses by design (`execution-jobs`, `settings`, `review-sec-render`). Scenarios that save or remove a key are never run against a release build (it always uses the real credential entry). Installed `m6-paper-project`: every step passes except the two that need an agent (draft; unattended Docker run), which the live scenarios cover against the installed app with the real provider: `live-restricted` 19/19 (at `c8fdca6`) and `live-agents` 20/20 (at `f6404c2`). Silent uninstall removes the program folder, the uninstall entry and the Start Menu shortcut; Tauri's uninstaller keeps `HKCU\Software\exposition\Exposition` (the install path) and the app data unless its "delete app data" option is chosen.
