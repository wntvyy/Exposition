// The workbench shell: activity rail, sidebar, editor area, bottom panel,
// auxiliary panel, status bar, command palette, notifications, and dialogs.
// Regions render whatever features registered in `registry`; the shell never
// imports feature internals.
import "./shell/workbench.css";

import { useEffect, useRef, type ReactNode } from "react";
import { Group, Panel as ResizablePanel, type PanelImperativeHandle } from "react-resizable-panels";

import { ConfirmHost } from "../ui/components/Dialog";
import { NotificationRegion } from "../ui/components/Notifications";
import { ResizeHandle } from "../ui/components/ResizeHandle";
import { installKeybindings } from "./keybindings";
import { layout, SIZE_LIMITS, useLayout, useVisibility, type Region } from "./layout";
import { EDITOR_PAGE, registerPageCommands, usePages } from "./pages";
import { registry } from "./registry";
import { ActivityRail } from "./shell/ActivityRail";
import { registerBuiltins } from "./shell/builtins";
import { CommandPalette } from "./shell/CommandPalette";
import { EditorArea } from "./shell/EditorArea";
import { focusNextRegion, focusRegion, regionElements, trackRegionFocus } from "./shell/regions";
import { StatusBar } from "./shell/StatusBar";
import { useRegistryVersion } from "./shell/useRegistry";
import { ViewContainer } from "./shell/ViewContainer";
import { ViewErrorBoundary } from "./shell/ViewErrorBoundary";

const px = (n: number) => `${n}px`;

function usePanelSizePersistence() {
  const refs = {
    sidebar: useRef<PanelImperativeHandle | null>(null),
    panel: useRef<PanelImperativeHandle | null>(null),
    auxiliary: useRef<PanelImperativeHandle | null>(null),
  };
  const save = (regions: Region[]) => {
    for (const region of regions) {
      const size = refs[region].current?.getSize().inPixels;
      if (size !== undefined && size > 0) layout.setSize(region, size);
    }
  };
  return { refs, save };
}

