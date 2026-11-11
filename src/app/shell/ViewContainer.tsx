import { Bot, FolderOpen, SquareTerminal, X } from "lucide-react";
import { useId } from "react";

import { IconButton } from "../../ui/components/Button";
import { Panel, PanelHeader } from "../../ui/components/Panel";
import { EmptyState, UnavailableState } from "../../ui/components/States";
import { Tab, TabList, TabPanel, Tabs } from "../../ui/components/Tabs";
import { layout, useLayout, type Region } from "../layout";
import { registry, type ViewContribution } from "../registry";
import { useRegistryVersion } from "./useRegistry";
import { ViewErrorBoundary } from "./ViewErrorBoundary";

const regionNames: Record<Region, string> = {
  sidebar: "Sidebar",
  panel: "Panel",
  auxiliary: "Auxiliary panel",
};

const hideKeybindings: Record<Region, string> = {
  sidebar: "Mod+B",
  panel: "Mod+J",
  auxiliary: "Mod+Alt+B",
};

/** Honest placeholder when no feature has contributed a view to a region yet. */
function RegionFallback({ region }: { region: Region }) {
  switch (region) {
    case "sidebar":
      return (
        <UnavailableState
          icon={FolderOpen}
          title="No folder open"
          description="Opening a project folder is not available in this build yet."
        />
      );
    case "panel":
      return (
        <EmptyState
          icon={SquareTerminal}
          title="No output"
          description="Build output, research runs, and terminals will appear here. They are not available yet."
        />
      );
    case "auxiliary":
      return (
        <UnavailableState
          icon={Bot}
          title="Not available yet"
          description="Agent tasks and change review will appear in this panel."
        />
      );
  }
}

export function useActiveView(region: Region): { views: ViewContribution[]; active: ViewContribution | undefined } {
  useRegistryVersion();
  const activeId = useLayout((s) => s[region].activeView);
  const views = registry.viewsAt(region);
  return { views, active: views.find((v) => v.id === activeId) ?? views[0] };
}

/**
 * Renders the views registered for a region. The sidebar shows one view at a
 * time (chosen from the activity rail) under a header; the panel and
 * auxiliary regions show a tab strip of their views.
 */
export function ViewContainer({ region }: { region: Region }) {
  const { views, active } = useActiveView(region);
  const headingId = useId();
  const hide = (
    <IconButton
      icon={X}
      size="sm"
      aria-label={`Hide ${regionNames[region].toLowerCase()}`}
      keybinding={hideKeybindings[region]}
      onPress={() => layout.setVisible(region, false)}
    />
  );

  if (region === "sidebar" || views.length === 0) {
    const title = active?.title ?? (region === "sidebar" ? "Project" : region === "panel" ? "Output" : "Assistant");
    return (
      <Panel className="wb-view-container" aria-labelledby={headingId}>
        <PanelHeader id={headingId} title={title} actions={hide} />
        <div className="wb-view-container__body">
          {active ? (
            <ViewErrorBoundary key={active.id} name={`${active.title} view`}>
              {active.render()}
            </ViewErrorBoundary>
          ) : (
            <RegionFallback region={region} />
          )}
        </div>
      </Panel>
    );
  }

  return (
    <Tabs
      className="wb-view-container wb-view-container--tabbed"
      selectedKey={active?.id}
      onSelectionChange={(key) => layout.setActiveView(region, String(key))}
    >
      <div className="wb-view-container__tabbar">
        <TabList aria-label={`${regionNames[region]} views`} className="wb-view-container__tabs">
          {views.map((view) => (
            <Tab key={view.id} id={view.id} icon={view.icon}>
              {view.title}
            </Tab>
          ))}
        </TabList>
        <div className="wb-view-container__actions">{hide}</div>
      </div>
      {views.map((view) => (
        <TabPanel key={view.id} id={view.id} className="wb-view-container__body">
          <ViewErrorBoundary name={`${view.title} view`}>{view.render()}</ViewErrorBoundary>
        </TabPanel>
      ))}
    </Tabs>
  );
}
