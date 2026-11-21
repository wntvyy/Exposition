// R-a1 (independent review of A1) against the built app (debug build, real
// IPC, real task loop and gate, real research kernels, real Docker when
// present) with the scripted provider — development only. The scripted
// provider replaces only the model.
//
// A. Permission boundary over IPC: another folder / a non-open root / a copy
//    of the project (with its .exposition) gets no grant; an Ask task continued
//    after the project gains Auto permissions still asks; "Approve for this
//    task" on a consequence decision is refused; an agent tool named like a
//    permission command or carrying an extra field changes nothing;
//    app_open_external refuses non-web addresses; the webview cannot forge a
//    run authorization.
// B. Revocation and lifecycle with real OS processes: revoking the project's
//    permissions (and, separately, folder trust) while an Auto task runs Python
//    natively stops the run, the kernel, the helper AND a child process the
//    code started; the queued call of the same batch never runs; Continue of
//    the stopped Auto task is refused; provenance records the agent's
//    authorization and not the user's.
// C. Restricted execution via the agent route (Auto, Code: restricted,
//    docker image python:3.13-slim): host files (junction, Windows path,
//    /mnt/c, admin share), network (DNS, TCP), project writes, environment,
//    Docker socket, process count; with no image, refused and nothing starts.
//
// Product defects are reported as `DEFECT` lines (the scenario still passes);
// held checks fail the scenario.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const done = { steps: [{ text: "done" }] };
const slash = (p) => p.replace(/\\/g, "/");

function pidAlive(pid) {
  if (!pid) return false;
  try {
    const out = execFileSync(
      "powershell",
      ["-NoProfile", "-Command", `if (Get-Process -Id ${Number(pid)} -ErrorAction SilentlyContinue) { 'yes' } else { 'no' }`],
      { encoding: "utf8" },
    );
    return out.trim() === "yes";
  } catch {
    return false;
  }
}

function killPid(pid) {
  if (!pid) return;
  try {
    execFileSync("taskkill", ["/F", "/T", "/PID", String(pid)], { stdio: "ignore" });
  } catch {}
}

function dockerImagePresent() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
    return true;
  } catch {
    return false;
  }
}

