// R-sec (M6 security review): where the provider API key can end up.
//
// Debug build only: it saves a FAKE key under a unique
// EXPOSITION_CREDENTIAL_NAMESPACE (debug builds only honor it; a release build
// would overwrite the user's real entry, so the scenario refuses to run there)
// and removes it again at the end. It never contacts a model provider.
//
// Checks:
// - the key typed into Agent provider settings never comes back to the webview
//   (status, DOM, inputs) and is not written to app data, the WebView2
//   profile, the project, or the app's stdout/stderr;
// - what a trusted native child (a Python research kernel) can do with it:
//   read it straight from Windows Credential Manager (CredReadW), and whether
//   the app's redaction keeps it out of run output, tracebacks, rich display
//   output, and what the app stores.
//
// Runs only when named: `npm run e2e -- review-sec-secrets`.
// Prints `ok:` / `DEFECT:` / `INFO:` lines and fails if any DEFECT.
import { createHash, randomBytes } from "node:crypto";
import { cp, mkdtemp, readdir, readFile, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const NAME = "review-sec-secrets";
const KERNEL_TIMEOUT = 120_000;

/** Every regular file under `dir` (best effort; locked files are skipped). */
async function* walk(dir) {
  let entries = [];
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* walk(path);
    else if (entry.isFile()) yield path;
  }
}

/** Files under `dir` whose bytes contain `needle` in UTF-8 or UTF-16LE. */
async function filesContaining(dir, needle) {
  const patterns = [Buffer.from(needle, "utf8"), Buffer.from(needle, "utf16le")];
  const hits = [];
  for await (const path of walk(dir)) {
    try {
      if ((await stat(path)).size > 256 * 1024 * 1024) continue;
      const bytes = await readFile(path);
      if (patterns.some((p) => bytes.includes(p))) hits.push(path);
    } catch {
      // locked or vanished
    }
  }
  return hits;
}

async function settle(page, runId) {
  return waitFor(
    async () => {
      const m = await invoke(page, "research_run", { runId });
      return m.record.status.kind !== "running" && m.record.status.kind !== "queued" ? m : null;
    },
    { timeout: KERNEL_TIMEOUT, message: `run ${runId}` },
  );
}

async function run(page, sessionId, code) {
  const started = await invoke(page, "research_execute", {
    request: { sessionId, source: { kind: "input", code }, requester: { kind: "user" } },
  });
  const manifest = await settle(page, started.id);
  const output = await invoke(page, "research_run_output", { runId: started.id, after: null, limit: 200 });
  return { manifest, output, text: JSON.stringify(output) };
}

