// R-sec (M6 security review): the command/permission surface.
//
// Static part (no app): every #[tauri::command] is registered, listed in
// build.rs, and granted exactly once by capabilities/default.json to the main
// window only; debug-only commands are compiled only under debug_assertions,
// registered only under it, absent from release COMMANDS and the static
// capability, and granted only by a debug-only runtime capability.
//
// Runtime part (debug app): Tauri core/plugin commands the workbench does not
// need are refused by the ACL (new windows/webviews, save/message dialogs,
// path resolution, opener/shell/fs plugins, ...), and an unknown command is
// refused rather than reaching a handler.
//
// Runs only when named: `npm run e2e -- review-sec-acl`.
import { readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

import { REPO_ROOT } from "../app.mjs";

const NAME = "review-sec-acl";

const kebab = (name) => name.replace(/_/g, "-");

export default async function reviewSecAcl({ launchApp, artifactsDir }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }
  const findings = [];
  const record = (verdict, id, detail = "") => {
    findings.push({ verdict, id, detail });
    console.log(`${verdict}: ${id}${detail ? ` — ${detail}` : ""}`);
  };
  const check = (id, condition, detail) => record(condition ? "ok" : "DEFECT", id, detail);

  // ------------------------------------------------------------ static
  const tauriDir = join(REPO_ROOT, "src-tauri");
  const buildRs = await readFile(join(tauriDir, "build.rs"), "utf8");
  const list = (name) => {
    const match = buildRs.match(new RegExp(`const ${name}: &\\[&str\\] = &\\[([\\s\\S]*?)\\];`));
    return [...(match?.[1] ?? "").matchAll(/"([a-z0-9_]+)"/g)].map((m) => m[1]);
  };
  const COMMANDS = list("COMMANDS");
  const DEV = list("DEV_COMMANDS");
  const capability = JSON.parse(await readFile(join(tauriDir, "capabilities", "default.json"), "utf8"));
  const granted = capability.permissions.filter((p) => typeof p === "string");

  const libRs = await readFile(join(tauriDir, "src", "lib.rs"), "utf8");
  const handler = libRs.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1] ?? "";
  const registered = [];
  let pendingCfg = false;
  for (const line of handler.split("\n").map((l) => l.trim())) {
    if (line === "#[cfg(debug_assertions)]") {
      pendingCfg = true;
      continue;
    }
    const m = line.match(/^[a-z_]+::([a-z0-9_]+),?$/);
    if (m) {
      registered.push({ name: m[1], debugOnly: pendingCfg });
      pendingCfg = false;
    }
  }

  // Every #[tauri::command] fn in src-tauri/src, with whether it is cfg-gated.
  const defined = [];
  for (const file of (await readdir(join(tauriDir, "src"))).filter((f) => f.endsWith(".rs"))) {
    const text = await readFile(join(tauriDir, "src", file), "utf8");
    const re = /((?:#\[[^\]]*\]\s*)+)pub (?:async )?fn ([a-z0-9_]+)/g;
    for (const m of text.matchAll(re)) {
      if (!m[1].includes("tauri::command")) continue;
      defined.push({ name: m[2], file, debugOnly: m[1].includes("cfg(debug_assertions)") });
    }
  }

  for (const cmd of defined) {
    const reg = registered.find((r) => r.name === cmd.name);
    if (cmd.debugOnly) {
      check(`dev-command-${cmd.name}-in-DEV_COMMANDS`, DEV.includes(cmd.name) && !COMMANDS.includes(cmd.name));
      check(`dev-command-${cmd.name}-registered-only-in-debug`, reg?.debugOnly === true);
      check(`dev-command-${cmd.name}-not-in-static-capability`, !granted.includes(`allow-${kebab(cmd.name)}`));
    } else {
      check(`command-${cmd.name}-listed-and-granted`, COMMANDS.includes(cmd.name) && granted.includes(`allow-${kebab(cmd.name)}`) && reg && !reg.debugOnly);
    }
  }
  const strayCommands = COMMANDS.filter((c) => !defined.some((d) => d.name === c));
  check("no-listed-command-without-handler", strayCommands.length === 0, strayCommands.join(", "));
  const strayGrants = granted.filter(
    (p) => p.startsWith("allow-") && !COMMANDS.some((c) => `allow-${kebab(c)}` === p),
  );
  check("no-grant-for-unlisted-command", strayGrants.length === 0, strayGrants.join(", "));
  check("capability-main-window-only", JSON.stringify(capability.windows) === '["main"]' && !capability.webviews && !capability.remote);
  const pluginGrants = granted.filter((p) => p.includes(":"));
  record("INFO", "core/plugin grants", pluginGrants.join(", "));

  // The debug-only runtime capabilities are created only under cfg(debug_assertions).
  for (const file of ["execution.rs", "agents.rs", "research.rs"]) {
    const text = await readFile(join(tauriDir, "src", file), "utf8");
    const gated = /#\[cfg\(debug_assertions\)\]\s*pub fn add_dev_capability/.test(text);
    check(`${file}-add_dev_capability-is-debug-only`, gated);
  }
  check(
    "lib.rs-add_dev_capability-calls-are-debug-only",
    [...libRs.matchAll(/add_dev_capability/g)].length === 3 &&
      [...libRs.matchAll(/#\[cfg\(debug_assertions\)\]\s*[a-z_]+::add_dev_capability/g)].length === 3 &&
      /#\[cfg\(debug_assertions\)\]\s*execution::add_dev_capability/.test(libRs) &&
      /#\[cfg\(debug_assertions\)\]\s*agents::add_dev_capability/.test(libRs) &&
      /#\[cfg\(debug_assertions\)\]\s*research::add_dev_capability/.test(libRs),
  );
  const stateRs = await readFile(join(tauriDir, "src", "state.rs"), "utf8");
  check(
    "credential-namespace-ignored-in-release",
    /#\[cfg\(not\(debug_assertions\)\)\]\s*\{\s*None\s*\}/.test(stateRs),
  );
  check("scripted-provider-off-in-release", /allow_scripted: cfg!\(debug_assertions\)/.test(stateRs));

  // ------------------------------------------------------------ runtime
  const app = await launchApp({ env: { EXPOSITION_CREDENTIAL_NAMESPACE: `review-sec-acl-${Date.now()}` } });
  try {
    const { page } = app;
    const attempt = (cmd, args = {}) =>
      page.evaluate(
        ([c, a]) =>
          window.__TAURI_INTERNALS__.invoke(c, a).then(
            (value) => ({ ok: true, value: JSON.stringify(value)?.slice(0, 200) }),
            (error) => ({ ok: false, error: String(typeof error === "string" ? error : JSON.stringify(error)).slice(0, 300) }),
          ),
        [cmd, args],
      );
    const mustRefuse = [
      ["plugin:webview|create_webview_window", { options: { label: "x", url: "https://example.invalid/" } }],
      ["plugin:window|create", { options: { label: "x", url: "https://example.invalid/" } }],
      ["plugin:webview|create_webview", { windowLabel: "main", options: { label: "y", url: "https://example.invalid/" } }],
      ["plugin:dialog|save", { options: {} }],
      ["plugin:dialog|message", { message: "x" }],
      ["plugin:dialog|ask", { message: "x" }],
      ["plugin:path|resolve_directory", { directory: 1 }],
      ["plugin:webview|set_webview_url", { label: "main", value: "https://example.invalid/" }],
      ["plugin:window|set_always_on_top", { label: "main", value: true }],
      ["plugin:app|set_app_theme", { theme: "dark" }],
      ["plugin:opener|open_url", { url: "https://example.invalid/" }],
      ["plugin:shell|open", { path: "https://example.invalid/" }],
      ["plugin:fs|read_text_file", { path: "C:\\Windows\\win.ini" }],
      ["plugin:http|fetch", {}],
      ["no_such_command", {}],
    ];
    for (const [cmd, args] of mustRefuse) {
      const r = await attempt(cmd, args);
      check(`refused ${cmd}`, !r.ok, r.ok ? `allowed: ${r.value}` : r.error.slice(0, 120));
    }
    // The page is still the app (no navigation happened).
    check("main-webview-still-app-origin", /tauri\.localhost/.test(page.url()), page.url());
  } finally {
    await app.close();
  }

  await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(findings, null, 2));
  const defects = findings.filter((f) => f.verdict === "DEFECT");
  if (defects.length) throw new Error(`${defects.length} DEFECT(s): ${defects.map((d) => d.id).join(", ")}`);
}