export default async function reviewA1({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(process.env.REVIEW_A1_SCRATCH ?? tmpdir(), "xp-review-a1-"));
  const root = join(base, "project");
  const copy = join(base, "project-copy");
  const other = join(base, "other");
  const outside = join(base, "outside");
  const dataDir = join(base, "data");
  for (const dir of [root, other, outside, dataDir, join(root, "notes")]) await mkdir(dir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Notes\n\nThe squares grow quickly.\n");
  await writeFile(join(root, "notes", "a.typ"), "= A\n");
  await writeFile(join(other, "main.typ"), "= Other\n");
  await writeFile(join(outside, "canary.txt"), "R-A1-CANARY outside the project\n");
  const spawned = [];

  const observations = [];
  const note = (id, ok, detail, defect = false) => {
    observations.push({ id, ok: !!ok, defect, detail });
    const tag = ok ? "ok    " : defect ? "DEFECT" : "FAIL  ";
    console.log(`${tag} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
  };

  const app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-review-a1-${Date.now()}` } });
  const { page } = app;
  const call = (command, args) => invoke(page, command, args);
  const fail = (command, args) => invokeError(page, command, args);

  const request = (workspaceRoot, mode, execution, prompt = "review") => ({
    prompt,
    workspaceRoot,
    scope: { kind: "workspace" },
    permission: "autoApply",
    execution,
    consent: "scopeContent",
    budgets: null,
    approvalMode: mode,
  });
  const detail = (id) => call("agents_task_detail", { task: id });
  const toolCalls = async (id) => (await detail(id)).transcript.filter((i) => i.kind === "toolCall");
  const status = async (id) => (await call("agents_task_list")).find((t) => t.id === id)?.status?.kind;
  const active = (kind) => ["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(kind);
  /** Run a task; `answer(approval)` returns true / false / "task" / null (leave pending). */
  const runTask = async (req, script, answer = () => false, timeout = 180_000) => {
    await call("agents_dev_load_script", { script });
    const task = await call("agents_task_create", { request: req });
    const cards = [];
    const deadline = Date.now() + timeout;
    for (;;) {
      const d = await detail(task.id);
      for (const approval of d.approvals ?? []) {
        if (cards.some((c) => c.id === approval.id)) continue;
        cards.push(approval);
        const a = answer(approval);
        if (a === null) continue;
        await call("agents_approval_resolve", {
          task: task.id,
          approval: approval.id,
          approved: a === true || a === "task",
          scope: a === "task" ? "task" : "once",
        });
      }
      if (!active(d.summary.status.kind)) return { id: task.id, summary: d.summary, cards, calls: await toolCalls(task.id) };
      if (Date.now() > deadline) throw new Error(`task ${task.id} did not finish`);
      await page.waitForTimeout(250);
    }
  };
  const agentSession = async (taskId) =>
    (await call("research_sessions")).find((s) => s.owner?.kind === "agent" && s.owner.taskId === taskId);

  try {
    const info = await call("app_info");
    assert.ok(info.debugBuild, "review-a1 needs a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 900 }).catch(() => {});
    await call("agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await call("workspace_open", { path: root });
    const rootText = opened.root;
    await call("trust_grant", { root: rootText });

    // ================= A. permission boundary =================
    const none = await call("agents_permissions_get", { root: rootText });
    note("A-new-project-has-no-grant", none.permissions === null && none.project, { project: none.project, permissions: none.permissions });

    const otherSet = await fail("agents_permissions_set", { root: other, choice: { files: "edit", code: "native", setup: false } });
    note("A-set-for-a-non-open-folder-is-refused", !!otherSet.error, otherSet.error ?? otherSet.unexpectedSuccess);
    const otherGet = await fail("agents_permissions_get", { root: other });
    note("A-get-for-a-non-open-folder-is-refused", !!otherGet.error, otherGet.error ?? otherGet.unexpectedSuccess);
    const otherTask = await fail("agents_task_create", { request: request(other, "auto", "approveEach") });
    note("A-auto-task-in-a-non-open-folder-is-refused", !!otherTask.error, otherTask.error ?? "created");

    // An Ask task that finished before the project had Auto permissions.
    // (The scripted provider keeps one script per task across runs: Continue serves its next turns.)
    const askTask = await runTask(request(rootText, "ask", "approveEach", "say hi"), {
      turns: [{ steps: [{ text: "hi" }] }, { steps: [tool("p", "run_python", { code: "print(40 + 2)" })] }, done],
    });
    note("A-control-ask-task-completed", askTask.summary.status.kind === "completed", askTask.summary.status);

    const granted = await call("agents_permissions_set", { root: rootText, choice: { files: "edit", code: "native", setup: false } });
    note("A-grant-for-the-open-project", granted.permissions?.code === "native", granted.permissions);
    const version = granted.permissions.version;

    // Continue the Ask task: it must still ask (Ask stays Ask).
    await call("agents_task_continue", { task: askTask.id, prompt: "now compute", budgets: null });
    const askCard = await waitFor(async () => (await detail(askTask.id)).approvals?.[0], { timeout: 60_000, message: "the Ask task's card" }).catch(() => null);
    note("A-continued-ask-task-still-asks-after-auto-grant", !!askCard, askCard ? askCard.title : "ran without a card");
    if (askCard) await call("agents_approval_resolve", { task: askTask.id, approval: askCard.id, approved: false, scope: "once" });
    await waitFor(async () => !active(await status(askTask.id)), { timeout: 60_000, message: "ask task stops" });

    // An agent tool named like a permission command, and an extra field on a real tool.
    const forged = await runTask(request(rootText, "auto", "approveEach"), {
      turns: [
        {
          steps: [
            tool("x1", "agents_permissions_set", { root: rootText, choice: { files: "edit", code: "native", setup: true } }),
            tool("x2", "run_python", { code: "print(1)", approved: true }),
            tool("x3", "create_file", { path: "notes/x.typ", content: "x", permissions: { setup: true } }),
          ],
        },
        done,
      ],
    });
    const after = await call("agents_permissions_get", { root: rootText });
    note(
      "A-unknown-tool-and-extra-fields-change-nothing",
      forged.calls.every((c) => c.isError || c.status === "failed") && after.permissions.version === version && !after.permissions.setup,
      { calls: forged.calls.map((c) => [c.name, c.status, (c.result ?? "").slice(0, 120)]), version: after.permissions.version },
    );
    note("A-extra-field-tool-call-did-not-run", !existsSync(join(root, "notes", "x.typ")), "notes/x.typ absent");

    // A consequence decision (delete with unsaved changes) cannot be approved "for this task".
    const doc = await call("document_open", { path: "notes/a.typ" });
    await call("document_apply", {
      request: {
        id: `review-a1-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: doc.state.info.id, baseRevision: doc.state.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "// unsaved\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await call("agents_dev_load_script", {
      script: { turns: [{ steps: [tool("d", "delete_paths", { paths: ["notes/a.typ"] })] }, done] },
    });
    const pendingTask = await call("agents_task_create", { request: request(rootText, "auto", "approveEach", "delete") });
    const pending = await waitFor(async () => (await detail(pendingTask.id)).approvals?.[0], {
      timeout: 60_000,
      message: "the deletion's card",
    }).catch(() => null);
    if (pending) {
      const repeat = await fail("agents_approval_resolve", { task: pendingTask.id, approval: pending.id, approved: true, scope: "task" });
      const still = (await detail(pendingTask.id)).approvals?.find((a) => a.id === pending.id);
      note("A-consequence-approval-for-task-is-refused", repeat.error?.code === "agents.notRepeatable" && !!still, { error: repeat.error, stillPending: !!still, forTask: pending.forTask });
      await call("agents_approval_resolve", { task: pendingTask.id, approval: pending.id, approved: false, scope: "once" });
      await waitFor(async () => !active(await status(pendingTask.id)), { timeout: 30_000, message: "delete task stops" });
    } else {
      note("A-consequence-approval-for-task-is-refused", false, { status: await status(pendingTask.id), calls: await toolCalls(pendingTask.id) });
    }
    note("A-declined-delete-kept-the-file", existsSync(join(root, "notes", "a.typ")), "notes/a.typ present");

    // app_open_external: only http(s) addresses.
    const hostileUrls = [
      "file:///C:/Windows/System32/calc.exe",
      "javascript:alert(1)",
      "data:text/html,<script>1</script>",
      "ms-settings:privacy",
      "\\\\localhost\\C$\\Windows",
      "http://user:pass@example.com/",
      "https://exa mple.com/",
      "https://example.com/\u202Egpj.exe",
      "tauri://localhost/",
      "http://",
      "C:\\Windows\\System32\\calc.exe",
      "search-ms:query=x",
    ];
    const urlResults = [];
    for (const url of hostileUrls) {
      const r = await fail("app_open_external", { url });
      urlResults.push([url, r.error?.code ?? "OPENED"]);
    }
    note("A-open-external-refuses-non-web-addresses", urlResults.every(([, c]) => c === "app.invalidUrl"), urlResults);

    // The webview cannot attach an authorization (or a requester) to a run.
    const envs = await call("research_environments", { refresh: false });
    const python = envs.runtimes.find((r) => r.language === "python" && r.availability?.kind === "available");
    let userSession = null;
    if (python) {
      userSession = await call("research_session_start", { request: { environmentId: python.id } });
      const forgedAuth = await fail("research_execute", {
        request: {
          sessionId: userSession.id,
          source: { kind: "input", code: "print('forged')" },
          authorization: { principal: "agent", taskId: "task-x", mode: "auto", basis: "projectPermissions", permissionsVersion: version },
        },
      });
      const forgedRequester = await fail("research_execute", {
        request: { sessionId: userSession.id, source: { kind: "input", code: "print('forged')" }, requester: { kind: "agent", taskId: "task-x" } },
      });
      note("A-webview-cannot-forge-run-authorization", !!forgedAuth.error && !!forgedRequester.error, { authorization: forgedAuth.error, requester: forgedRequester.error });
      const userRun = await call("research_execute", { request: { sessionId: userSession.id, source: { kind: "input", code: "print('user run')" } } });
      const userFinished = await waitFor(async () => {
        const { runs } = await call("research_runs");
        const r = runs.find((x) => x.id === userRun.id);
        return r && r.status.kind !== "running" ? r : null;
      }, { timeout: 60_000, message: "user run finishes" });
      const userManifest = await call("research_run", { runId: userRun.id });
      note("A-user-run-records-no-agent-authorization", userFinished.status.kind === "succeeded" && !userManifest.authorization, { status: userFinished.status, authorization: userManifest.authorization ?? null });
      await call("research_session_shutdown", { sessionId: userSession.id }).catch(() => {});
    } else {
      note("A-python-environment-available", false, "SETUP-FAIL: no available Python environment");
    }

    // ================= B. revocation and lifecycle =================
    const project = (await call("agents_permissions_get", { root: rootText })).project;
    const quick = await runTask(request(rootText, "auto", "approveEach"), {
      turns: [{ steps: [tool("q", "run_python", { code: "print(6 * 7)" })] }, done],
    });
    const quickResult = JSON.parse(quick.calls[0]?.result ?? "{}");
    const quickManifest = quickResult.run ? await call("research_run", { runId: quickResult.run }) : null;
    note(
      "B-agent-native-run-records-its-authorization",
      quick.cards.length === 0 && quickManifest?.authorization?.basis === "projectPermissions" && quickManifest.authorization.taskId === quick.id,
      { cards: quick.cards.length, authorization: quickManifest?.authorization, authorizedBy: quick.calls[0]?.authorizedBy },
    );

    const slowPython = (pidFile) =>
      [
        "import subprocess, sys, time",
        "c = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)'])",
        `open(r'${pidFile}', 'w').write(str(c.pid))`,
        "time.sleep(120)",
        "print('finished')",
      ].join("\n");
    const lifecycle = async (label, revoke) => {
      const pidFile = join(outside, `${label}-child.pid`);
      const queuedMarker = join(outside, `${label}-queued-ran.txt`);
      await call("agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                tool("slow", "run_python", { code: slowPython(slash(pidFile)) }),
                tool("queued", "run_python", { code: `open(r'${slash(queuedMarker)}', 'w').write('ran')` }),
              ],
            },
            done,
          ],
        },
      });
      const task = await call("agents_task_create", { request: request(rootText, "auto", "approveEach", `${label} long run`) });
      await waitFor(() => existsSync(pidFile), { timeout: 120_000, interval: 300, message: `${label}: child pid file` });
      const childPid = Number(await readFile(pidFile, "utf8"));
      const session = await agentSession(task.id);
      const pids = { kernel: session?.kernelPid, helper: session?.helperPid, child: childPid };
      spawned.push(...Object.values(pids));
      note(`B-${label}-control-processes-alive-before`, pids.kernel && pidAlive(pids.kernel) && pidAlive(childPid), pids);
      const t0 = Date.now();
      await revoke();
      const gone = await waitFor(
        () => !pidAlive(pids.kernel) && !pidAlive(pids.helper) && !pidAlive(pids.child),
        { timeout: 20_000, interval: 500, message: `${label}: processes exit` },
      ).then(() => true, () => false);
      const left = Object.fromEntries(Object.entries(pids).map(([k, v]) => [k, pidAlive(v)]));
      note(`B-${label}-kernel-helper-and-child-exit`, gone, { alive: left, ms: Date.now() - t0 }, true);
      await waitFor(async () => !active(await status(task.id)), { timeout: 60_000, message: `${label}: task stops` }).catch(() => {});
      await page.waitForTimeout(3_000);
      const calls = await toolCalls(task.id);
      note(`B-${label}-queued-call-never-ran`, !existsSync(queuedMarker), { calls: calls.map((c) => [c.name, c.status, (c.result ?? "").slice(0, 160)]) });
      const live = (await call("research_sessions")).filter(
        (s) => s.owner?.kind === "agent" && s.owner.taskId === task.id && !["stopped", "detached", "crashed"].includes(s.state.kind),
      );
      note(`B-${label}-no-live-agent-session-remains`, live.length === 0, live.map((s) => [s.id, s.state]));
      return task.id;
    };

    const revokedTask = await lifecycle("revoke-permissions", () => call("agents_permissions_revoke", { root: rootText, project }));
    note("B-revoke-permissions-canceled-the-auto-task", (await status(revokedTask)) === "canceled", await status(revokedTask));
    const continued = await fail("agents_task_continue", { task: revokedTask, prompt: "go on", budgets: null });
    note("B-continue-after-revoke-is-refused", continued.error?.code === "agents.permissionsRequired", continued.error ?? "continued");

    await call("agents_permissions_set", { root: rootText, choice: { files: "edit", code: "native", setup: false } });
    await lifecycle("revoke-trust", () => call("trust_revoke", { root: rootText }));
    await call("trust_grant", { root: rootText });

    // ================= A (copy). A copied project folder has no grant =================
    await call("agents_permissions_set", { root: rootText, choice: { files: "edit", code: "native", setup: false } });
    await cp(root, copy, { recursive: true });
    const copyOpened = await call("workspace_open", { path: copy });
    const copyState = await call("agents_permissions_get", { root: copyOpened.root });
    note("A-copy-of-the-project-has-no-grant", copyState.permissions === null, { project: copyState.project, originalProject: project, permissions: copyState.permissions });
    const copyTask = await fail("agents_task_create", { request: request(copyOpened.root, "auto", "approveEach") });
    note("A-auto-task-in-the-copy-is-refused", copyTask.error?.code === "agents.permissionsRequired", copyTask.error ?? "created");
    const backAgain = await call("workspace_open", { path: root });
    const originalState = await call("agents_permissions_get", { root: backAgain.root });
    note("A-original-keeps-its-grant", originalState.permissions?.code === "native", originalState.permissions);

    // ================= C. restricted execution via the agent =================
    if (!dockerImagePresent()) {
      note("C-docker-image-present", false, `SETUP-FAIL: docker image ${IMAGE} is not available locally`);
    } else {
      execFileSync("cmd", ["/c", "mklink", "/J", join(root, "escape"), outside], { stdio: "ignore" });
      await call("execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
      await call("agents_permissions_set", { root: rootText, choice: { files: "edit", code: "restricted", setup: false } });
      const canaryWin = slash(join(outside, "canary.txt"));
      const canaryRel = canaryWin.replace(/^[A-Za-z]:\//, "");
      const probe = [
        "import json, os, socket, subprocess",
        "r = {}",
        "def tryread(p):",
        "    try:",
        "        with open(p) as f:",
        "            return 'READ:' + f.read(40)",
        "    except Exception as e:",
        "        return type(e).__name__",
        "r['junction'] = tryread('/workspace/escape/canary.txt')",
        `r['winpath'] = tryread(${JSON.stringify(canaryWin)})`,
        `r['mntc'] = tryread(${JSON.stringify("/mnt/c/" + canaryRel)})`,
        `r['adminShare'] = tryread(${JSON.stringify("//localhost/C$/" + canaryRel)})`,
        `r['hostmnt'] = tryread(${JSON.stringify("/mnt/host/c/" + canaryRel)})`,
        "try:",
        "    open('/workspace/probe-write.txt', 'w').write('x')",
        "    r['projectWrite'] = 'WROTE'",
        "except Exception as e:",
        "    r['projectWrite'] = type(e).__name__",
        "try:",
        "    socket.getaddrinfo('example.com', 443)",
        "    r['dns'] = 'RESOLVED'",
        "except Exception as e:",
        "    r['dns'] = type(e).__name__",
        "try:",
        "    s = socket.create_connection(('1.1.1.1', 443), timeout=3)",
        "    r['tcp'] = 'CONNECTED'",
        "    s.close()",
        "except Exception as e:",
        "    r['tcp'] = type(e).__name__",
        "r['env'] = sorted(os.environ.keys())",
        "r['dockerSocket'] = [p for p in ('/var/run/docker.sock', '/run/docker.sock') if os.path.exists(p)]",
        "r['uid'] = os.getuid()",
        "r['mounts'] = [l.split()[1] for l in open('/proc/mounts') if l.split()[1].startswith(('/workspace', '/out', '/exposition', '/mnt', '/host'))]",
        "procs = []",
        "try:",
        "    for i in range(3000):",
        "        procs.append(subprocess.Popen(['sleep', '20']))",
        "except Exception as e:",
        "    r['spawnStopped'] = type(e).__name__",
        "r['spawned'] = len(procs)",
        "for p in procs:",
        "    try:",
        "        p.kill()",
        "    except Exception:",
        "        pass",
        "print('PROBE' + json.dumps(r))",
      ].join("\n");
      const jobsBefore = (await call("job_list")).length;
      const restricted = await runTask(request(rootText, "auto", "approveEach", "probe"), {
        turns: [{ steps: [tool("probe", "run_python", { code: probe })] }, done],
      }, () => false, 300_000);
      const result = JSON.parse(restricted.calls[0]?.result ?? "{}");
      const line = (result.stdout ?? "").split("\n").find((l) => l.startsWith("PROBE"));
      const r = line ? JSON.parse(line.slice(5)) : null;
      await writeFile(join(artifactsDir, "restricted-probe.json"), JSON.stringify({ result, probe: r }, null, 2));
      note("C-auto-restricted-ran-without-a-card-in-docker", restricted.cards.length === 0 && result.policy === "restricted:docker" && !!r, { cards: restricted.cards.length, policy: result.policy, authorizedBy: restricted.calls[0]?.authorizedBy, stderr: (result.stderr ?? "").slice(-300) });
      if (r) {
        const noRead = (v) => !String(v).startsWith("READ:");
        note("C-host-file-via-junction-not-readable", noRead(r.junction), r.junction, true);
        note("C-host-file-via-windows-path-mnt-admin-share-not-readable", [r.winpath, r.mntc, r.adminShare, r.hostmnt].every(noRead), [r.winpath, r.mntc, r.adminShare, r.hostmnt], true);
        note("C-project-not-writable", r.projectWrite !== "WROTE" && !existsSync(join(root, "probe-write.txt")), r.projectWrite, true);
        note("C-no-network", r.dns !== "RESOLVED" && r.tcp !== "CONNECTED", { dns: r.dns, tcp: r.tcp }, true);
        // GPG_KEY / PYTHON_SHA256 are the official python image's own ENV (the public release-signing key id
        // and the source checksum), not host variables.
        const imageDefined = new Set(["GPG_KEY", "PYTHON_SHA256", "PYTHON_VERSION", "LANG", "PATH", "HOME", "HOSTNAME", "PWD", "TMPDIR"]);
        const leaky = r.env.filter(
          (k) => !imageDefined.has(k) && /KEY|TOKEN|SECRET|PASS|ANTHROPIC|EXPOSITION|WEBVIEW2|USERPROFILE|APPDATA|CLAUDE|AWS|AZURE/i.test(k),
        );
        note("C-no-host-or-secret-environment", leaky.length === 0, { env: r.env }, true);
        note("C-no-docker-socket", r.dockerSocket.length === 0, r.dockerSocket, true);
        note("C-non-root", r.uid !== 0, r.uid, true);
        note("C-process-count-bounded", r.spawned < 3000, { spawned: r.spawned, stopped: r.spawnStopped, mounts: r.mounts }, true);
      }
      const jobs = await call("job_list");
      note("C-a-job-ran-not-a-kernel", jobs.length > jobsBefore, { before: jobsBefore, after: jobs.length });

      // No image: refused, never native, nothing starts.
      await call("execution_settings_update", { change: { kind: "dockerImage", image: null } });
      const jobsMid = (await call("job_list")).length;
      const sessionsMid = (await call("research_sessions")).length;
      const marker = join(outside, "no-image-ran.txt");
      const noImage = await runTask(request(rootText, "auto", "approveEach", "no image"), {
        turns: [{ steps: [tool("n", "run_python", { code: `open(r'${slash(marker)}', 'w').write('ran')` })] }, done],
      });
      note(
        "C-no-image-refused-never-native",
        noImage.cards.length === 0 && /agents\.restrictedUnavailable/.test(noImage.calls[0]?.result ?? "") && !existsSync(marker) &&
          (await call("job_list")).length === jobsMid && (await call("research_sessions")).length === sessionsMid,
        { cards: noImage.cards.map((c) => c.title), result: (noImage.calls[0]?.result ?? "").slice(0, 200) },
      );
    }
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2)).catch(() => {});
    await app.close();
    for (const pid of spawned) if (pidAlive(pid)) killPid(pid);
    await rm(base, { recursive: true, force: true }).catch(() => {});
  }
  const failed = observations.filter((o) => !o.ok && !o.defect);
  assert.deepEqual(failed, [], `held checks failed: ${JSON.stringify(failed).slice(0, 1200)}`);
  console.log(`review-a1: ${observations.filter((o) => o.ok).length} ok, ${observations.filter((o) => o.defect && !o.ok).length} DEFECT`);
}
