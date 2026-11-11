import { useEffect, useState, useSyncExternalStore } from "react";

import { editors, useEditors } from "./app/editors";
import { dismiss, useNotifications } from "./app/notifications";
import { registry } from "./app/registry";
import type { AppInfo } from "./bindings/AppInfo";
import { commands, errorMessage } from "./lib/ipc";

// Interim workbench (workspace worker): renders registry contributions —
// sidebar views, editor tabs, status items, notifications — and dispatches
// command keybindings, so features can be exercised in the real app until the
// UI owner's workbench replaces this component.

function useRegistry() {
  return useSyncExternalStore(registry.subscribe, registry.getVersion);
}

function matchesKeybinding(binding: string, e: KeyboardEvent) {
  const parts = binding.split("+");
  const key = parts.pop()?.toLowerCase();
  const mods = new Set(parts.map((p) => p.toLowerCase()));
  const mod = navigator.platform.startsWith("Mac") ? e.metaKey : e.ctrlKey;
  return (
    e.key.toLowerCase() === key &&
    mods.has("mod") === mod &&
    mods.has("shift") === e.shiftKey &&
    mods.has("alt") === e.altKey
  );
}

function Keybindings() {
  useRegistry();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const command = registry.allCommands().find((c) => c.keybinding && matchesKeybinding(c.keybinding, e));
      if (!command) return;
      e.preventDefault();
      void registry.runCommand(command.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return null;
}

function EditorArea() {
  useRegistry();
  const group = useEditors((s) => s.groups.primary);
  const active = group.editors.find((e) => e.resource.key === group.activeKey);
  const contribution = active ? registry.editorFor(active.resource) : undefined;
  const Editor = contribution?.component;
  return (
    <section className="interim-editors" aria-label="Editors">
      <div role="tablist" className="interim-tabs" aria-label="Open editors">
        {group.editors.map((e) => (
          <div key={e.resource.key} className="interim-tab" data-active={e.resource.key === group.activeKey}>
            <button
              type="button"
              role="tab"
              aria-selected={e.resource.key === group.activeKey}
              data-testid="editor-tab"
              data-key={e.resource.key}
              data-dirty={e.dirty}
              onClick={() => editors.activate(e.resource.key)}
            >
              {e.dirty ? "● " : ""}
              {e.resource.title}
            </button>
            <button
              type="button"
              aria-label={`Close ${e.resource.title}`}
              data-testid="editor-tab-close"
              onClick={() => void editors.close(e.resource.key)}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <div className="interim-editor">
        {active && Editor ? (
          <Editor key={active.resource.key} resource={active.resource} mode={active.mode} active />
        ) : (
          <p className="interim-muted">Open a file from the Files view.</p>
        )}
      </div>
    </section>
  );
}

function Notifications() {
  const items = useNotifications((s) => s.items);
  useEffect(() => {
    const timers = items
      .filter((n) => n.timeoutMs !== null)
      .map((n) => setTimeout(() => dismiss(n.id), n.timeoutMs ?? (n.tone === "danger" ? 12000 : 6000)));
    return () => timers.forEach(clearTimeout);
  }, [items]);
  return (
    <div className="interim-notifications" role="status" aria-live="polite">
      {items.map((n) => (
        <div key={n.id} className="interim-notification" data-tone={n.tone} data-testid="notification">
          <strong>{n.title}</strong>
          {n.body && <p>{n.body}</p>}
          <div>
            {n.actions?.map((a) => (
              <button key={a.label} type="button" onClick={() => void a.run()}>
                {a.label}
              </button>
            ))}
            <button type="button" onClick={() => dismiss(n.id)}>
              Dismiss
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  useRegistry();

  useEffect(() => {
    commands.appInfo().then(setInfo, (e: unknown) => setError(errorMessage(e)));
  }, []);

  return (
    <main className="interim-shell" data-testid="bootstrap-shell">
      <Keybindings />
      <aside className="interim-sidebar" aria-label="Sidebar">
        {registry.viewsAt("sidebar").map((view) => (
          <section key={view.id} className="interim-view" aria-label={view.title}>
            <h2>{view.title}</h2>
            {view.render()}
          </section>
        ))}
      </aside>
      <EditorArea />
      <footer className="interim-status">
        <span data-testid="app-info">
          {info && `${info.name} ${info.version}${info.debugBuild ? " · debug" : ""}`}
          {error && <span role="alert">{error}</span>}
        </span>
        <span>
          {registry.statusItemsAt("left").map((item) => (
            <span key={item.id}>{item.render()}</span>
          ))}
        </span>
        <span>
          {registry.statusItemsAt("right").map((item) => (
            <span key={item.id}>{item.render()}</span>
          ))}
        </span>
      </footer>
      <Notifications />
    </main>
  );
}
