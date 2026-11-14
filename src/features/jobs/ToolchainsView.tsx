// Toolchains view: discovered tools, versions, and guidance for missing ones.
import { useEffect } from "react";
import { RefreshCw } from "lucide-react";

import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import {
  Button,
  ErrorState,
  LoadingState,
  Section,
  SimpleTable,
  StatusText,
  Toolbar,
  type SimpleTableColumn,
  type Status,
} from "../../ui/components";
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

const COLUMNS: SimpleTableColumn<ToolchainInfo>[] = [
  { id: "tool", title: "Tool", width: "22%", isRowHeader: true, render: (tool) => tool.name },
  {
    id: "status",
    title: "Status",
    width: "13%",
    render: (tool) => <StatusText status={STATUS[tool.status].status} label={STATUS[tool.status].label} />,
  },
  { id: "version", title: "Version", width: "27%", render: (tool) => <span data-testid="tool-version">{tool.version ?? "—"}</span> },
  {
    id: "location",
    title: "Location / guidance",
    render: (tool) => (
      <>
        {tool.path && <code className="ui-code exp-tools__path">{tool.path}</code>}
        {tool.detail && <div className="ui-meta">{tool.detail}</div>}
        {tool.guidance && (
          <div className="exp-tools__guidance" data-testid="tool-guidance">
            {tool.guidance}
          </div>
        )}
      </>
    ),
  },
];

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
          actions={
            <Button size="sm" onPress={() => void loadToolchains(true)}>
              Retry
            </Button>
          }
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
        <Button size="sm" variant="ghost" icon={RefreshCw} isPending={loading} onPress={() => void loadToolchains(true)}>
          {loading ? "Checking…" : "Refresh"}
        </Button>
        <span className="ui-meta">Exposition finds tools on this computer and installs nothing automatically.</span>
      </Toolbar>
      {[...groups.entries()].map(([category, tools]) => (
        <Section key={category} title={CATEGORY_LABEL[category] ?? category} count={tools.length}>
          <SimpleTable
            aria-label={CATEGORY_LABEL[category] ?? category}
            className="exp-tools__table"
            columns={COLUMNS}
            rows={tools}
            rowKey={(tool) => tool.id}
            rowProps={(tool) => ({ "data-tool-id": tool.id, "data-status": tool.status })}
          />
        </Section>
      ))}
    </div>
  );
}