export default async function reviewSecSecrets({ launchApp, artifactsDir, fixture }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }
  const findings = [];
  const ok = (id, detail = "") => {
    findings.push({ id, verdict: "ok", detail });
    console.log(`ok: ${id}${detail ? ` — ${detail}` : ""}`);
  };
  const defect = (id, detail = "") => {
    findings.push({ id, verdict: "DEFECT", detail });
    console.log(`DEFECT: ${id}${detail ? ` — ${detail}` : ""}`);
  };
  const info = (id, detail = "") => {
    findings.push({ id, verdict: "INFO", detail });
    console.log(`INFO: ${id}${detail ? ` — ${detail}` : ""}`);
  };
  const check = (id, condition, detail) => (condition ? ok(id, detail) : defect(id, detail));

  const base = await mkdtemp(join(tmpdir(), "xp-rsec-secrets-"));
  const project = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), project, { recursive: true });
  // A unique, obviously fake key in the Anthropic token shape. `core` is what
  // the file scan looks for, so a partly redacted copy still counts.
  const core = `RSEC${randomBytes(12).toString("hex")}`;
  const FAKE_KEY = `sk-ant-api03-${core}-fake`;
  const keySha = createHash("sha256").update(FAKE_KEY).digest("hex");
  const namespace = `review-sec-secrets-${Date.now()}`;
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: namespace };
  const target = `anthropic-api-key.org.exposition.desktop.${namespace}`;

  let app = await launchApp({ env, dataDir });
  const appInfo = await invoke(app.page, "app_info");
  if (!appInfo.debugBuild) {
    await app.close();
    throw new Error(`${NAME} requires a debug build (a release build ignores the credential namespace)`);
  }
  let keySaved = false;
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });

    // ---- Save the key through the real dialog.
    await panel.getByRole("button", { name: "Agent provider settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Agent provider" });
    await dialog.waitFor();
    await dialog.getByLabel("API key").fill(FAKE_KEY);
    await dialog.getByRole("button", { name: "Save key" }).click();
    await page.waitForFunction(() => {
      const text = document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "";
      return text.includes("Configured") && !text.includes("Not configured");
    });
    keySaved = true;
    const status = await invoke(page, "agents_provider_status");
    check("status-has-no-key", !JSON.stringify(status).includes(core), `configured=${status.configured}`);
    const domLeak = await page.evaluate(
      (needle) =>
        document.documentElement.outerHTML.includes(needle) ||
        [...document.querySelectorAll("input, textarea")].some((el) => el.value.includes(needle)),
      core,
    );
    check("dom-has-no-key-after-save", !domLeak);
    await dialog.getByRole("button", { name: "Close" }).click();

    // ---- A trusted project with a Python research kernel.
    await invoke(page, "workspace_open", { path: project });
    const ws = await invoke(page, "research_workspace");
    await invoke(page, "trust_grant", { root: ws.root });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    if (!python) {
      info("python-kernel", "no usable Python environment; child checks skipped");
    } else {
      const session = await invoke(page, "research_session_start", {
        request: { environmentId: python.id, owner: { kind: "user" } },
      });
      await waitFor(
        async () => {
          const s = (await invoke(page, "research_sessions")).find((x) => x.id === session.id);
          return s && (s.state.kind ?? s.state) === "idle" ? s : null;
        },
        { timeout: KERNEL_TIMEOUT, message: "kernel idle" },
      ).catch(() => null);

      // 1. Environment: nothing key-like and not the key itself.
      const envRun = await run(
        page,
        session.id,
        [
          "import os, hashlib",
          // Compare hashes so the key text never appears in the stored run input.
          `print("ENV_HAS_CORE=" + str(any(hashlib.sha256(v.strip().encode()).hexdigest() == ${JSON.stringify(keySha)} or "sk-ant-" in v for v in os.environ.values())))`,
          'print("ENV_NAMES=" + ",".join(sorted(k for k in os.environ if any(s in k.upper() for s in ("KEY","TOKEN","SECRET","ANTHROPIC")))))',
        ].join("\n"),
      );
      check("kernel-env-has-no-key", envRun.text.includes("ENV_HAS_CORE=False"), envRun.text.match(/ENV_NAMES=[^"\\]*/)?.[0]);

      // 2. Same-user native code can read the stored key from Credential Manager
      // (only this scenario's namespaced entry is read; no enumeration).
      const credRun = await run(
        page,
        session.id,
        `
import ctypes, ctypes.wintypes as w, hashlib
class CREDENTIALW(ctypes.Structure):
    _fields_ = [("Flags", w.DWORD), ("Type", w.DWORD), ("TargetName", w.LPWSTR), ("Comment", w.LPWSTR),
                ("LastWritten", w.FILETIME), ("CredentialBlobSize", w.DWORD),
                ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)), ("Persist", w.DWORD),
                ("AttributeCount", w.DWORD), ("Attributes", ctypes.c_void_p), ("TargetAlias", w.LPWSTR),
                ("UserName", w.LPWSTR)]
adv = ctypes.WinDLL("advapi32", use_last_error=True)
adv.CredReadW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, ctypes.POINTER(ctypes.POINTER(CREDENTIALW))]
p = ctypes.POINTER(CREDENTIALW)()
_rsec_value = None
if adv.CredReadW(${JSON.stringify(target)}, 1, 0, ctypes.byref(p)):
    blob = bytes(p.contents.CredentialBlob[:p.contents.CredentialBlobSize])
    persist = p.contents.Persist
    adv.CredFree(p)
    for enc in ("utf-16-le", "utf-8"):
        try:
            cand = blob.decode(enc)
        except UnicodeDecodeError:
            continue
        if hashlib.sha256(cand.encode()).hexdigest() == ${JSON.stringify(keySha)}:
            _rsec_value = cand
    print("CRED_READ=1 MATCH=" + str(_rsec_value is not None) + " PERSIST=" + str(persist))
else:
    print("CRED_READ=0 ERR=" + str(ctypes.get_last_error()))
`,
      );
      const read = /CRED_READ=1 MATCH=True/.test(credRun.text);
      if (read) {
        info(
          "trusted-native-child-reads-key",
          "a trusted Python kernel read the stored API key from Windows Credential Manager (CredReadW, same user); " +
            (credRun.text.match(/PERSIST=\d/)?.[0] ?? ""),
        );
      } else {
        ok("trusted-native-child-cannot-read-key", credRun.text.match(/CRED_READ=[^"\\]*/)?.[0]);
      }

      if (read) {
        // 3. Redaction of a key that a child prints, raises, or displays.
        const printed = await run(page, session.id, 'print("RAW=" + _rsec_value)');
        check("stream-output-redacts-key", !printed.text.includes(core), printed.text.match(/RAW=[^"\\]*/)?.[0]);
        const raised = await run(page, session.id, "raise ValueError(_rsec_value)");
        check("traceback-redacts-key", !raised.text.includes(core) && !JSON.stringify(raised.manifest).includes(core));
        const shown = await run(
          page,
          session.id,
          'from IPython.display import HTML, display\ndisplay(HTML("<b>" + _rsec_value + "</b>"))\n_rsec_value',
        );
        const shownArtifacts = [];
        for (const entry of shown.output.entries ?? []) {
          for (const artifact of entry.item?.artifacts ?? []) shownArtifacts.push(artifact);
        }
        check(
          "display-output-redacts-key",
          !shown.text.includes(core),
          `display entries=${(shown.output.entries ?? []).length}, artifacts=${shownArtifacts.length}`,
        );
        // Artifacts are fetched separately; check what the webview would show.
        let artifactLeak = false;
        for (const artifact of shownArtifacts) {
          const artifactId = typeof artifact === "string" ? artifact : (artifact.id ?? artifact.artifactId);
          const text = await page.evaluate(
            async ([runId, id]) => {
              try {
                const bytes = await window.__TAURI_INTERNALS__.invoke("research_artifact", { runId, artifactId: id });
                return new TextDecoder().decode(bytes instanceof ArrayBuffer ? new Uint8Array(bytes) : new Uint8Array(bytes));
              } catch (error) {
                return `ERROR ${JSON.stringify(error)}`;
              }
            },
            [shown.manifest.record.id, artifactId],
          );
          if (text.includes(core)) artifactLeak = true;
        }
        // A child that holds the key can write it anywhere; rich display output
        // (HTML artifacts) is stored and shown as produced, so this is recorded,
        // not judged.
        if (shownArtifacts.length) {
          (artifactLeak ? info : ok)("display-artifact-redacts-key", `${shownArtifacts.length} artifact(s); key ${artifactLeak ? "present" : "absent"} in artifact bytes`);
        }
        await invoke(page, "research_run_output", { runId: shown.manifest.record.id, after: null, limit: 200 });
        await page.waitForTimeout(500);
        const domAfter = await page.evaluate((needle) => document.documentElement.outerHTML.includes(needle), core);
        check("dom-has-no-key-after-runs", !domAfter);
      }
      await invoke(page, "research_session_shutdown", { sessionId: session.id }).catch(() => {});
    }

    const logs = app.logs.join("");
    check("app-stdout-stderr-have-no-key", !logs.includes(core));
    check("console-has-no-key", !app.consoleMessages.join("\n").includes(core));
  } finally {
    await app.close();
  }

  // ---- What stayed on disk (app data incl. WebView2 profile, project).
  const dataHits = await filesContaining(dataDir, core);
  const projectHits = await filesContaining(project, core);
  const rel = (paths, root) => paths.map((p) => p.slice(root.length + 1));
  const childOutputHits = [...rel(dataHits, dataDir), ...rel(projectHits, project)].filter((p) =>
    /research|runs|artifact|\.exposition/i.test(p),
  );
  const otherHits = [...rel(dataHits, dataDir), ...rel(projectHits, project)].filter((p) => !childOutputHits.includes(p));
  check("key-not-on-disk-outside-child-outputs", otherHits.length === 0, otherHits.join(", "));
  (childOutputHits.length === 0 ? ok : info)("key-not-on-disk-in-stored-child-outputs", childOutputHits.join(", "));

  // ---- Remove the fake key (same namespace) and confirm.
  if (keySaved) {
    app = await launchApp({ env, dataDir });
    try {
      const cleared = await invoke(app.page, "agents_provider_clear_key");
      check("fake-key-removed", cleared.configured === false);
    } finally {
      await app.close();
    }
  }

  await writeFile(join(artifactsDir, "findings.json"), JSON.stringify({ namespace, base, findings }, null, 2));
  const defects = findings.filter((f) => f.verdict === "DEFECT");
  if (defects.length) throw new Error(`${defects.length} DEFECT(s): ${defects.map((d) => d.id).join(", ")}`);
}
