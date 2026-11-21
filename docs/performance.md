# Performance standard

This is the standard that performance work in Exposition follows, the measured
state it was derived from, and the budgets a change must keep. It applies to
every later implementation, including a rework of the agent runtime.

Validation (which tests a change must run) is described in
[testing.md](testing.md). This document covers cost: time, memory, bytes moved,
and work counts.

## Rules

1. **Measure first, on the same workload.** A change is a performance change
   only if a benchmark exists before the product code changes, is committed
   alone, and is run before and after on the same host, profile and scale.
   Report `n`, minimum, median and maximum. One run is reported as one run.
2. **Prefer counts to clocks.** A regression test asserts deterministic work:
   bytes read, records decoded, processes started, passes over a buffer, events
   sent, builds started. Wall-clock numbers are evidence in a report, never a
   test threshold.
3. **Cost follows the change.** An edit to one file, one run, one record or one
   page costs work in proportion to that item, not to the project, the history
   or the document. A cost that grows with the whole is a defect to record even
   when it is small today.
4. **Never trade correctness for speed.** A change may not lower a limit, drop
   output, skip a validation, weaken a trust, permission or mathematical check,
   or reuse a test result. Reusing a build artifact is allowed; reusing a
   verdict is not.
5. **Incomplete is said, never implied.** A bounded read that did not see
   everything says so in its result (`…Truncated`, `…Omitted`, `complete`,
   `next…` fields). "Not loaded" must never read as "none": a caller that
   decides something from a list either receives the whole list or a flag that
   forbids the decision.
6. **A cache has an owner and a proof.** Every retained collection names its
   owner, key, invalidation and bound, and has a differential test against the
   uncached computation (randomized where the input space allows). Decisions of
   authorization, capability, consent or trust are never cached. There is no
   general cache layer.
7. **Keep the fallback.** Where an index or incremental path replaces a scan,
   the scan stays as the fallback for inputs the fast path cannot prove, and the
   two are tested for equal output.
8. **Bounded concurrency, declared resources.** Parallel work declares what it
   shares (desktop focus, clipboard, a store, the heavy build slot). Defaults
   are serial; a host opts into more.
9. **Architecture stays put.** A performance change does not replace a
   scheduler, a store format or a service boundary. A fix that needs one is
   written up as a decision with its measured benefit, and waits.

## Measuring

| Command | Measures |
|---|---|
| `npm run perf:rust -- <crate> [--release] [--filter <name>]` | Runs the crate's `tests/perf_*.rs` (`#[ignore = "perf"]`), collects the `PERF {json}` lines they print. |
| `npm run perf:app -- <name>` | Runs `scripts/perf/app/<name>.mjs` against the built app: JS heap, DOM nodes, listeners, process working sets, long tasks. |
| `node scripts/perf/measure.mjs -- <command>` | Times any command over several runs with sampled memory of named processes. |

- `EXPOSITION_PERF_SCALE` selects `small`, `large` or `stress`. Fixtures are
  generated from a fixed seed (`scripts/perf/fixtures*.mjs` and the helpers in
  each `perf_*.rs`); nothing is downloaded.
- Each record carries the commit, profile, scale, CPU, memory and OS. Compare
  only records with the same profile and scale.
- The Rust runner always selects the app crate as well, so a benchmark builds
  with the workspace's dependency features and does not rebuild dependencies.
- Debug-profile numbers show shape (linear or quadratic, how many passes).
  Release-profile numbers are the ones users see. Say which one a number is.
- Benchmarks are not part of `check:*`. The counter tests that guard a fix are
  ordinary tests and are.

A new benchmark goes in `crates/<crate>/tests/perf_<topic>.rs` or
`scripts/perf/app/<name>.mjs`, prints one record per measured operation, and
has no assertion on time.

## Build and test

