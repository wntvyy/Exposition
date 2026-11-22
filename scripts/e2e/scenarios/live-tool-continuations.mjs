// LIVE check that a real model uses the agent tools' partial reads and listing
// continuations correctly (opt-in: `npm run e2e -- live-tool-continuations`).
// Costs money: it uses the API key saved in the app's provider settings (OS
// credential store, default entry) and the default model. Every task has small
// hard budgets and execution is off except in part 4.
//
// Each part asks a question whose answer the model can only reach through a
// bounded tool result and its continuation, and checks the answer AND the tool
// calls and results exactly as the provider received them (the stored
// conversation), not the bounded previews of the task view:
//   1. A text file over the 512 KiB whole-read limit: one fact some hundred
//      lines after a stated line deep in the file (how far is not said, so a
//      read may end before it and have to be continued), one on the last
//      line. The reads are partial (`start_line` / `max_lines`), their ranges
//      cover both lines, nothing was read whole, and the file was not edited.
//      Whether a read was continued from `nextStartLine` is recorded.
//   2. A folder with 620 files, one of which has a marked name beyond the
//      first 500 walked entries: the name reaches the model only in a
//      `list_files` continuation that passes the truncated result's
//      `nextOffset` and `listing`.
//   3. A follow-up on an unchanged project graph: the stored follow-up message
//      carries the short "unchanged" statement instead of the context block,
//      and the model answers from the earlier block without calling a tool.
//   4. `run_python` showing 650 values with `display` and printing the last
//      one (651 output entries: more than one 500-entry page of the run's
//      output, and more rich results than a tool result returns): the last
//      value is reported and the result says what it leaves out. Skipped,
//      with the reason recorded, when no usable Python research environment
//      exists.
// A model that answers wrongly, loops, or ignores a continuation is the finding
// this scenario exists for. Evidence: test-results/e2e/live-tool-continuations/
// summary.json (tool calls without file texts, checks, usage per task).
import { cp, mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

import { invoke, waitFor } from "../workspace-helpers.mjs";

/** Anthropic and OpenAI key shapes. */
const KEY_PATTERN = /sk-(?:ant-|proj-|svcacct-)[A-Za-z0-9_-]{8,}|sk-[A-Za-z0-9]{32,}/;
const WHOLE_READ_LIMIT = 512 * 1024;
const LIST_PAGE = 500;

const BUDGETS = {
  read: { maxTurns: 12, maxOutputTokens: 12_000, maxInputTokens: 600_000, maxWallMs: 300_000, maxToolCalls: 14, maxCommands: 0 },
  list: { maxTurns: 6, maxOutputTokens: 12_000, maxInputTokens: 500_000, maxWallMs: 300_000, maxToolCalls: 8, maxCommands: 0 },
  graph: { maxTurns: 3, maxOutputTokens: 8_000, maxInputTokens: 150_000, maxWallMs: 180_000, maxToolCalls: 3, maxCommands: 0 },
  python: { maxTurns: 4, maxOutputTokens: 8_000, maxInputTokens: 200_000, maxWallMs: 300_000, maxToolCalls: 4, maxCommands: 2 },
};

// ---- Fixture: deterministic content ------------------------------------------------

const READINGS = "data/readings.txt";
const READINGS_LINES = 14_000;
const CALIBRATION_AFTER = 9_000;
const CALIBRATION_LINE = 9_637;
const CALIBRATION_CODE = "QZ-58213-LM";
const CHECKSUM_TOTAL = "7741-BRAVO";

function readingsText() {
  const lines = [];
  let seed = 20261121;
  for (let n = 1; n <= READINGS_LINES; n += 1) {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    const value = (seed % 100000) / 100000;
    lines.push(`reading ${String(n).padStart(5, "0")} sensor=S${String(seed % 97).padStart(2, "0")} value=${value.toFixed(5)} status=ok`);
  }
  lines[CALIBRATION_LINE - 1] = `CALIBRATION code=${CALIBRATION_CODE}`;
  lines[READINGS_LINES - 1] = `CHECKSUM total=${CHECKSUM_TOTAL}`;
  return `${lines.join("\n")}\n`;
}

const ARCHIVE_FILES = 620;
const MARKED_INDEX = 587;
const MARKED_NAME = `note-${String(MARKED_INDEX).padStart(4, "0")}-final.txt`;
const archiveNames = () =>
  Array.from({ length: ARCHIVE_FILES }, (_, i) => (i + 1 === MARKED_INDEX ? MARKED_NAME : `note-${String(i + 1).padStart(4, "0")}.txt`));

const PYTHON_STEPS = 650;
const MAX_RESULTS = 500;
const PYTHON_CODE = `x = 2\nfor _ in range(${PYTHON_STEPS}):\n    x = (x * 1103515245 + 12345) % 2147483648\n    display(x)\nprint("last", x)`;
function pythonLast() {
  let x = 2n;
  for (let i = 0; i < PYTHON_STEPS; i += 1) x = (x * 1103515245n + 12345n) % 2147483648n;
  return String(x);
}

async function makeLargeProject() {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-continuations-"));
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(join(root, "archive"), { recursive: true });
  await writeFile(join(root, "README.md"), "# Readings\n\nSensor readings in `data/`, archived notes in `archive/`.\n");
  const text = readingsText();
  await writeFile(join(root, READINGS), text);
  const names = archiveNames();
  for (const name of names) await writeFile(join(root, "archive", name), `${name.replace(/\.txt$/, "")}\n`);
  return { root, readingsBytes: Buffer.byteLength(text), names };
}

// ---- Tasks and their stored conversation ----------------------------------------------

const terminal = (status) => !["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(status.kind);

/** Wait for the task's current run to end, approving the listed tools only. */
async function finish(page, id, approveTools, approvals, timeout = 420_000) {
  const answered = new Set();
  const started = Date.now();
  for (;;) {
    const detail = await invoke(page, "agents_task_detail", { task: id });
    for (const approval of detail.approvals) {
      if (answered.has(approval.id)) continue;
      answered.add(approval.id);
      const approved = approveTools.includes(approval.tool);
      approvals.push({ task: id, tool: approval.tool, title: approval.title, approved });
      await invoke(page, "agents_approval_resolve", { task: id, approval: approval.id, approved });
    }
    if (terminal(detail.summary.status)) return detail;
    if (Date.now() - started > timeout) {
      await invoke(page, "agents_task_cancel", { task: id }).catch(() => {});
      throw new Error(`task ${id} did not finish within ${timeout} ms`);
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}

/** Every message of the task exactly as the provider receives it. */
function sentMessages(dataDir, task) {
  const db = new DatabaseSync(join(dataDir, "agents.sqlite3"), { readOnly: true });
  try {
    return db
      .prepare("SELECT role, content FROM transcript WHERE task_id = ? AND kind = 'message' ORDER BY seq")
      .all(task)
      .map((row) => ({ role: row.role, content: JSON.parse(row.content) }));
  } finally {
    db.close();
  }
}

const resultText = (block) =>
  typeof block.content === "string"
    ? block.content
    : (block.content ?? []).map((part) => (typeof part === "string" ? part : (part.text ?? ""))).join("");
const parse = (text) => {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

/** The tool calls of `messages`, in order, each with the result the model was sent. */
function toolCalls(messages) {
  const results = new Map();
  for (const block of messages.flatMap((m) => m.content)) {
    if (block.type === "tool_result") results.set(block.tool_use_id, block);
  }
  return messages
    .filter((m) => m.role === "assistant")
    .flatMap((m) => m.content)
    .filter((b) => b.type === "tool_use")
    .map((b) => {
      const result = results.get(b.id);
      const text = result ? resultText(result) : null;
      return { name: b.name, input: b.input ?? {}, isError: result ? result.is_error === true : null, text, json: text === null ? null : parse(text) };
    });
}

const assistantText = (messages) =>
  messages
    .filter((m) => m.role === "assistant")
    .flatMap((m) => m.content)
    .filter((b) => b.type === "text")
    .map((b) => b.text)
    .join("\n");

/** A call for the summary: arguments and the result's signals, never a file's text or a listing's entries. */
function summarize(call) {
  const out = { tool: call.name, input: call.input, isError: call.isError };
  if (call.name === "run_python") out.input = { codeBytes: String(call.input.code ?? "").length };
  const json = call.json;
  if (json && typeof json === "object") {
    const signals = {};
    for (const [key, value] of Object.entries(json)) {
      if (["text", "entries", "graph", "stdout", "stderr", "results", "artifacts", "contentHash"].includes(key)) continue;
      signals[key] = typeof value === "string" ? value.slice(0, 400) : value;
    }
    if (typeof json.text === "string") signals.textBytes = Buffer.byteLength(json.text);
    if (Array.isArray(json.entries)) signals.entryCount = json.entries.length;
    if (typeof json.stdout === "string") signals.stdoutBytes = Buffer.byteLength(json.stdout);
    out.result = signals;
  } else if (call.text !== null) {
    out.result = call.text.slice(0, 400);
  }
  return out;
}

const usageDelta = (after, before) => ({
  turns: after.turns - (before?.turns ?? 0),
  inputTokens: after.inputTokens - (before?.inputTokens ?? 0),
  outputTokens: after.outputTokens - (before?.outputTokens ?? 0),
  cacheReadTokens: after.cacheReadTokens - (before?.cacheReadTokens ?? 0),
  cacheWriteTokens: after.cacheWriteTokens - (before?.cacheWriteTokens ?? 0),
  toolCalls: after.toolCalls - (before?.toolCalls ?? 0),
  commands: after.commands - (before?.commands ?? 0),
  servedBy: after.servedBy,
});

/** Wait until the project graph has stopped changing (two equal readings 1.5 s apart). */
async function graphSettled(page) {
  let previous = null;
  await waitFor(
    async () => {
      const status = await invoke(page, "graph_status");
      const now = JSON.stringify(status);
      const settled = previous === now && JSON.stringify(status.freshness).includes("synchronized");
      previous = now;
      return settled;
    },
    { timeout: 120_000, interval: 1_500, message: "the project graph settles" },
  );
}

export default async function liveToolContinuations({ launchApp, artifactsDir, assert, fixture }) {
  const large = await makeLargeProject();
  const small = await mkdtemp(join(tmpdir(), "exposition-live-continuations-graph-"));
  await cp(fixture("graph-basic"), small, { recursive: true });

  const app = await launchApp();
  const { page } = app;
  const summary = { model: null, parts: [], checks: [], approvals: [], skipped: [] };
  const check = (name, ok, detail = "") => {
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    summary.checks.push({ name, ok: !!ok, detail: text.slice(0, 1500) });
    console.log(`${ok ? "ok" : "FAIL"}  ${name}${text ? `: ${text.slice(0, 300)}` : ""}`);
  };
  const tasks = [];
  /** Run one part; a thrown error is that part's failure, the other parts still run. */
  const part = async (name, body) => {
    const record = { name, calls: [], usage: null, status: null };
    summary.parts.push(record);
    try {
      await body(record);
    } catch (error) {
      check(`${name}: ran to its end`, false, String(error?.message ?? error));
    }
  };
  /** Create a task, wait for it, and read what the provider was sent. */
  const run = async (record, request, approveTools = []) => {
    const created = await invoke(page, "agents_task_create", { request });
    tasks.push(created.id);
    const detail = await finish(page, created.id, approveTools, summary.approvals);
    const messages = sentMessages(app.dataDir, created.id);
    const calls = toolCalls(messages);
    record.task = created.id;
    record.status = detail.summary.status;
    record.usage = usageDelta(detail.summary.usage);
    record.calls = calls.map(summarize);
    record.answer = assistantText(messages).slice(-1200);
    return { id: created.id, detail, messages, calls, answer: assistantText(messages) };
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    const provider = await invoke(page, "agents_provider_status");
    assert.ok(provider.configured, "an API key must be saved in the app's provider settings");
    assert.notEqual(provider.settings.provider, "scripted", "live run uses the real provider");
    summary.model = { provider: provider.settings.provider, model: provider.settings.model, effort: provider.settings.effort };

    // ---- Fixture facts the parts rely on ----------------------------------------
    check("fixture: the readings file is over the whole-read limit", large.readingsBytes > WHOLE_READ_LIMIT, `${large.readingsBytes} bytes`);
    // list_files walks a folder in path order (byte order; the names are ASCII).
    const walk = [...large.names].sort();
    const markedAt = walk.indexOf(MARKED_NAME);
    check("fixture: the marked name is beyond the first 500 entries of the folder's walk", markedAt >= LIST_PAGE && walk.length > LIST_PAGE, {
      index: markedAt,
      files: walk.length,
    });

    await invoke(page, "workspace_open", { path: large.root });
    await page.locator('[data-testid="file-tree"] [data-path="README.md"]').waitFor({ timeout: 30_000 });
    const base = { workspaceRoot: large.root, scope: { kind: "workspace" }, consent: "scopeContent", permission: "autoApply", execution: "disabled" };

    // ---- 1. Large file: partial reads ----------------------------------------------
    await part("1 large file", async (record) => {
      const prompt =
        `In ${READINGS}, the first line after line ${CALIBRATION_AFTER} that starts with CALIBRATION holds a calibration code, and the ` +
        "last line of the file starts with CHECKSUM. Report the calibration code and the checksum total.";
      const { detail, calls, answer } = await run(record, { ...base, prompt, budgets: BUDGETS.read });
      await page.screenshot({ path: join(artifactsDir, "1-large-file.png") });
      check("1: task completed", detail.summary.status.kind === "completed", detail.summary.status);
      check("1: the answer gives the calibration code", answer.includes(CALIBRATION_CODE), answer.slice(-400));
      check("1: the answer gives the checksum total", answer.includes(CHECKSUM_TOTAL), answer.slice(-400));
      const reads = calls.filter((c) => c.name === "read_file" && c.input.path === READINGS);
      const succeeded = reads.filter((c) => c.isError === false && c.json);
      const whole = (c) => !(Number(c.input.start_line) > 0 || Number(c.input.max_lines) > 0);
      record.wholeReadAttempts = reads.filter(whole).length;
      check("1: the file was read with start_line / max_lines", succeeded.some((c) => !whole(c)), reads.map((c) => c.input));
      check(
        "1: no read returned the whole file (every successful result states its lines)",
        succeeded.every((c) => !whole(c) && Number.isInteger(c.json.startLine) && Number.isInteger(c.json.endLine) && c.json.totalLines === READINGS_LINES && c.json.complete === false),
        succeeded.map((c) => ({ input: c.input, startLine: c.json.startLine, endLine: c.json.endLine, complete: c.json.complete })),
      );
      // A result that stops before the file's end must say where to go on.
      check(
        "1: every partial result that ends before the file's end carries nextStartLine",
        succeeded.every((c) => c.json.endLine >= READINGS_LINES || c.json.nextStartLine === c.json.endLine + 1),
        succeeded.map((c) => ({ endLine: c.json.endLine, nextStartLine: c.json.nextStartLine ?? null })),
      );
      const covers = (line) => succeeded.some((c) => c.json.startLine <= line && line <= c.json.endLine && String(c.json.text ?? "").length > 0);
      check("1: the ranges read cover the calibration line", covers(CALIBRATION_LINE), { line: CALIBRATION_LINE });
      check("1: the ranges read cover the last line", covers(READINGS_LINES), { line: READINGS_LINES });
      const offered = new Set(succeeded.map((c) => c.json.nextStartLine).filter(Number.isInteger));
      record.followedNextStartLine = reads.filter((c) => offered.has(Number(c.input.start_line))).length;
      record.bytesRead = succeeded.reduce((sum, c) => sum + Buffer.byteLength(String(c.json.text ?? "")), 0);
      const edits = calls.filter((c) => c.name === "edit_document" && c.input.path === READINGS && c.isError === false);
      check("1: the file was not edited", edits.length === 0, edits.map((c) => c.input.path));
      const onDisk = await readFile(join(large.root, READINGS), "utf8");
      check("1: the file on disk is unchanged", onDisk === readingsText(), `${onDisk.length} chars`);
    });

    // ---- 2. Large listing: continuation ----------------------------------------------
    await part("2 large listing", async (record) => {
      const prompt =
        "Use list_files on the archive folder: exactly one file in it has a name ending in -final.txt. Which file is it? " +
        "Answer with its file name.";
      const { detail, messages, calls, answer } = await run(record, { ...base, prompt, budgets: BUDGETS.list });
      await page.screenshot({ path: join(artifactsDir, "2-large-listing.png") });
      check("2: task completed", detail.summary.status.kind === "completed", detail.summary.status);
      check("2: the answer names the marked file", answer.includes(MARKED_NAME), answer.slice(-400));
      const others = (answer.match(/note-\d{4}(?:-final)?\.txt/g) ?? []).filter((name) => name !== MARKED_NAME);
      check("2: the answer names no other file", others.length === 0, others);
      const lists = calls.filter((c) => c.name === "list_files" && c.isError === false && c.json);
      const truncated = lists.filter((c) => c.json.truncated === true && !c.json.listingChanged);
      check("2: a listing was truncated at 500 entries and said so", truncated.length > 0 && truncated.every((c) => Number.isInteger(c.json.nextOffset) && /^[0-9a-f]{32}$/.test(c.json.listing ?? "") && typeof c.json.note === "string"), truncated.map((c) => ({ input: c.input, nextOffset: c.json.nextOffset })));
      const continuations = lists.filter((c) => Number(c.input.offset) > 0);
      const proper = continuations.filter((c) =>
        truncated.some((t) => t.json.nextOffset === Number(c.input.offset) && t.json.listing === c.input.listing && t.input.path === c.input.path && t.input.recursive === c.input.recursive),
      );
      check("2: the model continued with the truncated result's nextOffset and listing", proper.length > 0, continuations.map((c) => c.input));
      // The folder never changes. A call that passes a listing the tool issued
      // must not be told it changed; a call that passes anything else as the
      // listing (a model sometimes sends an invented one with its first call)
      // must be told so, never answered as if it continued.
      const issued = new Set(lists.map((c) => c.json.listing).filter((id) => /^[0-9a-f]{32}$/.test(id ?? "")));
      const passed = lists.filter((c) => c.input.listing !== undefined && c.input.listing !== null && c.input.listing !== "");
      const wrong = passed.filter((c) => Boolean(c.json.listingChanged) !== !issued.has(c.input.listing));
      const unasked = lists.filter((c) => !passed.includes(c) && c.json.listingChanged);
      record.inventedListings = passed.filter((c) => !issued.has(c.input.listing)).length;
      check(
        "2: listingChanged answers exactly the calls that passed a listing the tool did not issue",
        wrong.length === 0 && unasked.length === 0,
        lists.map((c) => ({ listing: c.input.listing ?? null, listingChanged: c.json.listingChanged ?? false })),
      );
      const holds = (text) => typeof text === "string" && text.includes(MARKED_NAME);
      check("2: the marked name is in a continuation's entries", proper.some((c) => (c.json.entries ?? []).some((e) => holds(e.path))), proper.map((c) => (c.json.entries ?? []).length));
      // The continuation is the only way the name reached the model.
      const elsewhere = calls.filter((c) => !proper.includes(c) && holds(c.text)).map((c) => ({ tool: c.name, input: c.input }));
      const inPrompt = messages.filter((m) => m.role === "user").flatMap((m) => m.content).some((b) => b.type === "text" && holds(b.text));
      check("2: the marked name reached the model only through the continuation", elsewhere.length === 0 && !inPrompt, { elsewhere, inPrompt });
    });

    // ---- 3. Follow-up with an unchanged graph context -----------------------------------
    await invoke(page, "workspace_open", { path: small });
    await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 30_000 });
    const smallBase = { workspaceRoot: small, scope: { kind: "workspace" }, consent: "scopeContent" };
    await part("3 follow-up, unchanged graph context", async (record) => {
      await waitFor(
        async () =>
          (await invoke(page, "graph_snapshot", { query: { kinds: ["theorem", "lemma"], text: null, around: null, includeInactive: false, limit: 50 } })).entities.length >= 2,
        { timeout: 60_000, message: "paper.tex indexed" },
      );
      await graphSettled(page);
      const first = await run(record, {
        ...smallBase,
        permission: "readOnly",
        execution: "disabled",
        prompt: "Without calling any tools: how many entities of kind theorem does the project graph context list? Answer with the number.",
        budgets: BUDGETS.graph,
      });
      check("3: first task completed", first.detail.summary.status.kind === "completed", first.detail.summary.status);
      const context = first.detail.graph.context;
      // The fixture's proof cites one lemma: the graph records `references` from the proof to it.
      const proofs = (context?.entities ?? []).filter((e) => e.kind === "proof");
      const lemmas = (context?.entities ?? []).filter((e) => e.kind === "lemma");
      const relations = (context?.relations ?? []).filter((r) => r.kind === "references" && proofs.some((p) => p.id === r.from) && lemmas.some((l) => l.id === r.to));
      const lemma = lemmas.find((l) => l.id === relations[0]?.to);
      // The label as the paper writes it ("lem:closed" of "Lemma lem:closed").
      const key = lemma?.label?.split(/\s+/).at(-1);
      check("3 fixture: the context lists one proof with a references relation to one labelled lemma", proofs.length === 1 && relations.length === 1 && !!key, {
        proofs: proofs.map((p) => p.label),
        lemmas: lemmas.map((l) => l.label),
        relations: relations.length,
      });
      if (!key) return;
      record.expected = { lemma: lemma.label };
      const before = first.detail.summary.usage;
      const firstCount = first.messages.length;

      await invoke(page, "agents_task_continue", {
        task: first.id,
        prompt:
          "Still without calling any tools: in the project graph context, the proof has a references relation to one lemma. " +
          "Give that lemma's label.",
        budgets: null,
      });
      const detail = await finish(page, first.id, [], summary.approvals);
      await page.screenshot({ path: join(artifactsDir, "3-follow-up.png") });
      const messages = sentMessages(app.dataDir, first.id);
      const later = messages.slice(firstCount);
      const calls = toolCalls(later);
      const answer = assistantText(later);
      record.followUp = { status: detail.summary.status, usage: usageDelta(detail.summary.usage, before), calls: calls.map(summarize), answer: answer.slice(-800) };
      check("3: follow-up completed", detail.summary.status.kind === "completed", detail.summary.status);
      const blocks = (list) => list.filter((m) => m.role === "user").flatMap((m) => m.content).filter((b) => b.type === "text" && b.text.startsWith("<graph_context>")).map((b) => b.text);
      const sentFirst = blocks(messages.slice(0, firstCount));
      const sentLater = blocks(later);
      record.followUp.graphContextBytes = { first: sentFirst.map((t) => t.length), followUp: sentLater.map((t) => t.length) };
      check("3: the first message carries the whole context block", sentFirst.length === 1 && sentFirst[0].includes(key) && sentFirst[0].includes('"entities"'), sentFirst.map((t) => t.length));
      check(
        "3: the stored follow-up message carries the short statement, not the block",
        sentLater.length === 1 &&
          /^<graph_context>\nThe project graph context is unchanged since the graph context given earlier in this conversation \(project graph revision \d+\)/.test(sentLater[0]) &&
          sentLater[0].length < 600 &&
          !sentLater[0].includes('"entities"') &&
          !sentLater[0].includes(key),
        sentLater.map((t) => t.slice(0, 200)),
      );
      check("3: the task records two graph contexts (one per run)", detail.graph.contexts === 2, detail.graph.contexts);
      check("3: the follow-up called no tool (the answer comes from the earlier context)", calls.length === 0, calls.map((c) => c.name));
      check("3: the follow-up names the lemma from the earlier context", answer.includes(key), answer.slice(-400));
    });

    // ---- 4. run_python with long output ---------------------------------------------
    await part("4 run_python, long output", async (record) => {
      const environments = await invoke(page, "research_environments", { refresh: true });
      const python = environments.runtimes.find((r) => r.language === "python" && r.availability.kind === "available" && r.kernel && r.hostInterpreter);
      if (!python) {
        const reason = "no usable Python research environment (interpreter with a kernel) on this host";
        record.skipped = reason;
        summary.skipped.push({ part: record.name, reason });
        console.log(`skip  4: ${reason}`);
        return;
      }
      await invoke(page, "trust_grant", { root: small });
      const last = pythonLast();
      const prompt = `Call run_python once with exactly this code, then tell me the number on the line it prints at the end.\n\n${PYTHON_CODE}`;
      const { detail, calls, answer } = await run(
        record,
        { ...smallBase, permission: "autoApply", execution: "approveEach", prompt, budgets: BUDGETS.python },
        ["run_python"],
      );
      await page.screenshot({ path: join(artifactsDir, "4-run-python.png") });
      check("4: task completed", detail.summary.status.kind === "completed", detail.summary.status);
      const runs = calls.filter((c) => c.name === "run_python" && c.isError === false && c.json);
      const good = runs.filter((c) => String(c.json.stdout ?? "").trimEnd().endsWith(`last ${last}`));
      check("4: run_python ran the code and its result holds the last line", good.length > 0, runs.map((c) => String(c.json.stdout ?? "").slice(-40)));
      // Digit grouping in the answer ("1,234,567") is not a different number.
      const digits = answer.replace(/(?<=\d)[,\s](?=\d{3}(?!\d))/g, "");
      const found = (digits.match(/\d+/g) ?? []).includes(last);
      check("4: the answer gives the last printed value", found, answer.slice(-300));
      record.flags = runs.map((c) => ({ outputTruncated: c.json.outputTruncated, resultsOmitted: c.json.resultsOmitted, results: (c.json.results ?? []).length }));
      check("4: every result carries outputTruncated and resultsOmitted", runs.length > 0 && runs.every((c) => typeof c.json.outputTruncated === "boolean" && Number.isInteger(c.json.resultsOmitted)), record.flags);
      // 650 values were shown: a result that returns fewer says how many it left out, and that output is missing.
      check(
        "4: a result without every shown value says so (resultsOmitted, outputTruncated)",
        good.every((c) => {
          const held = (c.json.results ?? []).length;
          return held + c.json.resultsOmitted === PYTHON_STEPS && (held === PYTHON_STEPS || c.json.outputTruncated === true);
        }),
        record.flags,
      );
      check("4: more values were shown than a result returns (the flags are exercised)", good.some((c) => (c.json.results ?? []).length === MAX_RESULTS && c.json.resultsOmitted === PYTHON_STEPS - MAX_RESULTS), record.flags);
      // The tool reads a finished run's output 500 entries at a time: the last line is beyond the first page.
      const pages = [];
      for (const call of good) {
        const output = await invoke(page, "research_run_output", { runId: call.json.run, after: null, limit: null });
        pages.push({ lastSeq: output.lastSeq, truncated: output.truncated });
      }
      record.outputEntries = pages;
      check("4 fixture: the run's output has more than 500 entries", pages.length > 0 && pages.every((p) => p.lastSeq > LIST_PAGE), pages);
    });

    // ---- The key is nowhere in what was stored or recorded ---------------------------
    const stored = tasks.map((id) => JSON.stringify(sentMessages(app.dataDir, id))).join("\n");
    check("no API key in the stored conversations", !KEY_PATTERN.test(stored));
    check("no API key in the app logs", !KEY_PATTERN.test(app.logs.join("")));
    let keyInProject = false;
    const scan = async (dir) => {
      for (const entry of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) await scan(path);
        else if (KEY_PATTERN.test((await readFile(path).catch(() => Buffer.alloc(0))).toString("latin1"))) keyInProject = true;
      }
    };
    await scan(large.root);
    await scan(small);
    check("no API key in the project files", !keyInProject);
    check("no API key in the summary", !KEY_PATTERN.test(JSON.stringify(summary)));
  } finally {
    await writeFile(join(artifactsDir, "summary.json"), JSON.stringify(summary, null, 2)).catch(() => {});
    await app.close();
  }
  const failed = summary.checks.filter((c) => !c.ok);
  assert.deepEqual(failed.map((c) => c.name), [], `failed checks (see ${join(artifactsDir, "summary.json")})`);
}
