// Toolchains view: discovered tools, versions, and guidance for missing ones.
import { useEffect } from "react";
import { RefreshCw } from "lucide-react";

import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import { Button, ErrorState, LoadingState, StatusPill, Toolbar, type Status } from "../../ui/components";
import { loadToolchains, useJobs } from "./store";
import "./jobs.css";

const STATUS: Record<ToolchainInfo["status"], { status: Status; label: string }> = {
  available: { status: "succeeded", label: "Available" },
  missing: { status: "unavailable", label: "Missing" },
  broken: { status: "failed", label: "Not working" },
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

  if (toolchains === null) {
    if (error) {
      return (
        <ErrorState
          title="Could not check toolchains"
          details={error}
          actions={<Button onPress={() => void loadToolchains(true)}>Retry</Button>}
        />
      );
    }
    return <LoadingState title="Checking installed tools…" />;
  }

  const groups = new Map<string, ToolchainInfo[]>();
  for (const tool of toolchains) groups.set(tool.category, [...(groups.get(tool.category) ?? []), tool]);

  return (
    <div className="exp-tools" data-testid="toolchains-view" role="region" aria-label="Toolchains" aria-busy={loading}>
      <Toolbar aria-label="Toolchain actions" className="exp-jobs__toolbar">
        <Button size="sm" icon={RefreshCw} isPending={loading} onPress={() => void loadToolchains(true)}>
          {loading ? "Checking…" : "Refresh"}
        </Button>
        <span className="exp-muted">Exposition finds tools on this computer; it never installs them.</span>
      </Toolbar>
      {[...groups.entries()].map(([category, tools]) => (
        <table key={category} className="exp-tools__table">
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
                  <StatusPill size="sm" status={STATUS[tool.status].status} label={STATUS[tool.status].label} />
                </td>
                <td data-testid="tool-version">{tool.version ?? "—"}</td>
                <td>
                  {tool.path && <code className="exp-tools__path">{tool.path}</code>}
                  {tool.detail && <div className="exp-muted">{tool.detail}</div>}
                  {tool.guidance && (
                    <div className="exp-tools__guidance" data-testid="tool-guidance">
                      {tool.guidance}
                    </div>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ))}
    </div>
  );
}