| Setting | Value | Reason |
|---|---|---|
| Dev profile debug info | line tables only; none for dependencies | Cold build 1201 s → 855 s, peak memory +15.1 → +4.9 GB, build directory 40.4 → 12.4 GB, rebuild after a graph-crate edit 174 → 51 s (one run each, 3 jobs). `CARGO_PROFILE_DEV_DEBUG=full` restores full debug info for a debugger session. |
| Release frontend | `--mode release`: no source maps | The bundle is embedded in the executable; `dist` 33 → 8 MB. `build:release` selects the mode through `src-tauri/tauri.release.conf.json`. |
| Scenario concurrency | `--e2e-jobs 1` by default | See [testing.md](testing.md). Scenarios that use the desktop or the clipboard never overlap. |
| Cargo selection | always include `-p exposition` | A different package selection changes dependency features and rebuilds the tree. |

What a run may reuse: the build directory, the built app, installed
dependencies. What it may not: any check's result. A run records the commit and
input hash it started from and fails as stale (exit 2) if they changed while it
ran.

Known remaining cost: `build:app` with nothing changed still takes about 23 s
because the frontend build rewrites `dist` and the app crate embeds it; the Lean
security sweep (`review_sec2_lean`, about 300 s) is deliberate and is not to be
shortened.

## Budgets

Budgets are stated as work, with the measured time as context. A change that
exceeds a budget needs a recorded reason.

Times below are debug-profile medians measured before and after on the same
host at the `large` scale unless a row says otherwise; release-profile figures for the same benchmarks are in the table at the end of this section.

### Editing and workspace

| Operation | Budget (work) | Before → after |
|---|---|---|
| One typed character, merged undo group | at most 2 copies of the text and 1 bytewise seek to the edit; no line-ending scan | 1 MiB: 40.3 → 2.0 ms; 10 MiB: 398 → 20 ms |
| Undo | proportional to the touched region plus one copy | 10 MiB: 105 → 19.7 ms |
| Recovery journal tick | no in-memory copy of the text; the I/O lock is held for validation and rename when the document did not change during the write | 10 MiB: 44 MB → 69 KB allocated per tick; bytes written unchanged |
| Own-save disk check | read and hash; decode only when the hash differs | 10 MiB: 74 → 6.6 ms |
| Changed-documents scan | O(documents × path depth) | 50 documents × 100k changes: 515 → 17 ms |
| Watcher intake | nothing queued for ignored trees; backlog at most 16,384 signals, then a rescan of that folder | 2,000 files under `.git` + 200 visible: 6,599 → 600 queued signals |
| Agent save with pending recovery | reads journal heads only | 28 → 1.4 ms |
| Keystroke with the Outline shown | outline rows rebuilt only when the outline changed | 1 MiB document: 2,155 → 4.8 ms per key |
| Explorer listing | one listing per folder in flight plus one trailing; unchanged listings update nothing | 1,000-file folder, burst of 30 new files: 3.3 → 1.2 s |

### Graph (18,000 files, 79,000 entities, 402,000 relations)

| Operation | Budget (work) | Before → after |
|---|---|---|
| Status | answered from a memo checked against the store's change counter | 25.8 → 0.06 ms |
| Small commit | cost of the rows written | 44 → 1.7 ms |
| One-file pass, file unchanged | no full-table read | 167 → 0.5 ms |
| One-file pass, file edited | reads scoped to the pass's paths | 237 → 35 ms |
| Overlay | scoped to the document | 94 → 3.9 ms |
| 2,160 pending marks | one statement per batch | 3,800 → 7 ms |
| Reads that decide (impact, removal planning) | complete, or flagged incomplete | a dependent hidden behind 20,050 rejected suggestions is now found |

A scoped pass and a full reindex must produce the same files, folders, bindings
and containment: `review_perf_differential` checks this over random operation
sequences and is part of the normal test suite.

### Pages

