// R-sec (M6 security review): the IPC surface as a compromised or buggy
// webview script sees it (window.__TAURI_INTERNALS__.invoke), plus file
// integrity checks that need the real app.
//
// Runs only when named: `npm run e2e -- review-sec-ipc`. Prints `ok:` /
// `DEFECT:` / `info:` lines, writes findings.json, and fails if any DEFECT was
// reproduced. Debug build; isolated data dir and credential namespace.
//
// Sections:
//   A. workspace_open / trust_grant broad-root refusals (aliases: case,
//      verbatim, device, trailing dot, volume GUID, 8.3, junctions, loopback
//      administrative share UNC paths) and the app's own data directory.
//   B. execution_settings_update tool locations inside the project (aliases)
//      and a location that becomes project content after it was configured.
//   C. typesetting_export_pdf (Typst) destinations.
//   D. huge document payloads.
//   E. quick re-runs of earlier negatives (paths, recovery ids, lean ids).
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

import { waitForAsync } from "../wait.mjs";
import { makeProject } from "../workspace-helpers.mjs";

const NAME = "review-sec-ipc";

function recorder(artifactsDir) {
  const results = [];
  const log = (status, id, detail) => {
    results.push({ status, id, detail });
    const tag = status === "DEFECT" ? "DEFECT:" : status === "ok" ? "ok:" : "info:";
    console.log(`${tag} ${id}${detail ? ` — ${detail}` : ""}`);
  };
  return {
    ok: (id, detail = "") => log("ok", id, detail),
    defect: (id, detail = "") => log("DEFECT", id, detail),
    info: (id, detail = "") => log("info", id, detail),
    expect(id, ok, detail = "") {
      log(ok ? "ok" : "DEFECT", id, detail);
      return ok;
    },
    async finish() {
      await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(results, null, 2));
      const defects = results.filter((r) => r.status === "DEFECT");
      if (defects.length) throw new Error(`${defects.length} defect(s) reproduced: ${defects.map((d) => d.id).join("; ")}`);
    },
  };
}

function call(page, command, args = {}) {
  return page.evaluate(
    ([c, a]) =>
      window.__TAURI_INTERNALS__.invoke(c, a).then(
        (ok) => ({ ok }),
        (error) => ({ error: error ?? "rejected" }),
      ),
    [command, args],
  );
}

const short = (s, n = 160) => (typeof s === "string" ? s : JSON.stringify(s) ?? "undefined").slice(0, n);
const junction = (link, target) => execFileSync("cmd", ["/c", "mklink", "/J", link, target], { stdio: "ignore" });
/** 8.3 short form of an existing path, or null when the volume has none. */
function shortName(path) {
  try {
    const out = execFileSync("cmd", ["/d", "/s", "/c", `"for %I in ("${path}") do @echo %~sI"`], {
      encoding: "utf8",
      windowsVerbatimArguments: true,
    }).trim();
    return out && out.toLowerCase() !== path.toLowerCase() ? out : null;
  } catch {
    return null;
  }
}
function volumeGuid(drive) {
  try {
    const out = execFileSync("mountvol", [`${drive}\\`, "/L"], { encoding: "utf8" }).trim();
    return out.startsWith("\\\\?\\Volume{") ? out : null;
  } catch {
    return null;
  }
}
/** `C:\x\y` → `\\localhost\C$\x\y` (the administrative share of the same folder). */
const adminShare = (path, host = "localhost") => `\\\\${host}\\${path[0]}$${path.slice(2)}`;

function txn(id, document, baseRevision, patches) {
  return {
    request: {
      id,
      origin: { kind: "source" },
      edits: [{ document, baseRevision, patches }],
      undo: { kind: "newGroup" },
      conflict: "reject",
      label: null,
      selection: null,
    },
  };
}

