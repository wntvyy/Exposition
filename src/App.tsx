import { useEffect, useState } from "react";

import type { AppInfo } from "./bindings/AppInfo";
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
    </main>
  );
}
