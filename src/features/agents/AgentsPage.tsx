// The Agents page: detailed task and workflow management over the same agent
// service and store as the Agent view (the inline Editor entry point). Tasks,
// transcripts, approvals, and reviews are shared; this page only adds its own
// navigation (selected task, section, filter), so moving between Editor and
// Agents never duplicates a task or loses an approval or review.
import { Archive, Bot, BookmarkCheck, FolderOpen, ListTodo, Network, PanelRight, Plus, Square } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

import type { GraphSnapshot } from "../../bindings/GraphSnapshot";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import { EDITOR_PAGE, pages, usePages } from "../../app/pages";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  EmptyState,
  SegmentedControl,
  StatusPill,
  Tab,
  TabList,
  TabPanel,
  Tabs,
} from "../../ui/components";
import { isActive, relativeTime, statusLabel } from "./format";
import { agentGraph } from "./graphIpc";
import { useOpList } from "../operations/useOpList";
import { GraphContextView, kindTitle, ProposalList } from "./GraphViews";
import { ReviewView } from "./ReviewView";
import { AGENT_VIEW_ID, AGENTS_PAGE_ID, agents, type PageTab, type TaskFilter, useAgents } from "./store";
import { TaskComposer } from "./TaskComposer";
import { taskActions } from "./taskOps";
import { TaskView } from "./TaskView";
import "./agents.css";

const FILTERS: { id: TaskFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "active", label: "Running" },
  { id: "attention", label: "Needs you" },
  { id: "kept", label: "Kept" },
];

/** Whether a task belongs in a filter (pure; unit tested). */
export function taskMatches(task: TaskSummary, filter: TaskFilter, kept: ReadonlySet<string>): boolean {
  switch (filter) {
    case "all":
      return true;
    case "active":
      return isActive(task.status);
    case "attention":
      return task.status.kind === "awaitingApproval" || task.pendingReview > 0;
    case "kept":
      return kept.has(task.id);
  }
}

/** Task ids the project graph keeps as investigations. */
export function keptTaskIds(snapshot: GraphSnapshot | null): Set<string> {
  const ids = new Set<string>();
  for (const entity of snapshot?.entities ?? []) {
    const task = entity.kind === "investigation" ? entity.attributes.agentTask : undefined;
    if (task) ids.add(task);
  }
  return ids;
}

function PageTaskList({ kept }: { kept: ReadonlySet<string> }) {
  const tasks = useAgents((s) => s.tasks);
  const page = useAgents((s) => s.page);
  const shown = tasks.filter((t) => taskMatches(t, page.filter, kept));
  const list = useOpList({ id: "agent-tasks-page", label: "Task actions", items: shown, keyOf: (t) => t.id, actionsFor: taskActions });
  return (
    <div className="agents-stack">
      <SegmentedControl
        aria-label="Show tasks"
        value={page.filter}
        onChange={(filter) => agents.setPage({ filter })}
        options={FILTERS}
      />
      {shown.length === 0 ? (
        <p className="agents-muted agents-small">{tasks.length === 0 ? "No agent tasks yet." : "No tasks match this filter."}</p>
      ) : (
        <div ref={list.ref} className="xp-ops-surface">
        <ul className="agents-task-list" aria-label="Agent tasks" data-testid="agents-page-tasks">
          {shown.map((task) => {
            const pill = statusLabel(task.status);
            const selected = page.selected === task.id && !page.composing;
            const op = list.itemProps(task.id);
            return (
              <li
                key={task.id}
                data-op-item={op["data-op-item"]}
                data-op-selected={op["data-op-selected"]}
                className="agents-task-row"
                onMouseDown={op.onMouseDown}
                onClick={op.onClick}
                onFocus={op.onFocus}
                onContextMenu={op.onContextMenu}
              >
                <button
                  type="button"
                  className="agents-task-item"
                  aria-current={selected ? "true" : undefined}
                  data-selected={selected || undefined}
                  onClick={() => agents.setPage({ selected: task.id, composing: false })}
                  data-testid="agents-page-task"
                  data-task-id={task.id}
                >
                  <span className="agents-task-item__title">{task.title}</span>
                  <span className="agents-task-item__meta">
                    <StatusPill status={pill.status} label={pill.label} size="sm" />
                    {task.pendingReview > 0 && (
                      <StatusPill status="warning" label={`${task.pendingReview} to review`} size="sm" />
                    )}
                    {kept.has(task.id) && <Badge tone="info">Kept</Badge>}
                    <span className="agents-muted">{relativeTime(task.updatedAtMs)}</span>
                  </span>
                </button>
                {list.moreButton(task.id, task.title)}
              </li>
            );
          })}
        </ul>
        {list.contextMenu}
        </div>
      )}
    </div>
  );
}

