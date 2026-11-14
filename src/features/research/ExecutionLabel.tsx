// Accurate execution labels for research sessions.
//
// Interactive Python/R/Julia kernels run natively with the user's account
// privileges. The research space and per-owner sessions organize work; they
// do not contain it. Restricted research execution is the workflow Python
// batch step: a container that sees only the step's declared inputs (no
// network, read-only inputs, a dedicated output area). Its availability is
// what the workflow runner reports for this project; nothing ever falls back
// to native execution or moves between the two silently.
import { Ban, ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";

import { Badge, Banner, Section } from "../../ui/components";
import { workflowCommands } from "../workflows/ipc";

export const TRUSTED_NATIVE_LABEL = "Trusted native — runs with your account's privileges";
export const RESTRICTED_SCOPE =
  "Interactive research sessions run natively. Restricted research execution is available only for workflow Python batch steps (a container that sees only declared inputs).";

/** Compact label for session toolbars (the full sentence is its accessible name). */
export function ExecutionLabel() {
  const full = `${TRUSTED_NATIVE_LABEL}. The research folder and session separate your work but are not a sandbox. ${RESTRICTED_SCOPE}`;
  return (
    <span
      className="exp-research__mode"
      data-testid="research-execution-mode"
      data-mode="trustedNative"
      title={full}
      aria-label={full}
      role="note"
    >
      <Badge tone="warning" icon={ShieldAlert}>
        Trusted native — your account&rsquo;s privileges
      </Badge>
    </span>
  );
}

/** Availability of the restricted research adapter, as the workflow runner reports it. */
function useRestrictedAvailability(): { available: boolean; reason: string } | null {
  const [state, setState] = useState<{ available: boolean; reason: string } | null>(null);
  useEffect(() => {
    let live = true;
    workflowCommands
      .operations()
      .then((ops) => {
        const runner = ops.find((o) => o.id === "research.pythonBatch")?.runner;
        if (live && runner) setState({ available: runner.available, reason: runner.availability });
      })
      .catch(() => {
        if (live) setState({ available: false, reason: "The workflow runner is not available." });
      });
    return () => {
      live = false;
    };
  }, []);
  return state;
}

/** Fuller explanation for the Research page: one section, each policy with its label. */
export function ExecutionModes() {
  const restricted = useRestrictedAvailability();
  const available = restricted?.available ?? false;
  return (
    <Section title="Execution" className="exp-research__modes-section">
      <div className="exp-research__modes" data-testid="research-execution-modes">
        <div className="exp-research__mode-row" data-mode="trustedNative">
          <Badge tone="warning" icon={ShieldAlert}>
            Trusted native
          </Badge>
          <span className="ui-meta">
            Python, R, and Julia kernels run with your account&rsquo;s privileges. The research space and session keep work
            organized; they do not contain it.
          </span>
        </div>
        <div className="exp-research__mode-row" data-mode="restricted" data-available={String(available)}>
          <Badge tone={available ? "success" : "neutral"} icon={available ? ShieldCheck : Ban}>
            Restricted research
          </Badge>
          <span className="ui-meta">
            Workflow Python batch steps run in a container that sees only their declared inputs, with no network.{" "}
            {restricted === null ? "Checking…" : available ? "Available." : restricted.reason || "Not available."} Runs
            are never moved to or from a sandbox silently.
          </span>
        </div>
      </div>
    </Section>
  );
}

export function TrustedNativeBanner() {
  return (
    <Banner tone="warning" title={TRUSTED_NATIVE_LABEL}>
      The session and research folder organize work; they are not a sandbox. {RESTRICTED_SCOPE}
    </Banner>
  );
}
