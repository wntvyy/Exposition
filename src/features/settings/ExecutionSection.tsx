// Execution: the restricted (Docker) backend's availability and image (local
// images only; R and Julia workflow batches have their own image), what
// restricted runs enforce, and tool location overrides.
// Every change is validated by the execution service.
import { useEffect, useState } from "react";
import { FolderSearch, RefreshCw, RotateCcw, X } from "lucide-react";

import type { RestrictedLanguage } from "../../bindings/RestrictedLanguage";
import {
  Banner,
  Button,
  Dialog,
  ErrorState,
  LoadingState,
  Select,
  SimpleTable,
  StatusText,
  TextField,
  type SimpleTableColumn,
  type Status,
} from "../../ui/components";
import { loadToolchains, useJobs } from "../jobs/store";
import { dockerAvailability, imageOptions, imageState, restrictedGuarantees, SOURCE_LABELS, toolRows, type ImageState, type ToolRow } from "./model";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";
import { loadExecutionSettings, loadImages, setDockerImage, setLanguageImage, setToolPath, useSettings } from "./store";

const TOOL_STATUS: Record<ToolRow["status"], { status: Status; label: string }> = {
  available: { status: "succeeded", label: "Available" },
  missing: { status: "unavailable", label: "Missing" },
  broken: { status: "failed", label: "Not working" },
  checking: { status: "running", label: "Checking" },
};

/**
 * What an image row says about its image: present locally (succeeded), not
 * configured (refused, unavailable), or configured but not verified yet (a
 * settled fact, muted). A configured image that is missing gets a warning
 * banner below the row; the default image's row then says nothing more.
 */
function ImageStateText({
  image,
  notConfigured,
  testId,
  showMissing,
}: {
  image: ImageState;
  notConfigured: string;
  testId: string;
  showMissing: boolean;
}) {
  if (image.kind === "missing" && !showMissing) return null;
  return (
    <span data-testid={testId}>
      {image.kind === "notConfigured" && <StatusText status="unavailable" label={notConfigured} wrap />}
      {image.kind === "present" && (
        <StatusText status="succeeded" label={`${image.image} is present locally (${image.local.id}, ${image.local.size}).`} wrap />
      )}
      {(image.kind === "unverified" || image.kind === "missing") && <span className="ui-meta">Configured: {image.image}</span>}
    </span>
  );
}

function DockerGroup() {
  const toolchains = useJobs((s) => s.toolchains);
  const execution = useSettings((s) => s.execution);
  const images = useSettings((s) => s.images);
  const imagesError = useSettings((s) => s.imagesError);
  const imagesLoading = useSettings((s) => s.imagesLoading);
  const saving = useSettings((s) => s.savingImage);
  useEffect(() => {
    // Listed once per session; "Reload images" re-lists after `docker pull`.
    if (useSettings.getState().images === null) void loadImages();
  }, []);
  if (!execution) return null;
  const docker = dockerAvailability(toolchains);
  const configured = execution.settings.docker.image;
  const image = imageState(configured, images);

  return (
    <SettingsGroup
      title="Restricted execution (Docker)"
      testId="settings-docker"
      description="Restricted runs, such as commands of agent tasks set to restricted execution, happen in a locked-down container. When Docker or the image is unavailable, restricted runs are refused; they never fall back to running on this computer."
    >
      <SettingRow label="Docker" testId="docker-status">
        <div className="exp-settings__stack">
          {docker.kind === "checking" && <StatusText status="running" label="Checking" />}
          {docker.kind === "available" && <StatusText status="succeeded" label="Available" detail={docker.version ?? undefined} />}
          {docker.kind === "broken" && <StatusText status="failed" label="Not working" />}
          {docker.kind === "missing" && <StatusText status="unavailable" label="Not found" />}
          {(docker.kind === "available" || docker.kind === "broken") && docker.path && (
            <code className="ui-code exp-settings__path">{docker.path}</code>
          )}
          {docker.kind === "broken" && <span className="ui-meta">{docker.detail}</span>}
          {docker.kind === "missing" && <span className="ui-meta">{docker.guidance}</span>}
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
            <ImageStateText
              image={image}
              notConfigured="No image configured: restricted runs are refused."
              testId="docker-image-state"
              showMissing={false}
            />
          </div>
        )}
      </SettingRow>
      {imagesError && (
        <Banner tone="warning" title="Could not list local images" className="exp-settings__notice">
          {imagesError}
        </Banner>
      )}
      {image.kind === "missing" && (
        <Banner tone="warning" title={`${image.image} is not present locally`} className="exp-settings__notice">
          Restricted runs are refused until the image is available. Choose a local image, or pull it with Docker yourself.
        </Banner>
      )}
      <LanguageImageRow language="r" />
      <LanguageImageRow language="julia" />
      <SettingRow label="Every restricted run" testId="restricted-guarantees">
        <ul className="exp-settings__bullets">
          {restrictedGuarantees(execution.settings.docker).map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      </SettingRow>
    </SettingsGroup>
  );
}

/**
 * The image restricted R or Julia workflow batches run in (their own image,
 * chosen from local images like the default one). Without it those batches
 * are refused; they never run natively instead.
 */
