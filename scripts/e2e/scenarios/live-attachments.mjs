// LIVE: attachments reach the real model provider (opt-in:
// `npm run e2e -- live-attachments`). Costs money: it uses the API key saved
// in the app's provider settings (OS credential store, default entry). Small
// hard budgets; read-only tasks with execution disabled; no file is changed.
//
// What only the real provider can confirm: Exposition's image and PDF payloads
// (built from attachment snapshots at request time) are accepted and readable,
// and a follow-up replays the attachments with the history.
//   A. A generated PNG (solid red) and a generated one-page PDF (a title) are
//      attached as pasted bytes; the model names the colour and the title.
//   B. A follow-up in the same task, with a text attachment, still answers
//      from the earlier image (history replay with attachments).
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";

import { invoke, openWorkspace } from "../workspace-helpers.mjs";

const BUDGETS = { maxTurns: 3, maxOutputTokens: 8_000, maxInputTokens: 60_000, maxWallMs: 180_000, maxToolCalls: 2, maxCommands: 0 };
const TITLE = "Orchid Lattice Notes";

/** A solid-colour PNG (RGB, 64×64). */
function png([r, g, b]) {
  const size = 64;
  const row = Buffer.concat([Buffer.from([0]), Buffer.alloc(size * 3, 0).map((_, i) => [r, g, b][i % 3])]);
  const raw = Buffer.concat(Array.from({ length: size }, () => row));
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body) >>> 0);
    return Buffer.concat([len, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** A minimal one-page PDF showing `text` (Helvetica, with a valid xref). */
function pdf(text) {
  const stream = `BT /F1 24 Tf 72 720 Td (${text}) Tj ET`;
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let out = "%PDF-1.4\n";
  const offsets = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const o of offsets) out += `${String(o).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, "latin1");
}

const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);

async function waitTask(page, id, timeout = 180_000) {
  const started = Date.now();
  for (;;) {
    const detail = await invoke(page, "agents_task_detail", { task: id });
    if (terminal(detail.summary.status)) return detail;
    if (Date.now() - started > timeout) throw new Error(`task ${id} did not finish`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

const added = (outcome) => {
  if (outcome.kind !== "added") throw new Error(`not attached: ${JSON.stringify(outcome)}`);
  return outcome.attachment;
};
const answers = (detail) => detail.transcript.filter((t) => t.kind === "assistantText").map((t) => t.text).join("\n");

export default async function liveAttachments({ launchApp, artifactsDir, assert }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-attach-"));
  await writeFile(join(root, "notes.md"), "# Notes\n");
  const app = await launchApp();
  const { page } = app;
  const evidence = { root, tasks: [], checks: [] };
  const check = (name, ok, detail = "") => {
    evidence.checks.push({ name, ok, detail: typeof detail === "string" ? detail : JSON.stringify(detail) });
    assert.ok(ok, `${name}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };
  try {
    const status = await invoke(page, "agents_provider_status");
    check("provider configured from the OS credential store", status.configured === true, { model: status.settings.model, provider: status.settings.provider });
    await openWorkspace(page, root);
    const image = added(await invoke(page, "agents_attachments_add_bytes", { name: "swatch.png", mediaType: "image/png", data: png([220, 20, 30]).toString("base64") }));
    const doc = added(await invoke(page, "agents_attachments_add_bytes", { name: "notes.pdf", mediaType: "application/pdf", data: pdf(TITLE).toString("base64") }));
    check("attachments ingested", image.kind === "image" && doc.kind === "pdf", { image, doc });

    // A. image + PDF in the first message.
    const created = await invoke(page, "agents_task_create", {
      request: {
        prompt: "Without using any tools, answer in one line: what is the main colour of the attached image, and what is the title text in the attached PDF?",
        workspaceRoot: root,
        scope: { kind: "workspace" },
        permission: "readOnly",
        execution: "disabled",
        consent: "scopeContent",
        budgets: BUDGETS,
        attachments: [image.id, doc.id],
      },
    });
    const a = await waitTask(page, created.id);
    evidence.tasks.push({ id: created.id, status: a.summary.status, usage: a.summary.usage, answer: answers(a) });
    check("task A completed", a.summary.status.kind === "completed", a.summary.status);
    check("the model read the image (red)", /red|crimson|scarlet/i.test(answers(a)), answers(a));
    check("the model read the PDF title", /orchid\s+lattice/i.test(answers(a)), answers(a));
    const shown = a.transcript.find((t) => t.kind === "userText" && t.attachments.length === 2);
    check("the message shows both attachments", Boolean(shown), a.transcript.map((t) => t.kind));

    // B. follow-up with a text attachment; the image is replayed with the history.
    const note = added(await invoke(page, "agents_attachments_add_bytes", { name: "word.txt", mediaType: "text/plain", data: Buffer.from("The code word is PERIWINKLE.\n").toString("base64") }));
    await invoke(page, "agents_task_continue", {
      task: created.id,
      prompt: "Without tools, in one line: repeat the colour of the earlier image and the code word in the new attached text file.",
      budgets: null,
      attachments: [note.id],
      provider: null,
    });
    const b = await waitTask(page, created.id);
    evidence.tasks.push({ id: created.id, status: b.summary.status, usage: b.summary.usage, answer: answers(b) });
    const last = b.transcript.filter((t) => t.kind === "assistantText").at(-1)?.text ?? "";
    check("task B completed", b.summary.status.kind === "completed", b.summary.status);
    check("the follow-up still sees the image and reads the text attachment", /red|crimson|scarlet/i.test(last) && /periwinkle/i.test(last), last);
    check("no file changed", (await invoke(page, "agents_review", { task: created.id })).files.length === 0);
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
