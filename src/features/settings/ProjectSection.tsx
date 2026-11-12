// Project: startup behavior (application settings) and the open folder's
// LaTeX targets (`.exposition/project.json`): which .tex file builds with
// pdfLaTeX or LuaLaTeX, and the default target. Every change is validated by
// the workspace service; open page views of a changed entry rebuild.
import { useEffect, useState } from "react";
import { FolderOpen, Pencil, Plus, Trash2 } from "lucide-react";

import { editors, useActiveEditor } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { ProjectSettings } from "../../bindings/ProjectSettings";
import { errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  Dialog,
  EmptyState,
  ErrorState,
  LoadingState,
  Select,
  Switch,
  TextField,
} from "../../ui/components";
import { loadAppSettings, updateAppSettings, useAppSettings } from "../editor/settings";
import { useWorkspace } from "../workspace/store";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";
import {
  engineLabel,
  isTexPath,
  LATEX_ENGINES,
  latexTargets,
  removeTarget,
  setDefaultTarget,
  uniqueName,
  upsertTarget,
  validateDraft,
  type LatexEngine,
  type TargetDraft,
} from "./project";
import { loadProjectSettings, saveProjectSettings, useSettings } from "./store";

function StartupGroup() {
  const loaded = useAppSettings((s) => s.loaded);
  const settings = useAppSettings((s) => s.settings);
  useEffect(() => {
    void loadAppSettings();
  }, []);
  return (
    <SettingsGroup title="Startup">
      <SettingRow
        label="Reopen the last project"
        description="When Exposition starts, open the most recent folder again. Opening a folder never runs anything in it."
        testId="reopen-last-project"
      >
        {({ labelId, descriptionId }) => (
          <Switch
            aria-labelledby={labelId}
            aria-describedby={descriptionId}
            isDisabled={!loaded}
            isSelected={settings.files.reopenLastProject}
            onChange={(reopenLastProject) => {
              updateAppSettings({ ...settings, files: { ...settings.files, reopenLastProject } }).catch((e: unknown) =>
                notify({ tone: "danger", title: "Could not save the startup setting", body: errorMessage(e) }),
              );
            }}
          />
        )}
      </SettingRow>
    </SettingsGroup>
  );
}

