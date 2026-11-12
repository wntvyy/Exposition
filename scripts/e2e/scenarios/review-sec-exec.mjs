// R-sec (M6 security review): opening a hostile project never runs its code,
// project-provided shims are never chosen as tools (before or after trust),
// and child processes never inherit the app's credentials or interpreter
// influence variables. Debug build; runs only when named:
//   npm run e2e -- review-sec-exec
//
// The hostile project carries a marker stub (a tiny compiled .exe that writes
// RAN-<name>-<pid>.txt beside itself when executed) under the names of every
// tool Exposition runs, in the project root, .venv/venv, bin/ and Scripts/;
// plus a lakefile/Lean file/TeX file whose code writes a marker when executed,
// .latexmkrc/texmf.cnf/pyvenv.cfg/sitecustomize.py/kernelspecs, and a hostile
// .exposition/project.json. The app is launched with fake credential-like and
// PYTHONPATH/PERL5OPT/... variables.
//
// Prints `ok: ...`, `info: ...`, and `DEFECT: ...` lines; fails if any DEFECT.
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { copyFile, cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

import { waitForAsync } from "../wait.mjs";
import { invoke, invokeError } from "../workspace-helpers.mjs";

const STUB_SOURCE = `
fn main() {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().to_path_buf();
    let stem = exe.file_stem().unwrap().to_string_lossy().to_string();
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let text = format!("{:?}\\ncwd={}\\n", args, cwd);
    let _ = std::fs::write(dir.join(format!("RAN-{}-{}.txt", stem, std::process::id())), &text);
    if let Some(global) = option_env!("REVIEW_SEC_MARKER_DIR") {
        let _ = std::fs::write(std::path::Path::new(global).join(format!("RAN-{}-{}.txt", stem, std::process::id())), format!("{}\\n{}", exe.display(), text));
    }
    println!("{} 99.0 (review-sec stub)", stem);
}
`;

const SHIM_NAMES = [
  "python", "python3", "py", "git", "where", "cmd", "conhost", "pdflatex", "lualatex", "latexmk", "biber",
  "bibtex", "kpsewhich", "mktexfmt", "perl", "lean", "lake", "elan", "docker", "Rscript", "R", "julia",
  "jupyter", "jupyter-kernel", "node", "powershell", "pwsh", "where", "tasklist", "taskkill", "wmic",
];

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    if (st.isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

export default async function reviewSecExec({ launchApp, artifactsDir }) {
  if (!process.argv.slice(2).includes("review-sec-exec")) {
    console.log("skip review-sec-exec (run by name)");
    return;
  }
  const defects = [];
  const defect = (m) => {
    defects.push(m);
    console.log(`DEFECT: ${m}`);
  };
  const ok = (m) => console.log(`ok: ${m}`);
  const info = (m) => console.log(`info: ${m}`);

  const base = await mkdtemp(join(tmpdir(), "xp-rsec-exec-"));
  const project = join(base, "hostile-project");
  const markers = join(base, "markers");
  const evil = join(base, "evil-host-dir");
  await mkdir(project, { recursive: true });
  await mkdir(markers, { recursive: true });
  await mkdir(evil, { recursive: true });

  // --- the marker stub
  const stubSrc = join(base, "stub.rs");
  const stub = join(base, "stub.exe");
  await writeFile(stubSrc, STUB_SOURCE);
  execFileSync("rustc", ["-O", stubSrc, "-o", stub], { env: { ...process.env, REVIEW_SEC_MARKER_DIR: markers } });

  // --- hostile project
  for (const dir of ["", "bin", "Scripts", ".venv/Scripts", "venv/Scripts", "env/Scripts", ".elan/bin", "node_modules/.bin"]) {
    await mkdir(join(project, dir), { recursive: true });
    for (const name of SHIM_NAMES) await copyFile(stub, join(project, dir, `${name}.exe`));
  }
  await writeFile(join(project, ".venv", "pyvenv.cfg"), `home = ${join(project, "bin")}\ninclude-system-site-packages = false\n`);
  await writeFile(join(project, "pyvenv.cfg"), `home = ${join(project, "bin")}\n`);
  for (const mod of ["sitecustomize", "usercustomize", "json", "jupyter_client", "zmq", "ipykernel", "encodings_hook"]) {
    await writeFile(join(project, `${mod}.py`), `open(r"${join(markers, `PY-IMPORTED-${mod}.txt`)}", "w").write("x")\n`);
  }
  await writeFile(
    join(project, "lakefile.lean"),
    `import Lake\nopen Lake DSL\n#eval (IO.FS.writeFile "${join(markers, "LAKEFILE-RAN.txt").replace(/\\/g, "/")}" "x" : IO Unit)\npackage hostile\nlean_lib Hostile\n`,
  );
  await writeFile(join(project, "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  await mkdir(join(project, "Hostile"), { recursive: true });
  await writeFile(
    join(project, "Hostile.lean"),
    `#eval (IO.FS.writeFile "${join(markers, "LEAN-ELAB-RAN.txt").replace(/\\/g, "/")}" "x" : IO Unit)\ntheorem t : 1 = 1 := rfl\n`,
  );
  const m = (name) => join(markers, name).replace(/\\/g, "/");
  await writeFile(
    join(project, "main.tex"),
    [
      "\\documentclass{article}",
      "\\begin{document}",
      `\\immediate\\write18{cmd /c echo x > "${m("TEX-WRITE18-RAN.txt")}"}`,
      "Hello.",
      "\\end{document}",
      "",
    ].join("\n"),
  );
  await writeFile(join(project, ".latexmkrc"), `system("cmd /c echo x > ${m("LATEXMKRC-RAN.txt")}");\n`);
  await writeFile(join(project, "latexmkrc"), `system("cmd /c echo x > ${m("LATEXMKRC2-RAN.txt")}");\n`);
  await writeFile(join(project, "texmf.cnf"), "shell_escape = t\nopenout_any = a\nopenin_any = a\n");
  await writeFile(join(project, ".Rprofile"), `writeLines("x", "${m("RPROFILE-RAN.txt")}")\n`);
  await writeFile(join(project, "startup.jl"), `write("${m("JULIA-STARTUP-RAN.txt")}", "x")\n`);
  for (const k of ["kernels/evil", "share/jupyter/kernels/evil", ".jupyter/kernels/evil"]) {
    await mkdir(join(project, k), { recursive: true });
    await writeFile(
      join(project, k, "kernel.json"),
      JSON.stringify({ argv: [join(project, "bin", "python.exe"), "-f", "{connection_file}"], display_name: "Evil", language: "python" }),
    );
  }
  await mkdir(join(project, ".exposition"), { recursive: true });
  await writeFile(
    join(project, ".exposition", "project.json"),
    JSON.stringify({
      version: 1,
      entry: "main.tex",
      targets: [{ name: "pdf", format: "latex", engine: "pdfLatex", entry: "main.tex" }],
      defaultTarget: "pdf",
      toolPaths: { pdflatex: join(project, "bin", "pdflatex.exe"), python: join(project, "bin", "python.exe") },
      interpreter: join(project, "bin", "python.exe"),
    }),
  );
  await writeFile(join(project, "main.typ"), "= Hostile\nHello #read(\"main.tex\")\n");
  await writeFile(join(project, "notes.md"), "# Notes\n");
  // Host-side influence: a PYTHONPATH dir whose sitecustomize writes a marker.
  await writeFile(join(evil, "sitecustomize.py"), `open(r"${join(markers, "HOST-PYTHONPATH-RAN.txt")}", "w").write("x")\n`);

  const markerList = () => readdirSync(markers);
  const projectRan = () => walk(project).filter((f) => /[\\/]RAN-[^\\/]+\.txt$/.test(f)).map((f) => relative(project, f));

  const hostile = {
    EXPOSITION_CREDENTIAL_NAMESPACE: `review-sec-exec-${Date.now()}`,
    ANTHROPIC_API_KEY: "sk-ant-api03-REVIEWSECFAKEKEY-not-real-0000000000",
    anthropic_auth_token: "review-sec-lowercase-token-value",
    AWS_SECRET_ACCESS_KEY: "review-sec-aws-secret-value-1234",
    GITHUB_TOKEN: "ghp_reviewsecfakefakefakefakefake000000",
    MY_PASSWORD: "review-sec-password-value",
    PYTHONPATH: evil,
    PYTHONSTARTUP: join(evil, "sitecustomize.py"),
    PERL5OPT: "-Mreviewsecevil",
    TEXMFHOME: evil,
    NODE_OPTIONS: "--max-old-space-size=17",
    R_PROFILE_USER: join(evil, "Rprofile"),
    JUPYTER_PATH: join(project, "share", "jupyter"),
    REVIEW_SEC_HOST_ONLY: "review-sec-host-only-value",
  };
  const app = await launchApp({ env: hostile });
  const evidence = { project, steps: [] };
  try {
    const { page } = app;
    const opened = await invoke(page, "workspace_open", { path: project });
    const root = opened.root;
    for (const p of ["main.tex", "Hostile.lean", "main.typ", "notes.md", ".exposition/project.json"]) {
      await invoke(page, "document_open", { path: p }).catch((e) => info(`document_open ${p}: ${e.message}`));
    }
    await invoke(page, "workspace_list_dir", { path: null });
    await invoke(page, "toolchains_refresh");
    const envs = await invoke(page, "research_environments", { refresh: true });
    await invoke(page, "lean_projects");
    await invoke(page, "lean_documents");
    await invoke(page, "tex_build", { target: null, mode: "draft" }).catch((e) => info(`tex_build: ${e.message}`));
    await invoke(page, "project_settings_get").catch(() => {});
    await invokeError(page, "research_session_start", { request: { environmentId: "python:any" } });
    await page.waitForTimeout(8000);

    const pre = { ran: projectRan(), markers: markerList() };
    evidence.steps.push({ step: "untrusted", ...pre });
    if (pre.ran.length || pre.markers.length) {
      defect(`untrusted open executed project content: ${JSON.stringify(pre)}`);
    } else ok("untrusted: no project stub, lakefile, Lean, TeX, or Python import executed on open/discovery/refresh/build request");
    const jobs = await invoke(page, "job_list");
    const lower = root.toLowerCase();
    const inProject = jobs.filter((j) => j.info.program.toLowerCase().startsWith(lower));
    if (inProject.length) defect(`untrusted: jobs launched project programs: ${inProject.map((j) => j.info.program).join(", ")}`);
    const pyEnvs = envs.runtimes.filter((r) => r.executable && r.executable.toLowerCase().startsWith(lower));
    if (pyEnvs.length) defect(`untrusted: research_environments lists project interpreters: ${pyEnvs.map((r) => r.executable)}`);
    else ok("untrusted: no project interpreter listed");
    const tools = await invoke(page, "toolchains_list");
    const projTools = tools.filter((t) => t.path && t.path.toLowerCase().startsWith(lower));
    if (projTools.length) defect(`toolchain discovery chose project files: ${projTools.map((t) => `${t.id}=${t.path}`).join(", ")}`);
    else ok("toolchain discovery: no tool located inside the project");

    // --- trust, then exercise every launch path
    assert(await invoke(page, "trust_grant", { root }), "trust_grant");
    await invoke(page, "research_environments", { refresh: true });
    await invoke(page, "tex_build", { target: null, mode: "full" });
    const texDone = await waitForAsync(
      page,
      async () => {
        const s = await window.__TAURI_INTERNALS__.invoke("tex_status", { target: null });
        return s?.last && !s.running && !s.pending && !s.last.refusal ? s.last : null;
      },
      null,
      { timeout: 120_000, message: "tex build after trust" },
    ).catch((e) => (info(`tex build wait: ${e.message}`), null));
    info(`tex after trust: ${JSON.stringify(texDone?.result?.status ?? null).slice(0, 200)}`);
    // Lean: the server for the Lake project (runs the lakefile: trusted project code, expected).
    const leanDocs = await invoke(page, "lean_documents");
    info(`lean documents: ${JSON.stringify(leanDocs.map((d) => d.status?.kind ?? d.status)).slice(0, 200)}`);
    await page.waitForTimeout(15000);

    // Research: a system Python session; inspect the kernel's environment.
    const report = await invoke(page, "research_environments", { refresh: false });
    const sysPy = report.runtimes.find(
      (r) => r.language === "python" && r.availability.kind === "available" && !r.executable?.toLowerCase().startsWith(lower),
    );
    const projPy = report.runtimes.filter((r) => r.executable?.toLowerCase().startsWith(lower));
    info(`trusted: project interpreters listed: ${projPy.map((r) => `${r.executable}:${r.availability.kind}`).join(", ") || "none"}`);
    if (sysPy) {
      const session = await invoke(page, "research_session_start", { request: { environmentId: sysPy.id } });
      await waitForAsync(
        page,
        async (id) => (await window.__TAURI_INTERNALS__.invoke("research_sessions")).find((s) => s.id === id && s.state?.kind === "idle"),
        session.id,
        { timeout: 120_000, message: "kernel idle" },
      ).catch((e) => info(`kernel idle wait: ${e.message}`));
      const code = "import os, sys, json\nprint('ENV=' + json.dumps(sorted(os.environ)))\nprint('PATHVAR=' + os.environ.get('PATH',''))\nprint('SYSPATH=' + json.dumps(sys.path))";
      const run = await invoke(page, "research_execute", { request: { sessionId: session.id, source: { kind: "input", code } } });
      const done = await waitForAsync(
        page,
        async (id) => {
          const mf = await window.__TAURI_INTERNALS__.invoke("research_run", { runId: id });
          return mf.record.status.kind !== "running" ? mf : null;
        },
        run.id,
        { timeout: 120_000, message: "env run" },
      );
      const out = await invoke(page, "research_run_output", { runId: run.id, after: null, limit: 100 });
      const text = out.entries.filter((e) => e.item.kind === "stream").map((e) => e.item.text).join("");
      evidence.kernel = { status: done.record.status, text };
      const envNames = JSON.parse((text.match(/ENV=(.*)/) ?? [, "[]"])[1]).map((n) => n.toUpperCase());
      const leaked = Object.keys(hostile).filter((k) => envNames.includes(k.toUpperCase()));
      if (leaked.length) defect(`kernel environment inherited host variables: ${leaked.join(", ")}`);
      else ok(`kernel environment: none of ${Object.keys(hostile).length} hostile host variables present (${envNames.length} vars)`);
      for (const value of Object.values(hostile)) if (text.includes(value) && value.length > 12) defect(`kernel output contains host value ${value.slice(0, 12)}…`);
      const pathVar = (text.match(/PATHVAR=(.*)/) ?? [, ""])[1];
      if (pathVar.toLowerCase().includes(lower)) info(`kernel PATH contains the project: ${pathVar}`);
      else ok("kernel PATH has no project directory");
      // Kernel transport: Python kernels are documented as CurveZMQ-encrypted;
      // another local process without the keys must not get code executed.
      const live = (await invoke(page, "research_sessions")).find((s) => s.id === session.id);
      evidence.transport = { encryption: live?.transportEncryption, kernelPid: live?.kernelPid, helperPid: live?.helperPid };
      if (live?.transportEncryption !== "curve") defect(`Python kernel transport is ${live?.transportEncryption}, docs claim CurveZMQ`);
      else ok("Python kernel session reports CurveZMQ transport");
      if (live?.kernelPid) {
        const netstat = execFileSync("netstat", ["-ano", "-p", "TCP"], { encoding: "utf8" });
        const listening = netstat
          .split(/\r?\n/)
          .map((l) => l.trim().split(/\s+/))
          .filter((c) => c[0] === "TCP" && c[3] === "LISTENING" && c[4] === String(live.kernelPid));
        const addrs = listening.map((c) => c[1]);
        evidence.transport.listening = addrs;
        const exposed = addrs.filter((a) => !a.startsWith("127.0.0.1:"));
        if (exposed.length) defect(`kernel listens beyond loopback: ${exposed.join(", ")}`);
        else ok(`kernel ports bound to loopback only (${addrs.length})`);
        const ports = addrs.map((a) => a.split(":").pop());
        const probe = join(base, "zmq_probe.py");
        const forged = join(markers, "FORGED-ZMQ-EXEC.txt").replace(/\\/g, "/");
        await writeFile(
          probe,
          [
            "import json, sys, time, uuid, zmq",
            "ctx = zmq.Context()",
            `code = "open('${forged}', 'w').write('x')"`,
            "socks = []",
            "for port in sys.argv[1:]:",
            "    for kind in (zmq.DEALER, zmq.PUB, zmq.REQ):",
            "        s = ctx.socket(kind); s.setsockopt(zmq.LINGER, 0)",
            "        s.connect('tcp://127.0.0.1:%s' % port); socks.append((kind, s))",
            "time.sleep(1.0)",
            "for kind, s in socks:",
            "    h = {'msg_id': uuid.uuid4().hex, 'username': 'x', 'session': uuid.uuid4().hex, 'msg_type': 'execute_request', 'version': '5.3', 'date': ''}",
            "    parts = [b'<IDS|MSG>', b'', json.dumps(h).encode(), b'{}', b'{}', json.dumps({'code': code, 'silent': False}).encode()]",
            "    try:",
            "        s.send_multipart(parts, flags=zmq.NOBLOCK)",
            "    except Exception as e:",
            "        pass",
            "time.sleep(3.0)",
            "print('sent to', len(socks), 'sockets')",
          ].join("\n"),
        );
        try {
          const outp = execFileSync("python", ["-I", probe, ...ports], { encoding: "utf8", timeout: 30_000 });
          info(`zmq probe: ${outp.trim()}`);
          await page.waitForTimeout(2000);
          if (existsSync(forged)) defect("a local process without the kernel key executed code in the kernel");
          else ok("unauthenticated local ZMQ messages to every kernel port executed nothing");
        } catch (e) {
          info(`zmq probe could not run: ${e.message}`);
        }
      }
      await invoke(page, "research_session_shutdown", { sessionId: session.id }).catch(() => {});
    } else info("no system Python environment available: kernel env check skipped");

    await page.waitForTimeout(2000);
    const post = { ran: projectRan(), markers: markerList() };
    evidence.steps.push({ step: "trusted", ...post });
    const expectedAfterTrust = (f) =>
      /^(\.venv|venv)[\\/]Scripts[\\/]RAN-python-/.test(f) || false;
    const unexpected = post.ran.filter((f) => !expectedAfterTrust(f));
    if (unexpected.length) defect(`after trust, project shims ran in place of real tools: ${unexpected.join(", ")}`);
    else ok(`after trust: only project venv interpreters ran (${post.ran.filter(expectedAfterTrust).length} probe(s)); no root/bin/Scripts shim ran`);
    for (const mk of post.markers) {
      if (/^RAN-/.test(mk)) continue;
      if (/LAKEFILE-RAN|LEAN-ELAB-RAN/.test(mk)) info(`after trust: ${mk} (lakefile/Lean elaboration runs project code; trusted)`);
      else if (/HOST-PYTHONPATH|TEX-WRITE18|LATEXMKRC|RPROFILE|JULIA-STARTUP/.test(mk)) defect(`after trust: ${mk} was executed`);
      else if (/^PY-IMPORTED-/.test(mk)) defect(`after trust: a project module shadowed a library at startup (${mk})`);
      else info(`marker ${mk}`);
    }
    if (!post.markers.some((x) => /TEX-WRITE18/.test(x))) ok("TeX \\write18 did not run");
    if (!post.markers.some((x) => /HOST-PYTHONPATH/.test(x))) ok("host PYTHONPATH sitecustomize never ran in any Python child");
    const jobsAfter = await invoke(page, "job_list");
    evidence.jobs = jobsAfter.map((j) => ({ label: j.info.label, program: j.info.program, state: j.state }));
    const projPrograms = jobsAfter.filter((j) => j.info.program.toLowerCase().startsWith(lower));
    info(`after trust: project programs run: ${projPrograms.map((j) => j.info.program).join(", ") || "none"}`);
    await invoke(page, "trust_revoke", { root }).catch(() => {});

    // --- Julia: the helper builds `cd("<workspace root>")` as Julia source; a
    // folder name with `$( ... )` is Julia string interpolation.
    const jlRoot = join(base, "jl$(touch(join(['P','W','N'])))");
    await mkdir(jlRoot, { recursive: true });
    await writeFile(join(jlRoot, "notes.md"), "# Julia\n");
    const jlOpened = await invoke(page, "workspace_open", { path: jlRoot });
    await invoke(page, "trust_grant", { root: jlOpened.root });
    const jlReport = await invoke(page, "research_environments", { refresh: false });
    const julia = jlReport.runtimes.find((r) => r.language === "julia" && r.availability.kind === "available");
    if (!julia) {
      info("Julia/IJulia unavailable: folder-name interpolation check skipped");
    } else {
      const s = await invoke(page, "research_session_start", { request: { environmentId: julia.id } });
      await waitForAsync(
        page,
        async (id) => (await window.__TAURI_INTERNALS__.invoke("research_sessions")).find((x) => x.id === id && x.state?.kind !== "starting"),
        s.id,
        { timeout: 180_000, message: "julia session started" },
      ).catch((e) => info(`julia wait: ${e.message}`));
      await page.waitForTimeout(3000);
      const pwn = join(jlRoot, "PWN");
      evidence.julia = { root: jlOpened.root, pwn: existsSync(pwn) };
      if (existsSync(pwn)) defect(`starting a Julia session evaluated code from the folder name (${pwn} created by \`touch\` in the root name)`);
      else ok("Julia session start did not evaluate the folder name");
      await invoke(page, "research_session_shutdown", { sessionId: s.id }).catch(() => {});
    }
    await invoke(page, "trust_revoke", { root: jlOpened.root }).catch(() => {});
  } finally {
    await writeFile(join(artifactsDir, "review-sec-exec.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  if (defects.length) throw new Error(`${defects.length} DEFECT(s):\n${defects.join("\n")}`);
}

function assert(value, message) {
  if (!value) throw new Error(message);
}