| Operation | Budget (work) | Before → after |
|---|---|---|
| Opening a document's pages | page images requested ≤ pages displayed ± 1, independent of page count | 200 pages: 200 images (21.2 MB), 3,217 ms → 3 images (0.3 MB), 520 ms |
| DOM for a long document | one sized box per page; content only for displayed pages | 200 pages: 328,839 → 5,345 nodes |
| Edit with several Page tabs open | one build per entry the change concerns; none for an entry whose last settled build did not read the changed document | 5 tabs: 5 → 1 build; unrelated file: 5 → 0 |
| Hidden Page tab | builds and diagnostics stay live; no page image, geometry or layout request | 0 page requests while hidden |
| Typst page map | linear in pages | 200 pages, one-character edit: 1,151 → 274 ms |
| LaTeX first page-map query | linear in pages | 100 pages: 66,090 → 1,207 ms |
| LaTeX page image | rendered and sent only when the page's content changed | unchanged rebuild at 100 pages: 100 images, 2,307 ms → 0 images; one-character edit: 1 image |
| LaTeX burst of edits | a superseding request waits for 150 ms of quiet, at most 1 s | 20 edits at 10/s: 20 → 3 engine processes |

When a rebuild may be skipped: for a Typst or Markdown entry, only when its
latest build is still held, had the changed document open without reading it,
and no read failed in that build; for a pdfLaTeX entry, only when its latest
build is a settled, successful full build that did not read the changed
document and the entry does not use Biber. A LuaLaTeX entry and everything
else rebuilds.

### Research, jobs and workflows

| Operation | Budget (work) | Before → after |
|---|---|---|
| Job output event | a notification without text; no job-list read | 50 MB of output: 10.5 MB of event text → about 90 bytes per event |
| Run history listing | decodes only records that changed since the last listing | 100 records of 870 KB: 440 → 0.06 ms, 0 records decoded |
| Research run history page | one folder listing and the manifests of the runs returned (at most 500), whatever the folder holds; a damaged record is read once | 2,000 runs of 209 KB: 500 listed and 1,500 unreachable → every run reachable in 4 pages (2,000 manifests, 300 ms); first page 500 manifests, 83 → 75 ms (debug build) |
| Finished research runs in memory | output kept for the 16 most recent of the open project; older runs read from their verified records | 300 runs × 1 MB: 300 → 16 MB; 0 after a project switch |
| Restricted preflight | one engine query answers presence and identity; nothing cached | 2 → 1 process per preflight, 7 → 6 per job |
| Interpreter discovery | one scan at a time; concurrent requests share it | two concurrent requests: 10 → 5 probes |

### Agents

| Operation | Budget (work) | Before → after |
|---|---|---|
| Task list and summaries | no review text read while a file is at the task's tip or baseline | 50 tasks × 5 files × 200 KiB: 1,329 ms and 153.7 MB read → 157 ms and 0 B |
| Streaming | events bounded by time (25 ms / 16 KiB joins), content complete and in order | 8,000-word answer: 4,091 events with 3,912 lost → 5 events, 0 lost |
| Conversation per turn | read and converted once per run; what the model is sent is unchanged | 101 turns: 423 MB → 45 KB parsed |
| Task detail while running | the tail since the last known entry; previews cut in the store | full read after 101 turns: 7.94 MB → 266 KB read |
| Follow-up with an unchanged graph context | a short statement instead of the block, only when the application's own earlier block proves equality | 46.4 → 2.2 KB per follow-up |
| `read_file`, `list_files` | partial reads and continuations that say what was left out | a 94-byte theorem no longer costs a 307 KB read |

### Startup and idle

Four warm launches: first usable window in about 0.6 s (release) and 0.65 s
(debug); idle JS heap 15 MB; app process 43 MB plus 371 MB for WebView2. No
change was made here.

### Release profile

Release profile, `large` scale, same host, minimum / median / maximum in ms. "Before" is
the code before this work with the same benchmark; 5 to 10 samples per row.