function TargetDialog({
  settings,
  editing,
  initial,
  onClose,
}: {
  settings: ProjectSettings;
  /** Name of the target being edited; null adds one. */
  editing: string | null;
  initial: TargetDraft;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<TargetDraft>(initial);
  const [error, setError] = useState<string | null>(null);
  const saving = useSettings((s) => s.savingProject);

  async function save() {
    const invalid = validateDraft(draft, settings.targets, editing);
    if (invalid) {
      setError(invalid);
      return;
    }
    const refusal = await saveProjectSettings(upsertTarget(settings, editing, draft));
    if (refusal) setError(refusal);
    else onClose();
  }

  const change = (patch: Partial<TargetDraft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setError(null);
  };

  return (
    <Dialog
      title={editing === null ? "Add LaTeX target" : `Edit target ${editing}`}
      isOpen
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      footer={
        <>
          <Button variant="secondary" slot="close">
            Cancel
          </Button>
          <Button variant="primary" isPending={saving} onPress={() => void save()} data-testid="target-save">
            {editing === null ? "Add target" : "Save target"}
          </Button>
        </>
      }
    >
      <form
        className="exp-settings__dialog"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
        onKeyDown={(e) => {
          // Enter in a text field saves (the form has more than one field).
          if (e.key === "Enter" && e.target instanceof HTMLInputElement && !e.nativeEvent.isComposing) {
            e.preventDefault();
            void save();
          }
        }}
      >
        <TextField label="Name" autoFocus value={draft.name} onChange={(name) => change({ name })} />
        <TextField
          label="Main file"
          code
          value={draft.entry}
          onChange={(entry) => change({ entry })}
          description="The .tex file to build, relative to the project folder (for example paper/main.tex)."
          isInvalid={error !== null}
          errorMessage={error ?? undefined}
        />
        <Select
          label="Engine"
          value={draft.engine}
          options={LATEX_ENGINES}
          onChange={(engine) => change({ engine })}
        />
      </form>
    </Dialog>
  );
}

function TargetsGroup({ root, name }: { root: string; name: string }) {
  const project = useSettings((s) => (s.projectRoot === root ? s.project : null));
  const error = useSettings((s) => (s.projectRoot === root ? s.projectError : null));
  const saving = useSettings((s) => s.savingProject);
  const active = useActiveEditor();
  const [dialog, setDialog] = useState<{ editing: string | null; initial: TargetDraft } | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    void loadProjectSettings(root);
  }, [root]);

  if (error) {
    return (
      <SettingsGroup title="LaTeX targets">
        <ErrorState
          title="Could not read the project settings"
          details={error}
          actions={<Button onPress={() => void loadProjectSettings(root)}>Retry</Button>}
        />
      </SettingsGroup>
    );
  }
  if (!project) {
    return (
      <SettingsGroup title="LaTeX targets">
        <LoadingState title="Loading project settings…" />
      </SettingsGroup>
    );
  }

  const settings = project.settings;
  const targets = latexTargets(settings);
  const others = settings.targets.length - targets.length;

  async function save(next: ProjectSettings) {
    setSaveError(await saveProjectSettings(next));
  }

  function add() {
    // The active .tex file, else one open in another tab (Settings itself is usually active).
    const open = [active, ...editors.all()].map((e) => e?.resource.path);
    const entry = open.find(isTexPath) ?? "main.tex";
    const stem = entry.split("/").pop()?.replace(/\.tex$/i, "") ?? "main";
    setDialog({ editing: null, initial: { name: uniqueName(stem, settings.targets), entry, engine: "pdfLatex" } });
  }

  return (
    <SettingsGroup
      title="LaTeX targets"
      testId="latex-targets"
      description={
        <>
          How each .tex file of {name} is built, saved in <code className="exp-settings__path">.exposition/project.json</code>.
          A file without a target builds with pdfLaTeX.
        </>
      }
      actions={
        <Button size="sm" variant="ghost" icon={Plus} isDisabled={saving} onPress={add}>
          Add target…
        </Button>
      }
    >
      {project.warning && (
        <Banner tone="warning" title="Invalid project settings were reset">
          {project.warning}
        </Banner>
      )}
      {saveError && (
        <div data-testid="project-save-error">
          <Banner tone="danger" title="Could not save the project settings" live>
            {saveError}
          </Banner>
        </div>
      )}
      {targets.length === 0 ? (
        <EmptyState
          fill={false}
          title="No LaTeX targets"
          description="Every .tex file builds with pdfLaTeX. Add a target to build a file with LuaLaTeX or to make it the project's default."
          actions={
            <Button size="sm" icon={Plus} onPress={add}>
              Add target…
            </Button>
          }
        />
      ) : (
        <table className="exp-settings__table" aria-busy={saving}>
          <thead>
            <tr>
              <th scope="col" className="exp-settings__col-name">
                Name
              </th>
              <th scope="col">Main file</th>
              <th scope="col" className="exp-settings__col-fit">
                Engine
              </th>
              <th scope="col" className="exp-settings__col-fit">
                <span className="exp-settings__sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {targets.map((target) => (
              <tr key={target.name} data-testid="latex-target" data-name={target.name} data-engine={target.engine}>
                <th scope="row">
                  {target.name}
                  {settings.defaultTarget === target.name && <div className="exp-settings__muted">Default</div>}
                </th>
                <td>
                  <code className="exp-settings__path">{target.entry}</code>
                </td>
                <td>
                  <Select
                    aria-label={`Engine of ${target.name}`}
                    value={target.engine as LatexEngine}
                    options={LATEX_ENGINES}
                    isDisabled={saving}
                    onChange={(engine) => {
                      if (engine !== target.engine) {
                        void save({
                          ...settings,
                          targets: settings.targets.map((t) => (t === target ? { ...t, engine } : t)),
                        });
                      }
                    }}
                    className="exp-settings__select"
                  />
                </td>
                <td className="exp-settings__actions">
                  <Button
                    size="sm"
                    icon={Pencil}
                    aria-label={`Edit target ${target.name}`}
                    isDisabled={saving}
                    onPress={() =>
                      setDialog({
                        editing: target.name,
                        initial: { name: target.name, entry: target.entry, engine: target.engine as LatexEngine },
                      })
                    }
                  >
                    Edit…
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={Trash2}
                    aria-label={`Remove target ${target.name}`}
                    isDisabled={saving}
                    onPress={() => void save(removeTarget(settings, target.name))}
                  >
                    Remove
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {settings.targets.length > 0 && (
        <SettingRow
          label="Default target"
          description="Built when no file is named, and preferred for its own main file."
          testId="default-target"
        >
          {({ labelId, descriptionId }) => (
            <Select<string>
              aria-labelledby={labelId}
              aria-describedby={descriptionId}
              value={settings.defaultTarget ?? ""}
              options={[
                { id: "", label: "None" },
                ...settings.targets.map((t) => ({ id: t.name, label: t.name, description: `${t.entry} · ${engineLabel(t.engine)}` })),
              ]}
              isDisabled={saving}
              onChange={(next) => {
                if ((next || null) !== settings.defaultTarget) void save(setDefaultTarget(settings, next || null));
              }}
              className="exp-settings__select"
            />
          )}
        </SettingRow>
      )}
      {others > 0 && (
        <p className="exp-settings__muted">
          {others} other target{others === 1 ? "" : "s"} (Typst or Markdown) in this file are kept unchanged.
        </p>
      )}
      {dialog && (
        <TargetDialog
          settings={settings}
          editing={dialog.editing}
          initial={dialog.initial}
          onClose={() => setDialog(null)}
        />
      )}
    </SettingsGroup>
  );
}

export function ProjectSection() {
  const info = useWorkspace((s) => s.info);
  return (
    <SettingsPage title="Project" description="Opening projects, and how the open project is built." testId="settings-project">
      <StartupGroup />
      {info ? (
        <TargetsGroup root={info.root} name={info.name} />
      ) : (
        <SettingsGroup title="LaTeX targets">
          <EmptyState
            fill={false}
            icon={FolderOpen}
            title="No folder open"
            description="Open a project folder to choose how its LaTeX files are built."
          />
        </SettingsGroup>
      )}
    </SettingsPage>
  );
}
