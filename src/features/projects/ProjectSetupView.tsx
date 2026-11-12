// Project setup recovery (editor tab `project-setup:<operation>`): what an
// interrupted creation created and what is missing, with explicit choices.
// Nothing is replayed automatically and the creation is never shown as complete.
import { CircleCheck, Undo2 } from "lucide-react";
import { useEffect } from "react";

import type { CreationFileState } from "../../bindings/CreationFileState";
import type { EditorProps } from "../../app/registry";
import { Banner, Button, EmptyState, StatusPill, type Status } from "../../ui/components";
import { SETUP_KEY_PREFIX } from "./model";
import { checkSetups, resolveSetup, useProjects } from "./store";

const FILE_STATUS: Record<CreationFileState, { status: Status; label: string }> = {
  created: { status: "succeeded", label: "Created" },
  missing: { status: "unavailable", label: "Missing" },
  changed: { status: "warning", label: "Different content" },
};

export function ProjectSetupView({ resource }: EditorProps) {
  const operation = resource.key.slice(SETUP_KEY_PREFIX.length);
  const setup = useProjects((s) => s.setups.find((x) => x.operation === operation));
  const resolving = useProjects((s) => s.resolving);
  const error = useProjects((s) => s.error);

  useEffect(() => {
    void checkSetups();
  }, []);

  if (!setup) {
    return (
      <div className="xp-proj">
        <EmptyState title="Nothing to finish" description="This project setup needs no further action, or its project is not open." />
      </div>
    );
  }
  const message = setup.state.kind === "needsReconciliation" ? setup.state.message : null;
  const canFinish = setup.actions.includes("finish");
  return (
    <div className="xp-proj" data-testid="project-setup" data-operation={setup.operation}>
      <div className="xp-proj__page">
        <header className="xp-proj__header">
          <h2 className="xp-proj__title">Project setup did not finish</h2>
          <p className="xp-proj__muted">
            {setup.title} · {setup.root}
          </p>
          <StatusPill status="warning" label="Needs your decision" detail={`${setup.created} created · ${setup.missing} missing`} />
        </header>
        {message && (
          <Banner tone="warning" title="What happened">
            {message}
          </Banner>
        )}
        <section className="xp-proj__group" aria-label="Files">
          <h3 className="xp-proj__group-title">Files</h3>
          <ul className="xp-proj__files" data-testid="project-setup-files">
            {setup.files.map((f) => (
              <li key={f.path} data-state={f.state} data-path={f.path}>
                <code>{f.path}</code>
                <StatusPill status={FILE_STATUS[f.state].status} label={FILE_STATUS[f.state].label} />
              </li>
            ))}
          </ul>
          <p className="xp-proj__muted" data-testid="project-setup-graph" data-applied={setup.graphApplied}>
            {setup.graphApplied
              ? "The project structure was recorded."
              : "The project structure (resources, entities, relations) was not recorded yet."}{" "}
            Workflow drafts saved: {setup.workflowsSaved} of {setup.workflowsTotal}.
          </p>
        </section>
        {error && (
          <Banner tone="danger" title="Could not continue" live>
            {error}
          </Banner>
        )}
        <section className="xp-proj__group" aria-label="Choices">
          <h3 className="xp-proj__group-title">Continue</h3>
          <p className="xp-proj__muted">
            Finishing creates only the missing files (files that exist, including ones with different content, are left exactly as they are) and records the project structure. Keeping it stops here: the existing files stay and become project resources.
          </p>
          <div className="xp-proj__footer">
            {canFinish && (
              <Button variant="primary" icon={CircleCheck} isPending={resolving} isDisabled={resolving} onPress={() => void resolveSetup(setup.operation, "finish")} data-testid="project-setup-finish">
                Finish Setup
              </Button>
            )}
            <Button icon={Undo2} isDisabled={resolving} onPress={() => void resolveSetup(setup.operation, "keep")} data-testid="project-setup-keep">
              Keep as Is
            </Button>
          </div>
        </section>
      </div>
    </div>
  );
}
