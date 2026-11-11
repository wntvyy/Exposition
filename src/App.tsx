import { useEffect, useState, useSyncExternalStore } from "react";

import type { AppInfo } from "./bindings/AppInfo";
import { registry } from "./app/registry";
import { commands, errorMessage } from "./lib/ipc";

// Bootstrap shell. The workbench (UI owner) replaces this component.
export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    commands.appInfo().then(setInfo, (e: unknown) => setError(errorMessage(e)));
  }, []);

  return (
    <main className="bootstrap" data-testid="bootstrap-shell">
      <h1>Exposition</h1>
      {info && (
        <p data-testid="app-info">
          {info.name} {info.version} · {info.platform}/{info.arch}
          {info.debugBuild ? " · debug" : ""}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      <BootstrapPanels />
    </main>
  );
}

// Minimal host for registered panel views and status items, so features are
// usable before the workbench shell lands. Replaced by the workbench.
function BootstrapPanels() {
  useSyncExternalStore(registry.subscribe, registry.getVersion);
  const views = registry.viewsAt("panel");
  const [active, setActive] = useState<string | null>(null);
  const current = views.find((v) => v.id === active) ?? views[0];
  if (!current) return null;
  return (
    <>
      <div role="tablist" aria-label="Panels" className="bootstrap-tabs">
        {views.map((view) => (
          <button
            key={view.id}
            type="button"
            role="tab"
            id={`tab-${view.id}`}
            aria-selected={view.id === current.id}
            aria-controls={`panel-${view.id}`}
            onClick={() => setActive(view.id)}
          >
            <view.icon size={14} aria-hidden /> {view.title}
          </button>
        ))}
      </div>
      <div role="tabpanel" id={`panel-${current.id}`} aria-labelledby={`tab-${current.id}`} className="bootstrap-panel">
        {current.render()}
      </div>
      <footer className="bootstrap-status" aria-label="Status bar">
        {registry.statusItemsAt("left").map((item) => (
          <span key={item.id}>{item.render()}</span>
        ))}
      </footer>
    </>
  );
}