export default async function reviewSecIpc({ launchApp, artifactsDir, fixture }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }
  const r = recorder(artifactsDir);
  const project = await makeProject(fixture);
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `review-sec-ipc-${Date.now()}` };
  const app = await launchApp({ dataDir: project.dataDir, env });
  const { page } = app;
  const alive = async (what) => {
    const t0 = Date.now();
    const res = await Promise.race([
      call(page, "app_info"),
      new Promise((resolve) => setTimeout(() => resolve({ error: "timeout" }), 30_000)),
    ]);
    r.expect(`app still answers after ${what}`, !!res.ok, `${Date.now() - t0} ms`);
  };
  const reopenProject = async () => {
    const res = await call(page, "workspace_open", { path: project.root });
    if (!res.ok) throw new Error(`could not reopen the project: ${short(res.error)}`);
  };
  try {
    await reopenProject();
    const profile = process.env.USERPROFILE;
    const user = process.env.USERNAME;
    const systemRoot = process.env.SystemRoot ?? "C:\\Windows";
    const programFiles = process.env.ProgramFiles ?? "C:\\Program Files";
    const usersDir = join(profile, "..");

    // ------------------------------------------------------------------ A
    const links = join(project.base, "links");
    await mkdir(links, { recursive: true });
    junction(join(links, "to-profile"), profile);
    junction(join(links, "to-users"), usersDir);
    junction(join(links, "to-windows"), systemRoot);
    const guid = volumeGuid(profile.slice(0, 2));
    const broad = [
      ["users", usersDir],
      ["users (case, trailing slash)", `${usersDir.toLowerCase()}\\`],
      ["users (verbatim)", `\\\\?\\${usersDir}`],
      ["users (device namespace)", `\\\\.\\${usersDir}`],
      ["users (trailing dot)", `${usersDir}.`],
      ["profile", profile],
      ["profile (8.3)", shortName(profile)],
      ["Documents", join(profile, "Documents")],
      ["Program Files (8.3)", shortName(programFiles)],
      ["inside Program Files (8.3)", shortName(programFiles) && `${shortName(programFiles)}\\Common Files`],
      ["users (volume GUID)", guid && `${guid}${usersDir.slice(3)}`],
      ["junction → profile", join(links, "to-profile")],
      ["junction → users", join(links, "to-users")],
      ["junction → Windows", join(links, "to-windows")],
      ["drive via admin share", `\\\\localhost\\${profile[0]}$`],
      ["profile via admin share (localhost)", adminShare(profile)],
      ["profile via admin share (127.0.0.1)", adminShare(profile, "127.0.0.1")],
      ["profile via verbatim UNC", `\\\\?\\UNC\\localhost\\${profile[0]}$${profile.slice(2)}`],
      ["users via admin share", adminShare(usersDir)],
      ["Windows via admin share", adminShare(systemRoot)],
      ["Windows via ADMIN$", "\\\\localhost\\ADMIN$"],
      ["the app's data directory", project.dataDir],
      ["the app's execution data (trust.json)", join(project.dataDir, "execution")],
    ];
    for (const [label, path] of broad) {
      if (!path) {
        r.info(`A ${label}`, "not applicable on this machine");
        continue;
      }
      const reachable = existsSync(path);
      const trust = await call(page, "trust_grant", { root: path });
      let consequence = "";
      if (trust.ok?.trusted && path.startsWith("\\\\") && path.toLowerCase().includes(user.toLowerCase())) {
        // Consequence: a fresh folder the user never trusted, reached through
        // the same alias, is now trusted (its native jobs run without a dialog).
        const fresh = join(project.base, `never-trusted-${Date.now()}`);
        await mkdir(fresh, { recursive: true });
        const viaAlias = await call(page, "trust_status", { root: adminShare(fresh) });
        consequence = `; trust_status(${adminShare(fresh)}) → trusted=${viaAlias.ok?.trusted} via ${viaAlias.ok?.trustedVia}`;
      }
      if (trust.ok?.trusted) await call(page, "trust_revoke", { root: path });
      const open = await call(page, "workspace_open", { path });
      if (open.ok) await reopenProject();
      const accepted = [trust.ok?.trusted && "trust_grant", open.ok && "workspace_open"].filter(Boolean);
      const detail = `path ${path}${reachable ? "" : " (not reachable)"}; trust_grant → ${short(trust.ok ?? trust.error, 120)}; workspace_open → ${short(open.ok?.root ?? open.error, 120)}${consequence}`;
      r.expect(`A refuses ${label}`, accepted.length === 0, accepted.length ? `${accepted.join(" + ")} accepted it; ${detail}` : detail);
    }
    for (const [label, path] of [
      ["another user's profile (Default)", join(usersDir, "Default")],
      ["Public", join(usersDir, "Public")],
    ]) {
      if (!existsSync(path)) continue;
      const open = await call(page, "workspace_open", { path });
      if (open.ok) await reopenProject();
      r.info(`A ${label}`, `workspace_open → ${short(open.ok?.root ?? open.error, 120)} (strict descendant of the users folder; allowed by the rule as written)`);
    }

    // ------------------------------------------------------------------ B
    // A copied system executable stands in for a binary shipped by a project.
    const tools = join(project.root, "tools");
    await mkdir(tools, { recursive: true });
    const projectExe = join(tools, "git.exe");
    await copyFile(join(systemRoot, "System32", "whoami.exe"), projectExe);
    junction(join(links, "to-tools"), tools);
    const rootShort = shortName(project.root);
    const aliases = [
      ["direct", projectExe],
      ["upper case", projectExe.toUpperCase()],
      ["8.3 root", rootShort && join(rootShort, "tools", "git.exe")],
      ["junction outside the project", join(links, "to-tools", "git.exe")],
      ["verbatim", `\\\\?\\${projectExe}`],
      ["admin share UNC", adminShare(projectExe)],
    ];
    const toolChange = (path) => ({ change: { kind: "toolPath", tool: "git", path } });
    const probesOf = async () =>
      ((await call(page, "job_list")).ok ?? []).filter(
        (j) => j.info.program.toLowerCase().replace(/^\\\\\?\\/, "").includes("\\tools\\git.exe") || j.info.program.toLowerCase().includes("$\\"),
      );
    for (const [label, path] of aliases) {
      if (!path) {
        r.info(`B tool location (${label})`, "not applicable on this machine");
        continue;
      }
      const res = await call(page, "execution_settings_update", toolChange(path));
      const accepted = !!res.ok;
      let ran = "";
      if (accepted) {
        await call(page, "toolchains_refresh");
        const probes = await probesOf();
        ran = probes.length ? `; toolchains_refresh then ran ${probes.map((p) => p.info.program).join(", ")} without trust` : "";
        await call(page, "execution_settings_update", toolChange(null));
      }
      r.expect(`B tool location inside the open project refused (${label})`, !accepted, `${path} → ${short(res.ok ? "accepted" : res.error, 140)}${ran}`);
    }
    // Configured while the folder is not open, then the folder is opened (untrusted).
    await call(page, "workspace_close");
    const seen = new Set((await probesOf()).map((j) => j.info.id));
    const later = await call(page, "execution_settings_update", toolChange(projectExe));
    if (later.ok) {
      await reopenProject();
      const trust = await call(page, "trust_status", { root: project.root });
      await call(page, "toolchains_refresh");
      const probes = await waitForAsync(
        page,
        async ([seenIds]) => {
          const jobs = await window.__TAURI_INTERNALS__.invoke("job_list");
          const fresh = jobs.filter(
            (j) => !seenIds.includes(j.info.id) && /\\tools\\git\.exe$/i.test(j.info.program) && j.info.result,
          );
          return fresh.length ? fresh : null;
        },
        [[...seen]],
        { timeout: 10_000, message: "probe of the project executable" },
      ).catch(() => []);
      r.expect(
        "B a tool location that is now inside the open untrusted project is not run",
        probes.length === 0,
        `override set while closed → accepted; after opening the untrusted project (trusted=${trust.ok?.trusted}) toolchains_refresh ran: ${probes.map((p) => `${p.info.label} ${p.info.program} → exit ${p.info.result?.outcome?.code ?? short(p.info.result)}`).join("; ") || "nothing"}`,
      );
      await call(page, "execution_settings_update", toolChange(null));
    } else {
      r.info("B tool location set while no folder is open", short(later.error));
      await reopenProject();
    }

    // ------------------------------------------------------------------ C
    const main = await call(page, "document_open", { path: "main.typ" });
    const build = await call(page, "typesetting_build", { entry: "main.typ" });
    const buildId = build.ok?.result?.id;
    if (!buildId || build.ok.result.status === "failed") {
      r.info("C Typst export", `no build to export: ${short(build.error ?? build.ok?.result?.status)}`);
    } else {
      const exportTo = (destination) => call(page, "typesetting_export_pdf", { build: buildId, destination });
      // C1 a non-PDF file named *.pdf outside the project.
      const notes = join(project.outside, "notes.pdf");
      await writeFile(notes, "my notes, not a PDF");
      const c1 = await exportTo(notes);
      const c1After = await readFile(notes, "utf8").catch(() => "");
      r.expect("C export never replaces a file that is not a PDF", c1After === "my notes, not a PDF", `→ ${short(c1.ok ?? c1.error)}; file now starts ${JSON.stringify(c1After.slice(0, 8))}`);
      // C2 an open text document that happens to be named *.pdf.
      await writeFile(join(project.root, "readme.pdf"), "plain text kept as readme.pdf\n");
      const opened = await call(page, "document_open", { path: "readme.pdf" });
      if (opened.ok) {
        const c2 = await exportTo(join(project.root, "readme.pdf"));
        const after = await readFile(join(project.root, "readme.pdf"), "utf8");
        r.expect("C export never replaces an open document", after === "plain text kept as readme.pdf\n", `→ ${short(c2.ok ?? c2.error)}`);
        await call(page, "document_close", { document: opened.ok.state.info.id });
      } else {
        r.info("C open readme.pdf", short(opened.error));
      }
      // C3 alternate data stream of a project source.
      const mainPath = join(project.root, "main.typ");
      const mainBefore = await readFile(mainPath);
      const c3 = await exportTo(`${mainPath}:x.pdf`);
      const stream = await readFile(`${mainPath}:x.pdf`).catch(() => null);
      r.expect(
        "C export into an alternate data stream of a source is refused",
        !stream && (await readFile(mainPath)).equals(mainBefore),
        `→ ${short(c3.ok ?? c3.error)}; stream ${stream ? `written (${stream.length} bytes)` : "absent"}`,
      );
      // C4 a folder named *.pdf, an over-long path, a device path.
      await mkdir(join(project.outside, "folder.pdf"), { recursive: true });
      const c4 = await exportTo(join(project.outside, "folder.pdf"));
      r.expect("C export over a folder refused", !!c4.error, short(c4.error ?? c4.ok));
      const c5 = await exportTo(`${project.outside}\\${"a".repeat(40_000)}.pdf`);
      r.expect("C over-long destination refused", !!c5.error, short(c5.error ?? c5.ok, 100));
      const c6 = await exportTo("\\\\.\\NUL");
      r.expect("C device destination refused", !!c6.error, short(c6.error ?? c6.ok));
      const c7 = await exportTo(join(project.outside, "nul.pdf"));
      r.info("C destination nul.pdf", `${short(c7.ok?.path ?? c7.error)}; file exists afterwards: ${existsSync(join(project.outside, "nul.pdf"))}`);
      await alive("export probes");
    }
    if (main.ok) await call(page, "document_close", { document: main.ok.state.info.id });

    // ------------------------------------------------------------------ D
    const notesDoc = await call(page, "document_open", { path: "notes.md" });
    if (notesDoc.ok) {
      const info = notesDoc.ok.state.info;
      const timing = await page.evaluate(async ([id, rev]) => {
        const text = "z".repeat(50 * 1024 * 1024);
        const t0 = performance.now();
        let outcome;
        try {
          await window.__TAURI_INTERNALS__.invoke("document_apply", {
            request: {
              id: "huge",
              origin: { kind: "source" },
              edits: [{ document: id, baseRevision: rev, patches: [{ range: { start: 0, end: 0 }, text }] }],
              undo: { kind: "newGroup" },
              conflict: "reject",
              label: null,
              selection: null,
            },
          });
          outcome = "accepted";
        } catch (e) {
          outcome = `refused: ${JSON.stringify(e).slice(0, 120)}`;
        }
        const applyMs = performance.now() - t0;
        const t1 = performance.now();
        await window.__TAURI_INTERNALS__.invoke("document_list");
        return { outcome, applyMs, listMs: performance.now() - t1 };
      }, [info.id, info.revision]);
      r.info("D 50 MB document_apply", `${timing.outcome} in ${timing.applyMs.toFixed(0)} ms; next document_list ${timing.listMs.toFixed(0)} ms`);
      const save = await call(page, "document_save", { document: info.id, overwrite: false });
      r.expect("D a 50 MB buffer is not saved over the original", !!save.error && (await readFile(join(project.root, "notes.md"), "utf8")).length < 1_000_000, short(save.error ?? save.ok));
      await new Promise((resolve) => setTimeout(resolve, 3000));
      await alive("a 50 MB edit");
      await call(page, "document_undo", { document: info.id, origin: { kind: "source" } });
      await call(page, "document_close", { document: info.id });
    }

    // ------------------------------------------------------------------ E
    await writeFile(join(project.outside, "secret.txt"), "TOP-SECRET-R-SEC\n");
    junction(join(project.root, "escapejunction"), project.outside);
    const escShort = shortName(join(project.root, "escapejunction"));
    const hostile = [
      "../outside/secret.txt",
      join(project.outside, "secret.txt"),
      `\\\\?\\${join(project.outside, "secret.txt")}`,
      "escapejunction/secret.txt",
      "ESCAPEJUNCTION/SECRET.TXT",
      escShort && `${escShort.split("\\").pop()}/secret.txt`,
      "escape/secret.txt",
      "main.typ:hidden",
      "main.typ::$DATA",
      "CON",
      "nul.txt",
    ].filter(Boolean);
    for (const path of hostile) {
      const res = await call(page, "document_open", { path });
      const leaked = JSON.stringify(res).includes("TOP-SECRET");
      r.expect(`E document_open refuses ${JSON.stringify(path).slice(0, 60)}`, !!res.error && !leaked, short(res.error ?? res.ok?.state?.text, 120));
    }
    for (const id of ["../../settings", "rec-../../x", "rec-0123456789abcdef-1/../../x", "C:\\x"]) {
      const a = await call(page, "recovery_restore", { id });
      const b = await call(page, "recovery_discard", { id });
      r.expect(`E recovery id ${JSON.stringify(id)} refused`, !!a.error && !!b.error, `${short(a.error, 60)} / ${short(b.error, 60)}`);
    }
    const typ = await call(page, "document_open", { path: "main.typ" });
    if (typ.ok) {
      const lean = await call(page, "lean_install_toolchain", { document: typ.ok.state.info.id });
      r.expect("E lean_install_toolchain refuses a non-Lean document", !!lean.error, short(lean.error ?? lean.ok));
      await call(page, "document_close", { document: typ.ok.state.info.id });
    }
    for (const doc of ["doc-999999", "../x", "x".repeat(200)]) {
      const lean = await call(page, "lean_restart_file", { document: doc });
      r.expect(`E lean_restart_file refuses ${short(doc, 20)}`, !!lean.error, short(lean.error ?? lean.ok, 100));
    }
    // ------------------------------------------------------------------ F
    // Build ids of a previous workspace (TeX builds are retained in the TeX
    // service): after switching folders they must not serve pages or export
    // into the newly opened folder.
    const other = join(project.base, "other-paper");
    await mkdir(other, { recursive: true });
    await writeFile(
      join(other, "main.tex"),
      "\\documentclass{article}\n\\begin{document}\nOTHER PROJECT CONFIDENTIAL DRAFT\n\\end{document}\n",
    );
    const opened = await call(page, "workspace_open", { path: other });
    const granted = await call(page, "trust_grant", { root: other });
    const texBuild = opened.ok && granted.ok?.trusted ? await call(page, "typesetting_build", { entry: "main.tex" }) : null;
    const texId = texBuild?.ok?.result?.id;
    if (!texId || texBuild.ok.result.status === "failed") {
      r.info("F TeX build in another folder", `not available: ${short(texBuild?.error ?? texBuild?.ok?.result?.status ?? opened.error ?? granted.error)}`);
    } else {
      await reopenProject();
      // A PDF already in the project (a previous export of its own).
      const typ = await call(page, "typesetting_build", { entry: "main.typ" });
      const own = join(project.root, "main.pdf");
      if (typ.ok?.result?.id) await call(page, "typesetting_export_pdf", { build: typ.ok.result.id, destination: own });
      const ownBefore = existsSync(own) ? await readFile(own) : null;
      const img = await call(page, "tex_page_image", { build: texId, page: 0, scale: 1 });
      r.expect(
        "F pages of a build from the previously open folder are not served",
        !!img.error,
        img.error ? short(img.error) : "tex_page_image returned the other folder's page",
      );
      const exp = await call(page, "typesetting_export_pdf", { build: texId, destination: null });
      const ownAfter = existsSync(own) ? await readFile(own) : null;
      const replaced = ownBefore && ownAfter && !ownAfter.equals(ownBefore);
      r.expect(
        "F a build from the previously open folder is not exported into the open folder",
        !exp.ok,
        `typesetting_export_pdf(${texId}, default) → ${short(exp.ok?.path ?? exp.error)}${replaced ? "; the project's own main.pdf was replaced by the other folder's PDF" : ""}`,
      );
      await call(page, "trust_revoke", { root: other });
    }
    await reopenProject();

    await alive("all probes");
  } finally {
    await r.finish().finally(() => app.close());
  }
}