export function Workbench() {
  const visibility = useVisibility();
  const sizes = {
    sidebar: useLayout((s) => s.sidebar.size),
    panel: useLayout((s) => s.panel.size),
    auxiliary: useLayout((s) => s.auxiliary.size),
  };
  const { refs, save } = usePanelSizePersistence();

  useEffect(() => registerBuiltins(), []);
  useEffect(() => registerPageCommands(), []);
  useEffect(() => installKeybindings(), []);
  useEffect(() => {
    trackRegionFocus();
    const onResize = () => layout.setViewportWidth(window.innerWidth);
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const editorLayout = (
        <Group
          id="wb-columns"
          orientation="horizontal"
          className="wb-columns"
          onLayoutChanged={(_, meta) => {
            if (meta.isUserInteraction) save(["sidebar", "auxiliary"]);
          }}
        >
          {visibility.sidebar && (
            <>
              <ResizablePanel
                id="wb-sidebar"
                panelRef={refs.sidebar}
                defaultSize={px(sizes.sidebar)}
                minSize={px(SIZE_LIMITS.sidebar.min)}
                maxSize={px(SIZE_LIMITS.sidebar.max)}
                groupResizeBehavior="preserve-pixel-size"
              >
                <aside className="wb-region wb-sidebar" data-region="sidebar" aria-label="Sidebar">
                  <ViewContainer region="sidebar" />
                </aside>
              </ResizablePanel>
              <ResizeHandle id="wb-sidebar-handle" aria-label="Resize sidebar" onRegionKey={focusNextRegion} />
            </>
          )}
          <ResizablePanel id="wb-center" minSize={px(280)}>
            <Group
              id="wb-rows"
              orientation="vertical"
              className="wb-rows"
              onLayoutChanged={(_, meta) => {
                if (meta.isUserInteraction) save(["panel"]);
              }}
            >
              <ResizablePanel id="wb-editors" minSize={px(120)}>
                <EditorArea />
              </ResizablePanel>
              {visibility.panel && (
                <>
                  <ResizeHandle
                    id="wb-panel-handle"
                    aria-label="Resize panel"
                    orientation="horizontal"
                    onRegionKey={focusNextRegion}
                  />
                  <ResizablePanel
                    id="wb-panel"
                    panelRef={refs.panel}
                    defaultSize={px(sizes.panel)}
                    minSize={px(SIZE_LIMITS.panel.min)}
                    maxSize="80%"
                    groupResizeBehavior="preserve-pixel-size"
                  >
                    <section className="wb-region wb-panel" data-region="panel" aria-label="Panel">
                      <ViewContainer region="panel" />
                    </section>
                  </ResizablePanel>
                </>
              )}
            </Group>
          </ResizablePanel>
          {visibility.auxiliary && (
            <>
              <ResizeHandle id="wb-auxiliary-handle" aria-label="Resize auxiliary panel" onRegionKey={focusNextRegion} />
              <ResizablePanel
                id="wb-auxiliary"
                panelRef={refs.auxiliary}
                defaultSize={px(sizes.auxiliary)}
                minSize={px(SIZE_LIMITS.auxiliary.min)}
                maxSize={px(SIZE_LIMITS.auxiliary.max)}
                groupResizeBehavior="preserve-pixel-size"
              >
                <aside className="wb-region wb-auxiliary" data-region="auxiliary" aria-label="Auxiliary panel">
                  <ViewContainer region="auxiliary" />
                </aside>
              </ResizablePanel>
            </>
          )}
        </Group>
  );

  return (
    <div className="wb" data-testid="workbench">
      <div className="wb-main">
        <ActivityRail />
        <Pages editor={editorLayout} />
      </div>
      <StatusBar />
      <NotificationRegion />
      <CommandPalette />
      <ConfirmHost />
    </div>
  );
}

/**
 * The page area: the Editor page (the workbench layout) and every registered
 * page that has been visited. Hidden pages stay mounted but `hidden` and
 * `inert`, so their state survives navigation and they take no focus.
 */
function Pages({ editor }: { editor: ReactNode }) {
  useRegistryVersion();
  const active = usePages((s) => s.active);
  const visited = usePages((s) => s.visited);
  const focusRequest = usePages((s) => s.focusRequest);
  const registered = registry.pages().filter((page) => visited.includes(page.id));

  // A page shown from a command or keybinding takes keyboard focus: its first
  // region (the editor area on the Editor page). Waits a frame so a page
  // mounted by this navigation has rendered.
  useEffect(() => {
    if (focusRequest === 0) return;
    const frame = requestAnimationFrame(() => {
      const container = [...document.querySelectorAll<HTMLElement>(".wb-pages > [data-page]")].find(
        (page) => page.dataset.page === active,
      );
      if (!container || container.contains(document.activeElement)) return;
      const inner = regionElements().filter((region) => container.contains(region));
      const target =
        (active === EDITOR_PAGE ? inner.find((region) => region.dataset.region === "editor") : undefined) ?? inner[0];
      if (target) focusRegion(target);
    });
    return () => cancelAnimationFrame(frame);
    // Runs for each request; `active` is read when the request arrives.
  }, [focusRequest]);

  return (
    <div className="wb-pages">
      <div
        className="wb-page wb-page--editor"
        data-page={EDITOR_PAGE}
        hidden={active !== EDITOR_PAGE}
        inert={active !== EDITOR_PAGE}
      >
        {editor}
      </div>
      {registered.map((page) => (
        <div
          key={page.id}
          className="wb-page"
          data-page={page.id}
          data-region={active === page.id ? "page" : undefined}
          aria-label={page.title}
          role="region"
          hidden={active !== page.id}
          inert={active !== page.id}
        >
          <ViewErrorBoundary name={page.title}>{page.render()}</ViewErrorBoundary>
        </div>
      ))}
    </div>
  );
}
