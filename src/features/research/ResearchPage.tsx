// The Research page: a code/data exploration suite over the project's
// research space. Explorer (groups over graph identities), Monaco editing of
// research sources (the shared document model: one model per document, also
// used by the Editor page), the user's research session (shared with the
// bottom Research panel: one set of sessions and runs), bounded outputs with
// figure/table inspection, retention, and the inspector (provenance and
// graph dependencies). Enabling Research launches nothing; research works
// without any manuscript.
import { FlaskConical, FolderOpen, Play, PlayCircle, TextSelect, X } from "lucide-react";
import { useEffect, useState } from "react";
import { Group, Panel as ResizablePanel } from "react-resizable-panels";

import { useLayout } from "../../app/layout";
import { notify } from "../../app/notifications";
import { focusNextRegion } from "../../app/shell/regions";
import {
  Banner,
  Button,
  EditorTabList,
  EmptyState,
  IconButton,
  LoadingState,
  ResizeHandle,
  TabPanel,
  Tabs,
  Toolbar,
} from "../../ui/components";
import { SourceEditor, handleForPath, type DocumentHandle } from "../editor";
import { ExecutionModes } from "./ExecutionLabel";
import { ResearchExplorer } from "./Explorer";
import { ResearchInspector } from "./Inspector";
import { CodeInput, EnvironmentNotice, RunHistory, RunOutputView, SessionBar } from "./ResearchPanel";
import {
  closeSource,
  enableResearch,
  reconcilePromotion,
  selectItem,
  startSpaceTracking,
  useSpace,
} from "./space";
import { execute, loadEnvironments, openProjectFolder, runFile, startResearchTracking, useResearch } from "./store";
import "./research.css";

const SCRIPT = /\.(py|ipy|r|jl)$/i;

/** Run the selection of a research source (pinned to the revision the editor shows). */
async function runSourceSelection(path: string): Promise<void> {
  const handle = handleForPath(path);
  if (!handle) return;
  for (let i = 0; i < 40 && handle.sync.pendingCount > 0; i += 1) await new Promise((r) => setTimeout(r, 50));
  const range = handle.selection[0];
  if (!range || range.start === range.end || handle.sync.pendingCount > 0) {
    notify({ tone: "info", title: "Nothing selected", body: "Select code in the source first." });
    return;
  }
  const ordered = range.start <= range.end ? range : { start: range.end, end: range.start };
  await execute({ kind: "selection", document: handle.id, revision: handle.sync.revision, range: ordered });
}

const closeWatchers = new Map<string, () => void>();

function onSourceEditor(path: string) {
  return (_editor: unknown, handle: DocumentHandle | null) => {
    closeWatchers.get(path)?.();
    closeWatchers.delete(path);
    if (!handle) return;
    // The service closed the document (e.g. closing its Editor tab closed it):
    // drop the research tab too instead of showing a disposed model.
    const off = handle.subscribe(() => {
      if (!handle.closed) return;
      off();
      closeWatchers.delete(path);
      useSpace.setState((s) => ({
        openSources: s.openSources.filter((p) => p !== path),
        activeSource: s.activeSource === path ? (s.openSources.find((p) => p !== path) ?? null) : s.activeSource,
      }));
    });
    closeWatchers.set(path, off);
  };
}