| Operation | Before | After |
|---|---|---|
| One typed character, 10 MiB, middle | 35.8 / 36.0 / 36.7 | 4.22 / 4.24 / 4.29 |
| One typed character, 1 MiB, middle | 3.63 / 3.71 / 3.93 | 0.46 / 0.46 / 0.47 |
| Undo of a typing group, 10 MiB | 17.4 / 17.5 / 18.2 | 4.19 / 4.22 / 4.26 |
| Changed-documents scan, 50 documents × 100,000 changes | 371 / 373 / 375 | 2.52 / 2.64 / 3.15 |
| Agent save with 5 pending recovery entries of 5 MiB | 27.4 / 27.8 / 31.5 | 3.53 / 3.90 / 4.88 |
| Graph status | 23.7 / 24.0 / 24.6 | 0.05 / 0.06 / 0.06 |
| Graph: small commit | 41.7 / 42.1 / 44.2 | 1.56 / 1.63 / 1.74 |
| Graph: one-file pass, file unchanged | 98.7 / 101 / 110 | 0.37 / 0.39 / 0.41 |
| Graph: one-file pass, file edited | 132 / 133 / 138 | 30.6 / 30.9 / 32.4 |
| Graph: overlay of a small file | 46.7 / 47.7 / 48.6 | 2.82 / 2.86 / 2.92 |
| Graph: 2,160 pending marks | 3,711 / 3,740 / 3,806 | 6.56 / 6.83 / 7.07 |
| Graph: impact of a hub (complete reads) | 234 / 237 / 254 | 252 / 257 / 265 |
| Graph: snapshot around a 25,000-relation hub (complete reads) | 782 / 800 / 802 | 932 / 936 / 953 |
| Typst: rebuild after one character, 200 pages | 685 / 688 / 695 | 87.3 / 88.3 / 89.7 |
| LaTeX: first page-map query, 100 pages | 3,812 / 3,825 / 3,828 | 207 / 208 / 212 |
| LaTeX: page images after an unchanged rebuild, 100 pages | 1,029 / 1,039 / 1,045 | 0.01 / 0.01 / 0.01 |
| LaTeX: page images after a one-character edit, 100 pages | 1,030 / 1,030 / 1,040 | 10.6 / 10.8 / 1,030 |
| Run history listing, limit 100, 500 runs of 870 KB | 147 / 148 / 155 | 0.03 / 0.03 / 0.10 |
| Run record write (one more index to maintain) | 0.70 / 0.88 / 1.05 | 1.13 / 1.21 / 3.29 |

Three rows are slower and are kept: graph impact and the hub snapshot no longer
stop at a silent row cap, and a run record write maintains the listing index. The
maximum of the one-character LaTeX row is the first edit of the run, which changed
a font subset and with it every page that uses the font. Agents, research and
webview rows are compared in the debug profile (tables above).

## Retained state

Every cache and retained collection, with owner, key, invalidation and bound.
Adding one means adding a row here and a differential test.

| Collection | Owner | Key | Invalidation | Bound |
|---|---|---|---|---|
| Graph status memo | graph store | the store's change counter | any commit on any connection | one value |
| Listed run summaries | workflow `RunStore` | row id, valid while id and state match | per row on write; all on another connection's commit or on compaction | 2,048, then cleared |
| Stored research runs | `ResearchService` | run id | output released after the record is written and read back; other projects' runs on switch | 16 runs with output |
| Research output in the webview | research store | run id | re-read on selection | selected + executing + newest 8 finished |
| LaTeX page images | `TexService` | content reference + scale | none needed (content-keyed); cleared when the folder is released | 96 MiB, least recently used |
| Page image URLs | `PageSession` | render reference of the shown build | revoked on detach | displayed pages + 12 |
| Build markers | Monaco model (weak) | path, revision, latest build | recomputed when any of the three changes | one per model |
| Run conversation | one agent run | — | dropped with the run; set aside if a message cannot be converted | the conversation that is sent anyway |
| Held stream deltas | one provider stream | — | flushed on kind change, timer, reset and end | 16 KiB or 25 ms |
| Watcher backlog | workspace intake | — | drained each batch | 16,384 signals, then rescan |
| Explorer listings in flight | workspace store | folder path | removed when the listing finishes; cleared on project change | folders in flight |
| Sorted commands | command registry | registry version | any registration change | one array |

The source index of a typesetting map and the per-file LaTeX offset index live
for one extraction or one build and are not caches.

## Known limits and open decisions

Measured and left for a decision, because the fix changes a format, a contract
or product behaviour:

- **Undo and rebase history have no byte bound.** Fifty external rewrites of a
  2 MiB document retain 210 MB; only the counts (1,000 groups, 2,048 revisions)
  limit it.
- **Folders of 10,000 files.** Expanding one takes about 12 s; the shared tree
  needs virtualization.