/** Saved investigations and workflow definitions in the project graph. */
function KeptEntities({ snapshot, error }: { snapshot: GraphSnapshot | null; error: string | null }) {
  const tasks = useAgents((s) => s.tasks);
  if (error) return <p className="agents-muted agents-small">{error}</p>;
  const entities = snapshot?.entities ?? [];
  if (entities.length === 0) {
    return (
      <p className="agents-muted agents-small">
        Nothing kept yet. Keep a task as an investigation to admit it into the project graph; saved workflow drafts also
        appear here.
      </p>
    );
  }
  return (
    <ul className="agents-list" aria-label="Kept in the project graph" data-testid="agents-kept">
      {entities.map((entity) => {
        const task = entity.attributes.agentTask;
        const known = task ? tasks.find((t) => t.id === task) : undefined;
        // A workflow draft opens in the workflow editor (review, edit, save; it runs only from a plan the user starts).
        const workflowId = entity.kind === "workflow" ? entity.attributes["workflow.id"] : undefined;
        return (
          <li key={entity.id} data-testid="agents-kept-entity" data-entity-id={entity.id} data-kind={entity.kind}>
            <Badge tone="neutral">{kindTitle(entity.kind)}</Badge> {entity.label}
            {known && (
              <Button size="sm" variant="ghost" onPress={() => agents.setPage({ selected: known.id, composing: false })}>
                Open task
              </Button>
            )}
            {workflowId && (
              <Button
                size="sm"
                variant="ghost"
                onPress={() => editors.open({ key: `workflow:${workflowId}`, title: entity.label })}
                data-testid="agents-kept-open-workflow"
              >
                Open workflow
              </Button>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function TaskHeader({ task, kept, onKept }: { task: TaskSummary; kept: boolean; onKept: () => void }) {
  const [busy, setBusy] = useState(false);
  const pill = statusLabel(task.status);
  const active = isActive(task.status);
  async function keep() {
    setBusy(true);
    const entity = await agents.run(() => agentGraph.keepInvestigation(task.id));
    setBusy(false);
    if (entity) {
      notify({ tone: "success", title: "Kept as an investigation", body: task.title });
      onKept();
    }
  }
  return (
    <div className="agents-stack">
      <h2 className="agents-page__title">{task.title}</h2>
      <div className="agents-row">
        <span data-testid="agents-page-status" data-status={task.status.kind}>
          <StatusPill status={pill.status} label={pill.label} />
        </span>
        {active && (
          <Button size="sm" variant="secondary" icon={Square} onPress={() => void agents.run(() => commands.agentsTaskCancel(task.id))}>
            Cancel
          </Button>
        )}
        <Button
          size="sm"
          icon={kept ? BookmarkCheck : Archive}
          isDisabled={active || kept}
          isPending={busy}
          onPress={() => void keep()}
          data-testid="agents-keep-investigation"
        >
          {kept ? "Kept as investigation" : "Keep as investigation"}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          icon={PanelRight}
          onPress={() => {
            pages.show(EDITOR_PAGE);
            layout.showView("auxiliary", AGENT_VIEW_ID);
            agents.show("task", task.id);
          }}
          data-testid="agents-show-in-panel"
        >
          Show in Agent panel
        </Button>
      </div>
    </div>
  );
}

function TaskDetailSections({ id, kept, onKept }: { id: TaskId; kept: boolean; onKept: () => void }) {
  const task = useAgents((s) => s.details[id]?.summary ?? s.tasks.find((t) => t.id === id));
  const graph = useAgents((s) => s.details[id]?.graph);
  const tab = useAgents((s) => s.page.tab);
  if (!task) return null;
  const proposals = graph?.proposals ?? [];
  const waiting = proposals.filter((p) => p.state === "awaitingApproval").length;
  return (
    <div className="agents-stack" data-testid="agents-page-task-detail" data-task-id={id}>
      <TaskHeader task={task} kept={kept} onKept={onKept} />
      <Tabs selectedKey={tab} onSelectionChange={(key) => agents.setPage({ tab: key as PageTab })}>
        <TabList aria-label="Task sections">
          <Tab id="run" icon={ListTodo}>
            Run
          </Tab>
          <Tab id="graph" icon={Network}>
            Graph context
          </Tab>
          <Tab id="proposals">{`Proposals (${proposals.length}${waiting ? `, ${waiting} waiting` : ""})`}</Tab>
          <Tab id="changes">{`Changes (${task.changedFiles})`}</Tab>
        </TabList>
        <TabPanel id="run" className="agents-page__panel">
          <TaskView id={id} variant="page" />
        </TabPanel>
        <TabPanel id="graph" className="agents-page__panel">
          {graph ? <GraphContextView graph={graph} /> : null}
        </TabPanel>
        <TabPanel id="proposals" className="agents-page__panel">
          <ProposalList proposals={proposals} />
        </TabPanel>
        <TabPanel id="changes" className="agents-page__panel">
          {task.changedFiles > 0 ? (
            <ReviewView id={id} showBack={false} />
          ) : (
            <p className="agents-muted agents-small">The task has not changed any file.</p>
          )}
        </TabPanel>
      </Tabs>
    </div>
  );
}

export function AgentsPage() {
  const page = useAgents((s) => s.page);
  const error = useAgents((s) => s.error);
  const workspace = useAgents((s) => s.workspace);
  const taskCount = useAgents((s) => s.tasks.length);
  const showing = usePages((s) => s.active === AGENTS_PAGE_ID);
  const [kept, setKept] = useState<GraphSnapshot | null>(null);
  const [keptError, setKeptError] = useState<string | null>(null);

  const refreshKept = useCallback(async () => {
    try {
      setKept(await agentGraph.keptEntities());
      setKeptError(null);
    } catch (e) {
      setKept(null);
      setKeptError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    void agents.init();
  }, []);

  useEffect(() => {
    if (showing) void refreshKept();
  }, [showing, workspace?.root, taskCount, refreshKept]);

  const keptIds = keptTaskIds(kept);
  return (
    <div className="agents agents-page" data-testid="agents-page">
      <aside className="agents-page__list" aria-label="Agent tasks and kept work" data-region="agents-tasks">
        <div className="agents-row">
          <h2 className="agents-page__title">Agents</h2>
          <Button
            variant={workspace ? "primary" : "secondary"}
            size="sm"
            icon={Plus}
            onPress={() => agents.setPage({ composing: true })}
            data-testid="agents-page-new-task"
          >
            New task
          </Button>
        </div>
        <PageTaskList kept={keptIds} />
        <section className="agents-stack" aria-label="Kept in the project graph">
          <h3 className="agents-page__subtitle">Kept in the project graph</h3>
          <KeptEntities snapshot={kept} error={keptError} />
        </section>
      </aside>
      <section className="agents-page__detail" aria-label="Selected agent task" data-region="agents-detail">
        {error && (
          <div data-testid="agents-page-error">
            <Banner
              tone="danger"
              title="Agent action failed"
              live
              actions={
                <Button size="sm" variant="ghost" onPress={() => agents.setError(null)}>
                  Dismiss
                </Button>
              }
            >
              {error}
            </Banner>
          </div>
        )}
        {page.composing ? (
          <TaskComposer
            onCreated={(id) => agents.setPage({ selected: id, composing: false, tab: "run" })}
            onCancel={() => agents.setPage({ composing: false })}
          />
        ) : page.selected ? (
          <TaskDetailSections id={page.selected} kept={keptIds.has(page.selected)} onKept={() => void refreshKept()} />
        ) : !workspace ? (
          <EmptyState
            icon={FolderOpen}
            title="No folder open"
            description="Agent tasks work on an open project folder, through the same documents as your editors. Open a folder to start or continue tasks."
            actions={
              <Button variant="primary" icon={FolderOpen} onPress={() => void registry.runCommand("workspace.openFolder")}>
                Open Folder…
              </Button>
            }
          />
        ) : (
          <EmptyState
            icon={Bot}
            title="Select a task"
            description="Tasks from the Agent panel and this page are the same tasks. Select one to see its run, the graph context it was given, the graph changes it proposed, and its reviewable changes."
            actions={
              <Button icon={Plus} onPress={() => agents.setPage({ composing: true })}>
                New task
              </Button>
            }
          />
        )}
      </section>
    </div>
  );
}
