// The workbench shell: activity rail, sidebar, editor area, bottom panel,
// auxiliary panel, status bar, command palette, notifications, and dialogs.
// Regions render whatever features registered in `registry`; the shell never
// imports feature internals.
import "./shell/workbench.css";

import { useEffect, useRef } from "react";
import { Group, Panel as ResizablePanel, type PanelImperativeHandle } from "react-resizable-panels";

import { ConfirmHost } from "../ui/components/Dialog";
import { NotificationRegion } from "../ui/components/Notifications";
import { ResizeHandle } from "../ui/components/ResizeHandle";
import { installKeybindings } from "./keybindings";
import { layout, SIZE_LIMITS, useLayout, useVisibility, type Region } from "./layout";
import { ActivityRail } from "./shell/ActivityRail";
import { registerBuiltins } from "./shell/builtins";
import { CommandPalette } from "./shell/CommandPalette";
import { EditorArea } from "./shell/EditorArea";
import { focusNextRegion, trackRegionFocus } from "./shell/regions";
import { StatusBar } from "./shell/StatusBar";
import { ViewContainer } from "./shell/ViewContainer";

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
  useEffect(() => installKeybindings(), []);
  useEffect(() => {
    trackRegionFocus();
    const onResize = () => layout.setViewportWidth(window.innerWidth);
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  return (
    <div className="wb" data-testid="workbench">
      <div className="wb-main">
        <ActivityRail />
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
      </div>
      <StatusBar />
      <NotificationRegion />
      <CommandPalette />
      <ConfirmHost />
    </div>
  );
}