function SourceArea() {
  const open = useSpace((s) => s.openSources);
  const active = useSpace((s) => s.activeSource);
  if (open.length === 0 || !active) {
    return (
      <div className="exp-rpage__sources" data-region="research-sources" aria-label="Research sources">
      <EmptyState
        fill
        icon={FlaskConical}
        title="No source open"
        description="Select a source or data file in the research explorer to edit it here. Files open in the same document model as the Editor page."
      />
      </div>
    );
  }
  const script = SCRIPT.test(active);
  return (
    <div className="exp-rpage__sources" data-testid="research-sources" data-active={active} data-region="research-sources" aria-label="Research sources">
      <Tabs
        className="exp-rpage__tabs"
        selectedKey={active}
        onSelectionChange={(key) => {
          useSpace.setState({ activeSource: String(key) });
          selectItem({ kind: "file", path: String(key) });
        }}
      >
        <div className="exp-rpage__tabbar">
          <EditorTabList
            aria-label="Open research sources"
            items={open.map((p) => ({ key: p, title: p.split("/").pop() ?? p, description: p }))}
            onClose={(key) => void closeSource(key)}
          />
          <Toolbar aria-label="Run" className="exp-rpage__run">
            <Button size="sm" variant="primary" icon={Play} isDisabled={!script} onPress={() => void runFile(active)}>
              Run file
            </Button>
            <Button size="sm" icon={TextSelect} isDisabled={!script} onPress={() => void runSourceSelection(active)}>
              Run selection
            </Button>
          </Toolbar>
        </div>
        {open.map((path) => (
          <TabPanel key={path} id={path} shouldForceMount className="exp-rpage__panel">
            <SourceEditor path={path} active={path === active} onEditor={onSourceEditor(path)} />
          </TabPanel>
        ))}
      </Tabs>
    </div>
  );
}

function PendingPromotions() {
  const promotions = useSpace((s) => s.promotions);
  const pending = promotions?.unresolved ?? [];
  if (pending.length === 0) return null;
  return (
    <div data-testid="research-pending-promotions" data-count={pending.length}>
      <Banner tone="warning" title={`${pending.length} promotion${pending.length === 1 ? "" : "s"} need reconciliation`}>
        <ul className="exp-graph__list">
          {pending.map((r) => (
            <li key={r.id} data-operation={r.id}>
              {r.description} —{" "}
              <span className="exp-muted">
                {r.state.kind === "needsReconciliation" ? r.state.message : "interrupted before it finished"}
              </span>{" "}
              <Button size="sm" onPress={() => void reconcilePromotion(r.id)}>
                Reconcile
              </Button>
            </li>
          ))}
        </ul>
      </Banner>
    </div>
  );
}

/** Below this window width the inspector is an overlay drawer instead of a column (as on the Graph page). */
const NARROW_WIDTH = 1100;

/** Narrow windows: the inspector opens over the work area when something is selected; it can be closed. */
function InspectorDrawer({ selected }: { selected: ReturnType<typeof useSpace.getState>["selected"] }) {
  const [open, setOpen] = useState(Boolean(selected));
  useEffect(() => setOpen(Boolean(selected)), [selected]);
  if (!open || !selected) return null;
  return (
    <aside className="exp-rpage__drawer" aria-label="Inspector" data-region="research-inspector" data-testid="research-inspector-drawer">
      <header className="exp-rpage__drawer-bar">
        <h2 className="exp-rpage__drawer-title">Inspector</h2>
        <IconButton icon={X} size="sm" aria-label="Close inspector" onPress={() => setOpen(false)} />
      </header>
      <div className="exp-rpage__inspector">
        <ResearchInspector item={selected} />
      </div>
    </aside>
  );
}

