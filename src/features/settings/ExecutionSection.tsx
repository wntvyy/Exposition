// Execution: the restricted (Docker) backend's availability and image (local
// images only), what restricted runs enforce, and tool location overrides.
// Every change is validated by the execution service.
import { useEffect, useState } from "react";
import { FolderSearch, RefreshCw, RotateCcw, X } from "lucide-react";

import { Banner, Button, Dialog, ErrorState, LoadingState, Select, StatusPill, TextField, type Status } from "../../ui/components";
import { loadToolchains, useJobs } from "../jobs/store";
import { dockerAvailability, imageOptions, imageState, restrictedGuarantees, SOURCE_LABELS, toolRows, type ToolRow } from "./model";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";
import { loadExecutionSettings, loadImages, setDockerImage, setToolPath, useSettings } from "./store";

const TOOL_STATUS: Record<ToolRow["status"], { status: Status; label: string }> = {
  available: { status: "succeeded", label: "Available" },
  missing: { status: "unavailable", label: "Missing" },
  broken: { status: "failed", label: "Not working" },
  checking: { status: "running", label: "Checking" },
};

function DockerGroup() {
  const toolchains = useJobs((s) => s.toolchains);
  const execution = useSettings((s) => s.execution);
  const images = useSettings((s) => s.images);
  const imagesError = useSettings((s) => s.imagesError);
  const imagesLoading = useSettings((s) => s.imagesLoading);
  const saving = useSettings((s) => s.savingImage);
  useEffect(() => {
    void loadImages();
  }, []);
  if (!execution) return null;
  const docker = dockerAvailability(toolchains);
  const configured = execution.settings.docker.image;
  const image = imageState(configured, images);

  return (
    <SettingsGroup
      title="Restricted execution (Docker)"
      testId="settings-docker"
      description="Agent-generated and other untrusted code runs in a locked-down container. When Docker or the image is unavailable, restricted runs are refused; they never fall back to running on this computer."
    >
      <SettingRow label="Docker" testId="docker-status">
        <div className="exp-settings__stack">
          {docker.kind === "checking" && <StatusPill size="sm" status="running" label="Checking" />}
          {docker.kind === "available" && (
            <StatusPill size="sm" status="succeeded" label="Available" detail={docker.version ?? undefined} />
          )}
          {docker.kind === "broken" && <StatusPill size="sm" status="failed" label="Not working" />}
          {docker.kind === "missing" && <StatusPill size="sm" status="unavailable" label="Not found" />}
          {(docker.kind === "available" || docker.kind === "broken") && docker.path && (
            <code className="exp-settings__path">{docker.path}</code>
          )}
          {docker.kind === "broken" && <span className="exp-settings__muted">{docker.detail}</span>}
          {docker.kind === "missing" && <span className="exp-settings__muted">{docker.guidance}</span>}
        </div>
      </SettingRow>
      <SettingRow
        label="Container image"
        description="Only images already on this computer are listed. Exposition never pulls images; run docker pull yourself to add one."
        testId="docker-image"
      >
        {({ labelId, descriptionId }) => (
          <div className="exp-settings__stack">
            <div className="exp-settings__inline">
              <Select
                aria-labelledby={labelId}
                aria-describedby={descriptionId}
                placeholder={imagesLoading && !images ? "Listing local images…" : "Choose a local image"}
                value={configured}
                options={imageOptions(images, configured)}
                isDisabled={saving || (!images && !configured)}
                onChange={(next) => {
                  if (next !== configured) void setDockerImage(next);
                }}
                className="exp-settings__select"
                data-testid="docker-image-select"
              />
              <Button
                size="sm"
                variant="ghost"
                icon={X}
                isDisabled={!configured || saving}
                onPress={() => void setDockerImage(null)}
              >
                Clear
              </Button>
              <Button size="sm" variant="ghost" icon={RefreshCw} isPending={imagesLoading} onPress={() => void loadImages()}>
                Reload images
              </Button>
            </div>
            {image.kind === "notConfigured" && (
              <span className="exp-settings__muted" data-testid="docker-image-state">
                No image configured: restricted runs are refused.
              </span>
            )}
            {image.kind === "present" && (
              <span className="exp-settings__muted" data-testid="docker-image-state">
                {image.image} is present locally ({image.local.id}, {image.local.size}).
              </span>
            )}
            {image.kind === "unverified" && (
              <span className="exp-settings__muted" data-testid="docker-image-state">
                Configured: {image.image}
              </span>
            )}
          </div>
        )}
      </SettingRow>
      {imagesError && (
        <Banner tone="warning" title="Could not list local images">
          {imagesError}
        </Banner>
      )}
      {image.kind === "missing" && (
        <Banner tone="warning" title={`${image.image} is not present locally`}>
          Restricted runs are refused until the image is available. Choose a local image, or pull it with Docker yourself.
        </Banner>
      )}
      <div className="exp-settings__enforce" data-testid="restricted-guarantees">
        <h4 className="exp-settings__subhead">Every restricted run</h4>
        <ul>
          {restrictedGuarantees(execution.settings.docker).map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      </div>
    </SettingsGroup>
  );
}

function ToolPathDialog({ tool, onClose }: { tool: ToolRow; onClose: () => void }) {
  const [path, setPath] = useState(tool.override ?? tool.path ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function save() {
    setBusy(true);
    const refusal = await setToolPath(tool.id, path);
    setBusy(false);
    if (refusal) setError(refusal);
    else onClose();
  }

  return (
    <Dialog
      title={`Location of ${tool.name}`}
      isOpen
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      footer={
        <>
          <Button variant="secondary" slot="close">
            Cancel
          </Button>
          <Button variant="primary" isPending={busy} isDisabled={!path.trim()} onPress={() => void save()} data-testid="tool-path-save">
            Use this executable
          </Button>
        </>
      }
    >
      <form
        className="exp-settings__dialog"
        onSubmit={(e) => {
          e.preventDefault();
          if (path.trim()) void save();
        }}
      >
        <TextField
          label="Executable path"
          code
          autoFocus
          value={path}
          onChange={(value) => {
            setPath(value);
            setError(null);
          }}
          isInvalid={error !== null}
          errorMessage={error ?? undefined}
          description="The full path of the program itself (an .exe on Windows). Batch files and scripts are refused: Exposition runs programs directly, never through a shell."
          data-testid="tool-path-input"
        />
        {tool.path && tool.source !== "override" && (
          <p className="exp-settings__muted">
            Currently found: <code className="exp-settings__path">{tool.path}</code>
          </p>
        )}
      </form>
    </Dialog>
  );
}

function ToolsGroup() {
  const toolchains = useJobs((s) => s.toolchains);
  const loading = useJobs((s) => s.toolchainsLoading);
  const error = useJobs((s) => s.toolchainsError);
  const execution = useSettings((s) => s.execution);
  const [editing, setEditing] = useState<ToolRow | null>(null);
  const [resetting, setResetting] = useState<string | null>(null);
  const [resetError, setResetError] = useState<string | null>(null);
  const rows = toolRows(execution, toolchains);

  async function reset(tool: ToolRow) {
    setResetting(tool.id);
    setResetError(await setToolPath(tool.id, null));
    setResetting(null);
  }

  return (
    <SettingsGroup
      title="Tool locations"
      testId="tool-locations"
      description="Exposition finds tools on this computer and never installs them. Set a location to use a specific installation instead."
      actions={
        <Button size="sm" variant="ghost" icon={RefreshCw} isPending={loading} onPress={() => void loadToolchains(true)}>
          {loading ? "Checking…" : "Re-check tools"}
        </Button>
      }
    >
      {error && (
        <Banner tone="danger" title="Could not check tools">
          {error}
        </Banner>
      )}
      {resetError && (
        <Banner tone="danger" title="Could not reset the location">
          {resetError}
        </Banner>
      )}
      <table className="exp-settings__table" aria-busy={loading}>
        <thead>
          <tr>
            <th scope="col">Tool</th>
            <th scope="col">Status</th>
            <th scope="col">Location</th>
            <th scope="col">
              <span className="exp-settings__sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((tool) => (
            <tr key={tool.id} data-tool-id={tool.id} data-status={tool.status} data-source={tool.source ?? undefined}>
              <th scope="row">
                {tool.name}
                {tool.version && <div className="exp-settings__muted" data-testid="tool-version">{tool.version}</div>}
              </th>
              <td>
                <StatusPill size="sm" status={TOOL_STATUS[tool.status].status} label={TOOL_STATUS[tool.status].label} />
              </td>
              <td>
                {tool.path && (
                  <code className="exp-settings__path" data-testid="tool-path">
                    {tool.path}
                  </code>
                )}
                {tool.override && tool.override !== tool.path && (
                  <div>
                    Configured: <code className="exp-settings__path">{tool.override}</code>
                  </div>
                )}
                <div className="exp-settings__muted">
                  {tool.source ? SOURCE_LABELS[tool.source] : tool.override ? "Set in settings" : null}
                  {tool.detail && <span> · {tool.detail}</span>}
                </div>
                {tool.status === "missing" && tool.guidance && <div className="exp-settings__muted">{tool.guidance}</div>}
              </td>
              <td className="exp-settings__actions">
                <Button
                  size="sm"
                  icon={FolderSearch}
                  aria-label={`Set location of ${tool.name}`}
                  onPress={() => setEditing(tool)}
                >
                  {tool.override ? "Change…" : "Set…"}
                </Button>
                {tool.override && (
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={RotateCcw}
                    aria-label={`Use the discovered ${tool.name}`}
                    isPending={resetting === tool.id}
                    onPress={() => void reset(tool)}
                  >
                    Reset
                  </Button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {editing && <ToolPathDialog tool={editing} onClose={() => setEditing(null)} />}
    </SettingsGroup>
  );
}

export function ExecutionSection() {
  const execution = useSettings((s) => s.execution);
  const error = useSettings((s) => s.executionError);
  useEffect(() => {
    void loadExecutionSettings();
    if (useJobs.getState().toolchains === null) void loadToolchains(false);
  }, []);

  return (
    <SettingsPage title="Execution" testId="settings-execution">
      {execution === null ? (
        error ? (
          <ErrorState
            title="Could not read execution settings"
            details={error}
            actions={<Button onPress={() => void loadExecutionSettings()}>Retry</Button>}
          />
        ) : (
          <LoadingState title="Loading execution settings…" />
        )
      ) : (
        <>
          <DockerGroup />
          <ToolsGroup />
        </>
      )}
    </SettingsPage>
  );
}