function LanguageImageRow({ language }: { language: RestrictedLanguage }) {
  const execution = useSettings((s) => s.execution);
  const images = useSettings((s) => s.images);
  const imagesLoading = useSettings((s) => s.imagesLoading);
  const saving = useSettings((s) => s.savingImage);
  if (!execution) return null;
  const name = language === "r" ? "R" : "Julia";
  const configured = (language === "r" ? execution.settings.docker.rImage : execution.settings.docker.juliaImage) ?? null;
  const image = imageState(configured, images);
  const testId = `docker-${language}-image`;
  return (
    <>
      <SettingRow
        label={`${name} image`}
        description={`Restricted ${name} workflow batches run in this image (${language === "r" ? "Rscript" : "julia"} on its standard PATH), under the same restrictions.`}
        testId={testId}
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
                  if (next !== configured) void setLanguageImage(language, next);
                }}
                className="exp-settings__select"
                data-testid={`${testId}-select`}
              />
              <Button size="sm" variant="ghost" icon={X} isDisabled={!configured || saving} onPress={() => void setLanguageImage(language, null)}>
                Clear
              </Button>
            </div>
            <ImageStateText
              image={image}
              notConfigured={`No ${name} image: restricted ${name} batches are refused; they never run natively instead.`}
              testId={`${testId}-state`}
              showMissing
            />
          </div>
        )}
      </SettingRow>
      {image.kind === "missing" && (
        <Banner tone="warning" title={`${image.image} is not present locally`} className="exp-settings__notice">
          Restricted {name} batches are refused until the image is available. Choose a local image, or pull it with Docker yourself.
        </Banner>
      )}
    </>
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
          <p className="ui-meta">
            Currently found: <code className="ui-code exp-settings__path">{tool.path}</code>
          </p>
        )}
      </form>
    </Dialog>
  );
}

/** Tool locations: what was found, where, how, and the actions that change it. */
function toolColumns(
  edit: (tool: ToolRow) => void,
  resetting: string | null,
  reset: (tool: ToolRow) => void,
): SimpleTableColumn<ToolRow>[] {
  return [
    {
      id: "tool",
      title: "Tool",
      isRowHeader: true,
      render: (tool) => (
        <>
          <div>{tool.name}</div>
          {tool.version && (
            <div className="ui-meta" data-testid="tool-version">
              {tool.version}
            </div>
          )}
        </>
      ),
    },
    {
      id: "status",
      title: "Status",
      width: "1%",
      render: (tool) => <StatusText status={TOOL_STATUS[tool.status].status} label={TOOL_STATUS[tool.status].label} />,
    },
    {
      id: "location",
      title: "Location",
      render: (tool) => (
        <>
          {tool.path && (
            <code className="ui-code exp-settings__path" data-testid="tool-path">
              {tool.path}
            </code>
          )}
          {tool.override && tool.override !== tool.path && (
            <div>
              Configured: <code className="ui-code exp-settings__path">{tool.override}</code>
            </div>
          )}
          {tool.status === "missing" && tool.guidance && <div className="ui-meta">{tool.guidance}</div>}
        </>
      ),
    },
    {
      id: "source",
      title: "Source",
      render: (tool) => (
        <span className="ui-meta">
          {tool.source ? SOURCE_LABELS[tool.source] : tool.override ? "Set in settings" : null}
          {tool.detail && <span> · {tool.detail}</span>}
        </span>
      ),
    },
    {
      id: "actions",
      title: "Actions",
      hideTitle: true,
      width: "1%",
      render: (tool) => (
        <div className="exp-settings__actions">
          <Button size="sm" icon={FolderSearch} aria-label={`Set location of ${tool.name}`} onPress={() => edit(tool)}>
            {tool.override ? "Change…" : "Set…"}
          </Button>
          {tool.override && (
            <Button
              size="sm"
              variant="ghost"
              icon={RotateCcw}
              aria-label={`Use the discovered ${tool.name}`}
              isPending={resetting === tool.id}
              onPress={() => reset(tool)}
            >
              Reset
            </Button>
          )}
        </div>
      ),
    },
  ];
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
        <Banner tone="danger" title="Could not check tools" className="exp-settings__notice">
          {error}
        </Banner>
      )}
      {resetError && (
        <Banner tone="danger" title="Could not reset the location" className="exp-settings__notice">
          {resetError}
        </Banner>
      )}
      <SimpleTable
        aria-label="Tool locations"
        aria-busy={loading}
        className="exp-settings__table"
        columns={toolColumns(setEditing, resetting, (tool) => void reset(tool))}
        rows={rows}
        rowKey={(tool) => tool.id}
        rowProps={(tool) => ({ "data-tool-id": tool.id, "data-status": tool.status, "data-source": tool.source ?? undefined })}
      />
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
          {execution.warning && (
            <div className="exp-settings__notice exp-settings__inset" data-testid="execution-settings-warning">
              <Banner tone="warning" title="Invalid execution settings were reset">
                {execution.warning}
              </Banner>
            </div>
          )}
          <DockerGroup />
          <ToolsGroup />
        </>
      )}
    </SettingsPage>
  );
}
