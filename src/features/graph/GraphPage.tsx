// The Graph page: purposeful inspection of the project's typed model (a
// projection, not a whiteboard). Project and Mathematics subsections share the
// canonical entities; their saved views keep layout only. Opening the page is
// what starts graph reads; the rest of the app never needs it.
import { FolderOpen, Network, RotateCcw, Sigma } from "lucide-react";
import { useEffect } from "react";

import { registry } from "../../app/registry";
import type { RecoveryOption } from "../../bindings/RecoveryOption";
import { Banner, Button, confirm, EmptyState, ErrorState, LoadingState, SegmentedControl, UnavailableState } from "../../ui/components";
import { useWorkspace } from "../workspace/store";
import { errorText, graphApi } from "./api";
import { Mathematics } from "./Mathematics";
import { useGraphPage, type GraphSection } from "./navigation";
import { ProjectGraph } from "./ProjectGraph";
import { FreshnessPill } from "./status";
import { canRead, graph, useGraph } from "./store";
import { createViewStore } from "./viewStore";

export const projectViewStore = createViewStore("Project", { kind: "project" });
export const mathViewStore = createViewStore("Mathematics", { kind: "mathematics" });

function recoveryLabel(option: RecoveryOption): string {
  switch (option.kind) {
    case "retry":
      return "Try again";
    case "startNew":
      return "Start a new project graph";
    case "restoreBackup":
      return `Restore the backup from ${new Date(option.takenAtMs).toLocaleString()}`;
  }
}

async function recover(option: RecoveryOption) {
  if (option.kind === "startNew") {
    const choice = await confirm<"cancel" | "start">({
      title: "Start a new project graph?",
      message:
        "Curated relationships, collections, and saved views in the damaged graph will not be in the new one. The damaged graph file is kept aside, not deleted.",
      actions: [
        { id: "cancel", label: "Cancel", variant: "secondary" },
        { id: "start", label: "Start new graph", variant: "danger" },
      ],
      cancelId: "cancel",
      defaultId: "cancel",
      tone: "warning",
    });
    if (choice !== "start") return;
  }
  try {
    graph.setStatus(await graphApi().recover(option));
    await graph.resync();
  } catch (error) {
    await confirm({ title: "Recovery failed", message: errorText(error), actions: [{ id: "ok", label: "OK", variant: "primary" }], cancelId: "ok" });
  }
}

export function GraphPage() {
  const folder = useWorkspace((s) => s.info);
  const status = useGraph((s) => s.status);
  const statusError = useGraph((s) => s.statusError);
  const epoch = useGraph((s) => s.epoch);
  const section = useGraphPage((s) => s.section);
  const readable = canRead(status);

  useEffect(() => graph.start(), []);
  // (Re)load saved views whenever the project graph changes.
  const project = status?.project ?? null;
  useEffect(() => {
    if (!readable) return;
    void projectViewStore.load();
    void mathViewStore.load();
  }, [readable, project, epoch]);
  // Save pending layout changes when the page unmounts (app shutdown).
  useEffect(
    () => () => {
      void projectViewStore.flush();
      void mathViewStore.flush();
    },
    [],
  );

  let body;
  if (!folder) {
    body = (
      <EmptyState
        icon={FolderOpen}
        title="No folder open"
        description="Open a project folder to see its graph: resources, research, mathematics, and how they relate."
        actions={
          <Button variant="primary" onPress={() => void registry.runCommand("workspace.openFolder")}>
            Open Folder…
          </Button>
        }
      />
    );
  } else if (statusError) {
    body = <ErrorState title="The project graph could not be read" description={statusError} actions={<Button onPress={() => void graph.resync()}>Retry</Button>} />;
  } else if (!status) {
    body = <LoadingState title="Opening the project graph…" />;
  } else if (status.state.kind === "unavailable") {
    const { reason, recovery } = status.state;
    body = (
      <UnavailableState
        title="The project graph is unavailable"
        description={
          <>
            <p>{reason}</p>
            <p>Editing files keeps working. Graph-dependent actions are paused until the graph is recovered.</p>
          </>
        }
        actions={recovery.map((option) => (
          <Button key={recoveryLabel(option)} icon={option.kind === "retry" ? RotateCcw : undefined} onPress={() => void recover(option)}>
            {recoveryLabel(option)}
          </Button>
        ))}
      />
    );
  } else if (status.state.kind === "detached") {
    body = <LoadingState title="Attaching the project graph…" />;
  } else {
    body = section === "project" ? <ProjectGraph store={projectViewStore} /> : <Mathematics store={mathViewStore} />;
  }

  return (
    <div className="xp-graph-page" data-testid="graph-page">
      <header className="xp-graph-page__header" data-region="graph-header" aria-label="Graph page">
        <h1 className="xp-graph-page__title">Graph</h1>
        <SegmentedControl<GraphSection>
          aria-label="Graph section"
          value={section}
          onChange={(next) => useGraphPage.setState({ section: next })}
          options={[
            { id: "project", label: "Project", icon: Network },
            { id: "mathematics", label: "Mathematics", icon: Sigma },
          ]}
        />
        <span className="xp-graph-toolbar__spacer" />
        {status && readable && (
          <>
            <FreshnessPill freshness={status.freshness} />
            <span className="xp-muted xp-graph-page__counts" data-testid="graph-counts">
              {status.entities} items · {status.relations} relationships · revision {status.revision}
            </span>
          </>
        )}
      </header>
      {status?.freshness.state === "pending" && readable && (
        <Banner tone="warning" title="The graph is catching up with recent file changes">
          Answers may not include the latest edits yet; they are labelled when they come from unsaved buffers.
        </Banner>
      )}
      <div className="xp-graph-page__body">{body}</div>
    </div>
  );
}
