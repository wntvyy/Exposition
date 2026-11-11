// Toolchains view: discovered tools, versions, and guidance for missing ones.
import { useEffect } from "react";

import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import { loadToolchains, useJobs } from "./store";
import "./jobs.css";

const STATUS_LABEL: Record<ToolchainInfo["status"], string> = {
  available: "Available",
  missing: "Missing",
  broken: "Not working",
};

const STATUS_TONE: Record<ToolchainInfo["status"], string> = {
  available: "success",
  missing: "warning",
  broken: "danger",
};

const CATEGORY_LABEL: Record<string, string> = {
  tex: "TeX",
  python: "Python",
  r: "R",
  julia: "Julia",
  lean: "Lean",
  container: "Restricted execution",
  vcs: "Version control",
};

export function ToolchainsView() {
  const toolchains = useJobs((s) => s.toolchains);
  const loading = useJobs((s) => s.toolchainsLoading);
  const error = useJobs((s) => s.toolchainsError);

  useEffect(() => {
    if (useJobs.getState().toolchains === null) void loadToolchains(false);
  }, []);

  const groups = new Map<string, ToolchainInfo[]>();
  for (const tool of toolchains ?? []) {
    groups.set(tool.category, [...(groups.get(tool.category) ?? []), tool]);
  }

  return (
    <section className="exp-tools" data-testid="toolchains-view" aria-label="Toolchains" aria-busy={loading}>
      <div className="exp-toolbar">
        <h2 className="exp-panel-title">Toolchains</h2>
        <button type="button" className="exp-button" disabled={loading} onClick={() => void loadToolchains(true)}>
          {loading ? "Checking…" : "Refresh"}
        </button>
        <span className="exp-muted">Exposition never installs tools; it finds what is on this computer.</span>
      </div>
      {error && (
        <p role="alert" className="exp-error">
          {error}
        </p>
      )}
      {toolchains === null && loading && <p className="exp-empty">Checking installed tools…</p>}
      {[...groups.entries()].map(([category, tools]) => (
        <table key={category} className="exp-tool-table">
          <caption>{CATEGORY_LABEL[category] ?? category}</caption>
          <thead>
            <tr>
              <th scope="col">Tool</th>
              <th scope="col">Status</th>
              <th scope="col">Version</th>
              <th scope="col">Location / guidance</th>
            </tr>
          </thead>
          <tbody>
            {tools.map((tool) => (
              <tr key={tool.id} data-tool-id={tool.id} data-status={tool.status}>
                <th scope="row">{tool.name}</th>
                <td>
                  <span className={`exp-pill exp-tone-${STATUS_TONE[tool.status]}`}>{STATUS_LABEL[tool.status]}</span>
                </td>
                <td data-testid="tool-version">{tool.version ?? "—"}</td>
                <td>
                  {tool.path && <code className="exp-tool-path">{tool.path}</code>}
                  {tool.detail && <div className="exp-muted">{tool.detail}</div>}
                  {tool.guidance && (
                    <div className="exp-guidance" data-testid="tool-guidance">
                      {tool.guidance}
                    </div>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ))}
    </section>
  );
}
