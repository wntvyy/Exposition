// Lean feature registration: the infoview (auxiliary panel), the status bar
// pill for the active Lean file, commands, and Monaco Lean support.
import { Sigma } from "lucide-react";
import { useEffect } from "react";

import { useActiveEditor, useEditors } from "../../app/editors";
import { layout } from "../../app/layout";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { commands, errorMessage } from "../../lib/ipc";
import { StatusPill } from "../../ui/components";
import { publishProblems } from "../../app/problems";
import { goToDefinition, installLeanMonaco, revealInEditor } from "./leanMonaco";
import { LEAN_VIEW_ID, LeanPanel, installToolchain, trustAndRestart } from "./LeanPanel";
import { statusView } from "./status";
import { leanStateForPath, startLeanTracking, useLean } from "./store";

installLeanMonaco();

// Publish Lean diagnostics to the shared Problems view.
let publishedDocs: unknown = null;
useLean.subscribe((state) => {
  if (state.docs === publishedDocs) return;
  publishedDocs = state.docs;
  publishProblems({
    id: "lean",
    label: "Lean",
    diagnostics: Object.values(state.docs).flatMap((doc) => doc.diagnostics),
    reveal: (diagnostic) => {
      if (diagnostic.location) void revealInEditor(diagnostic.location.path, { range: diagnostic.location.range });
    },
  });
});

function LeanStatusItem() {
  useEffect(() => startLeanTracking(), []);
  const active = useActiveEditor();
  const path = active?.resource.path;
  const state = useLean((s) => (path?.endsWith(".lean") ? Object.values(s.docs).find((d) => d.path === path) : undefined));
  if (!state) return null;
  const view = statusView(state.status);
  return (
    <span data-testid="lean-status-item" data-status={state.status.kind} title={`Lean: ${state.toolchain?.name ?? "no toolchain"}`}>
      <StatusPill size="sm" status={view.status} label={`Lean: ${view.label}`} detail={view.detail} />
    </span>
  );
}

registry.registerView({
  id: LEAN_VIEW_ID,
  location: "auxiliary",
  title: "Lean",
  icon: Sigma,
  order: 20,
  render: () => <LeanPanel />,
});

registry.registerStatusItem({ id: "lean.status", alignment: "left", order: 40, render: () => <LeanStatusItem /> });

/** The Lean state of the active editor, if it is a Lean file. */
function activeState() {
  const s = useEditors.getState();
  const group = s.groups[s.activeGroup];
  const path = group.editors.find((e) => e.resource.key === group.activeKey)?.resource.path;
  return leanStateForPath(path);
}

const isLeanActive = () => Boolean(activeState());

async function run(action: (document: string) => Promise<void>, failure: string) {
  const state = activeState();
  if (!state) return;
  try {
    await action(state.document);
  } catch (e) {
    notify({ tone: "danger", title: failure, body: errorMessage(e) });
  }
}

registry.registerCommand({
  id: "lean.showInfoview",
  title: "Show Lean Infoview",
  category: "Lean",
  run: () => layout.showView("auxiliary", LEAN_VIEW_ID),
});

registry.registerCommand({
  id: "lean.restartServer",
  title: "Restart Lean Server",
  category: "Lean",
  enabled: isLeanActive,
  run: () => run((d) => commands.leanRestart(d), "Could not restart the Lean server"),
});

registry.registerCommand({
  id: "lean.restartFile",
  title: "Restart File (Rebuild Imports)",
  category: "Lean",
  enabled: isLeanActive,
  run: () => run((d) => commands.leanRestartFile(d), "Could not restart the file"),
});

registry.registerCommand({
  id: "lean.goToDefinition",
  title: "Go to Definition",
  category: "Lean",
  enabled: isLeanActive,
  run: () => goToDefinition(),
});

registry.registerCommand({
  id: "lean.trustWorkspace",
  title: "Trust Workspace to Check Lean",
  category: "Lean",
  enabled: () => activeState()?.status.kind === "untrusted",
  run: async () => {
    const state = activeState();
    if (state) await trustAndRestart(state);
  },
});

registry.registerCommand({
  id: "lean.installToolchain",
  title: "Install Project Toolchain",
  category: "Lean",
  enabled: () => {
    const status = activeState()?.status;
    return status?.kind === "toolchainMissing" && status.install !== null;
  },
  run: async () => {
    const state = activeState();
    if (state) await installToolchain(state);
  },
});