export function ResearchPage() {
  const narrow = useLayout((s) => s.viewportWidth < NARROW_WIDTH);
  const workspace = useResearch((s) => s.workspace);
  const listing = useSpace((s) => s.listing);
  const loaded = useSpace((s) => s.loaded);
  const enabling = useSpace((s) => s.enabling);
  const error = useSpace((s) => s.error);
  const selected = useSpace((s) => s.selected);

  useEffect(() => {
    startResearchTracking();
    startSpaceTracking();
    if (!useResearch.getState().environments) void loadEnvironments(false);
  }, []);

  if (!workspace) {
    return (
      <div className="exp-rpage" data-testid="research-page" data-state="noFolder">
        <EmptyState
          icon={FlaskConical}
          title="No folder open"
          description="Open a project folder to work with research code, data, and results."
          actions={
            <Button variant="primary" icon={FolderOpen} onPress={() => void openProjectFolder()}>
              Open Folder…
            </Button>
          }
        />
      </div>
    );
  }
  if (!loaded) {
    return (
      <div className="exp-rpage" data-testid="research-page" data-state="loading">
        <LoadingState title="Loading research…" />
      </div>
    );
  }
  const enabled = Boolean(listing?.space);
  return (
    <div className="exp-rpage" data-testid="research-page" data-state={enabled ? "enabled" : "disabled"}>
      <header className="exp-rpage__header" data-region="research-header" aria-label="Research session">
        <div className="exp-rpage__heading">
          <h2 className="exp-rpage__title">Research</h2>
          {listing?.space && (
            <code className="exp-muted" data-testid="research-space-root">
              {listing.space.root}
            </code>
          )}
        </div>
        <SessionBar />
      </header>
      {error && (
        <Banner tone="danger" title="Research error">
          <span data-testid="research-space-error">{error}</span>
        </Banner>
      )}
      {listing?.graphUnavailable && (
        <Banner tone="warning" title="The project graph is unavailable">
          {listing.graphUnavailable} Files can still be edited and run; nothing is recorded in the graph until it is available.
        </Banner>
      )}
      <PendingPromotions />
      <div className="exp-rpage__main">
        <Group orientation="horizontal" className="exp-rpage__body">
          <ResizablePanel id="rp-explorer" defaultSize="20%" minSize="12%">
            <div className="exp-rpage__explorer" data-region="research-explorer" aria-label="Research explorer">
              {enabled ? (
                <ResearchExplorer />
              ) : (
                <div data-testid="research-enable">
                  <EmptyState
                    fill={false}
                    icon={FlaskConical}
                    title="Research is not enabled"
                    description="Enabling creates one research space for this project — Sources, Data, and Notes folders under .exposition/research — and starts nothing. No manuscript is needed."
                    actions={
                      <Button variant="primary" isPending={enabling} onPress={() => void enableResearch()}>
                        Enable Research
                      </Button>
                    }
                  />
                </div>
              )}
              <ExecutionModes />
            </div>
          </ResizablePanel>
          <ResizeHandle aria-label="Resize research explorer" onRegionKey={focusNextRegion} />
          <ResizablePanel id="rp-work" defaultSize="50%" minSize="25%">
            <Group orientation="vertical" className="exp-rpage__work">
              <ResizablePanel id="rp-source" defaultSize="50%" minSize="15%">
                <SourceArea />
              </ResizablePanel>
              <ResizeHandle aria-label="Resize source and output" orientation="horizontal" onRegionKey={focusNextRegion} />
              <ResizablePanel id="rp-output" defaultSize="50%" minSize="15%">
                <div className="exp-research exp-rpage__output" role="region" aria-label="Research output" data-region="research-output">
                  <EnvironmentNotice />
                  <div className="exp-research__main">
                    <div className="exp-research__work">
                      <CodeInput />
                      <RunOutputView />
                    </div>
                    <RunHistory />
                  </div>
                </div>
              </ResizablePanel>
            </Group>
          </ResizablePanel>
          {!narrow && (
            <>
              <ResizeHandle aria-label="Resize inspector" onRegionKey={focusNextRegion} />
              <ResizablePanel id="rp-inspector" defaultSize="30%" minSize="15%">
                <aside className="exp-rpage__inspector" aria-label="Inspector" data-region="research-inspector">
                  <ResearchInspector item={selected} />
                </aside>
              </ResizablePanel>
            </>
          )}
        </Group>
        {narrow && <InspectorDrawer selected={selected} />}
      </div>
      {!enabled && <p className="exp-muted exp-rpage__note"><PlayCircle size={12} aria-hidden /> Runs work without enabling Research.</p>}
    </div>
  );
}