- **Workflow run records store the plan with every write.** A 97-step plan
  writes 761 MB over a run; storing the plan once needs a store version that
  older app versions refuse.
- **Recovery journal writes the whole text each tick.** At 10 MiB the write is
  slower than typing, so it runs under the I/O lock as before; only a delta
  format changes that.
- **Interpreter discovery on project switch** costs about 1.3 s; caching it
  across projects would hide packages installed outside the app until Refresh.
- **Image reference scan** sends the whole text per typing pause (5.3 MB at
  5 MiB); a document-id command is needed.
- **Lean freshness while checking** (three document states per edit) needs a
  "for an earlier revision" state in the interface.
- **Storage & History with thousands of research runs** reads every run
  record to list it with its size and outputs: 150 ms for 500 records of
  209 KB, 650 ms for 2,000 (debug build). It listed 500 of the 2,000 before.
- **An experiment's attempts** are looked for among the runs of the first
  history page; with more runs than that the list says it may be incomplete.
- **Rebuilds for unrelated edits** still happen for a LaTeX entry until its
  automatic full build has run, after a failed build, and always with Biber
  or LuaLaTeX.
- **A file over the whole-read limit** can be read by an agent in parts but not
  edited by it.
- **`build:app` with nothing changed** takes about 23 s.

Known differences between an incremental graph pass and a full reindex that
predate this work (a reference that stays resolved after a second declaration
of its label; a Lean module that goes stale when a lakefile moves; duplicate
resources for a folder path given in another letter case) are recorded by the
strict mode of `review_perf_differential` and are not yet fixed.

## For the agent runtime rework

The agent runtime may be reworked on top of these contracts. Keep them:

- **What the model is sent is a function of the stored conversation.** A run
  holds its conversation in memory, and each remembered message is exactly what
  a store read returns. A rework that adds compaction or sharing must keep a
  test that compares requests with a fresh read.
- **Tool results say what they left out.** `read_file` (`complete`,
  `nextStartLine`), `list_files` (`nextOffset`, `listing`, `listingChanged`),
  `run_python` (`outputTruncated`, `resultsOmitted`), `check_lean`
  (`diagnosticsOmitted`), graph reads (`truncated`). New tools follow the same
  rule; none may return a clean short answer.
- **No decision is cached.** The gate, authority re-read, consent, scope and
  exposure checks run per call. `edit_document` works only on a revision the
  task has seen in full; do not resolve edits against the live text, which
  would let a metadata-only task probe content.
- **Application-authored context is identified by position, never by scanning
  text** a user or a tool could have written.
- **Events are bounded by time, not by tokens**, and the webview can always
  recover by reading the tail of a task's detail from a sequence number.
- **Reads used by several agents at once are cheap by construction**: task
  summaries read no review text, the graph status is a memo, run listings decode
  nothing unchanged. Parallel agents multiply these reads; measure with
  `perf:rust -- exposition-agents` at `large` before and after.

Measured sizes to design against: system prompt 7.8 KB; tool definitions
35.8 KB, of which `workflow_propose` is 13.9 KB; building the tool list
0.6 ms per request; one turn about 12 ms of local work (debug). Not yet
addressed: tool-definition size per request, per-event render cost in the
webview (no app benchmark exists for the agents view), and the remaining
unflagged `take(300)` relation reads in the agents' graph queries.

Several agents running at once in the app multiply every per-run cost above.
What is known for that case, and what is not:

- Each run holds its own conversation in memory for its lifetime; with N runs
  that is N conversations. No bound across runs exists.
- The event channel holds 4,096 events and a webview that falls behind
  resynchronizes. Joined stream deltas keep one run far below that; the margin
  with many runs streaming at once is not measured.
- Task summaries, graph status and run listings were made cheap for one reader
  at a time. Writers still take the same store and document locks one at a
  time; contention between agents is not measured.
- Process launches (LaTeX, Python, Lean, commands) go through the shared
  execution service; nothing here changes how many may run at once.

The agent benchmarks time one task at a time. A rework that adds concurrent
agents starts by adding a benchmark that runs several.
