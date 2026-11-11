import { Columns2, Command as CommandIcon, FileQuestion, Files, PanelRightClose } from "lucide-react";
import { Group, Panel as ResizablePanel } from "react-resizable-panels";

import { Button, IconButton, SegmentedControl } from "../../ui/components/Button";
import { Keybinding } from "../../ui/components/Kbd";
import { ResizeHandle } from "../../ui/components/ResizeHandle";
import { EmptyState, UnavailableState } from "../../ui/components/States";
import { EditorTabList, TabPanel, Tabs } from "../../ui/components/Tabs";
import { editors, useEditors, type EditorGroupId, type OpenEditor } from "../editors";
import { layout, useLayout } from "../layout";
import { registry, type DocumentMode } from "../registry";
import { focusNextRegion } from "./regions";
import { useRegistryVersion } from "./useRegistry";
import { ViewErrorBoundary } from "./ViewErrorBoundary";

const modeLabels: Record<DocumentMode, string> = { source: "Source", page: "Page", split: "Split" };

const groupNames: Record<EditorGroupId, string> = { primary: "Left editor group", secondary: "Right editor group" };

function EditorHost({ editor, groupActive }: { editor: OpenEditor; groupActive: boolean }) {
  const contribution = registry.editorFor(editor.resource);
  if (!contribution) {
    return (
      <UnavailableState
        icon={FileQuestion}
        title={`No editor for ${editor.resource.title}`}
        description="No editor that can open this kind of resource is available yet."
      />
    );
  }
  const Component = contribution.component;
  const mode = contribution.modes.includes(editor.mode) ? editor.mode : (contribution.modes[0] ?? "source");
  return (
    <ViewErrorBoundary name={`${editor.resource.title} editor`}>
      <Component resource={editor.resource} mode={mode} active={groupActive} />
    </ViewErrorBoundary>
  );
}

/** Close every editor in a group (each honoring its close guard). */
export async function closeGroup(id: EditorGroupId): Promise<void> {
  for (const editor of [...useEditors.getState().groups[id].editors]) {
    if (!(await editors.closeInGroup(editor.resource.key, id))) return;
  }
}

function EmptyGroup({ id }: { id: EditorGroupId }) {
  return (
    <section
      className="wb-editor-group"
      data-group={id}
      data-group-empty
      aria-label={groupNames[id]}
      onFocusCapture={() => editors.focusGroup(id)}
    >
      <EmptyState
        icon={Files}
        title="No editor open"
        description="Editors for project files open here. Every action is available from the command palette."
        actions={
          <Button variant="secondary" icon={CommandIcon} onPress={() => void registry.runCommand("workbench.action.showCommands")}>
            Show all commands
            <Keybinding binding="Mod+Shift+P" className="wb-editor-empty__keys" />
          </Button>
        }
      />
    </section>
  );
}

function EditorGroupView({ id }: { id: EditorGroupId }) {
  useRegistryVersion();
  const group = useEditors((s) => s.groups[id]);
  const isActive = useEditors((s) => s.activeGroup === id);
  if (group.editors.length === 0) return <EmptyGroup id={id} />;

  const active = group.editors.find((e) => e.resource.key === group.activeKey) ?? group.editors[0]!;
  const contribution = registry.editorFor(active.resource);
  const modes = contribution?.modes ?? [];
  const mode = modes.includes(active.mode) ? active.mode : modes[0];

  return (
    <section
      className="wb-editor-group"
      data-group={id}
      data-group-active={isActive || undefined}
      aria-label={groupNames[id]}
      onFocusCapture={() => editors.focusGroup(id)}
      onPointerDownCapture={() => editors.focusGroup(id)}
    >
      <Tabs
        className="wb-editor-group__tabs"
        selectedKey={active.resource.key}
        onSelectionChange={(key) => editors.activate(String(key), id)}
      >
        <div className="wb-editor-group__header">
          <EditorTabList
            aria-label={`Open editors, ${groupNames[id].toLowerCase()}`}
            items={group.editors.map((e) => ({
              key: e.resource.key,
              title: e.resource.title,
              dirty: e.dirty,
              preview: e.preview,
              description: e.resource.path ?? e.resource.title,
            }))}
            onClose={(key) => void editors.closeInGroup(key, id)}
            onDoubleClick={(key) => {
              const editor = group.editors.find((e) => e.resource.key === key);
              if (editor?.preview) editors.open(editor.resource, { group: id, preview: false });
            }}
          />
          <div className="wb-editor-group__actions">
            {modes.length > 1 && mode && (
              <SegmentedControl
                aria-label="Document mode"
                value={mode}
                onChange={(next) => editors.setMode(active.resource.key, next)}
                options={modes.map((m) => ({ id: m, label: modeLabels[m] }))}
              />
            )}
            {id === "primary" ? (
              <IconButton
                icon={Columns2}
                size="sm"
                aria-label="Split editor right"
                keybinding="Mod+\\"
                onPress={() => void registry.runCommand("workbench.action.splitEditor")}
              />
            ) : (
              <IconButton
                icon={PanelRightClose}
                size="sm"
                aria-label="Close editor group"
                onPress={() => void closeGroup("secondary")}
              />
            )}
          </div>
        </div>
        {group.editors.map((editor) => (
          <TabPanel
            key={editor.resource.key}
            id={editor.resource.key}
            shouldForceMount
            className="wb-editor-group__panel"
          >
            <EditorHost editor={editor} groupActive={isActive} />
          </TabPanel>
        ))}
      </Tabs>
    </section>
  );
}

/** Center editor area: one group, or two side-by-side groups when the right group has editors. */
export function EditorArea() {
  const showSecondary = useEditors((s) => s.groups.secondary.editors.length > 0);
  const split = useLayout((s) => s.editorSplit);
  return (
    <div className="wb-editor-area" data-region="editor" aria-label="Editors" role="region">
      <Group
        id="wb-editor-groups"
        orientation="horizontal"
        className="wb-editor-area__groups"
        onLayoutChanged={(sizes, meta) => {
          const primary = sizes["wb-group-primary"];
          if (meta.isUserInteraction && showSecondary && primary !== undefined) layout.setEditorSplit(primary);
        }}
      >
        <ResizablePanel id="wb-group-primary" minSize="15%" defaultSize={showSecondary ? `${split}%` : "100%"}>
          <EditorGroupView id="primary" />
        </ResizablePanel>
        {showSecondary && (
          <>
            <ResizeHandle aria-label="Resize editor groups" onRegionKey={focusNextRegion} />
            <ResizablePanel id="wb-group-secondary" minSize="15%" defaultSize={`${100 - split}%`}>
              <EditorGroupView id="secondary" />
            </ResizablePanel>
          </>
        )}
      </Group>
    </div>
  );
}
