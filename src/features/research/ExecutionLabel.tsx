// Accurate execution labels for research sessions.
//
// Interactive Python/R/Julia kernels run natively with the user's account
// privileges. The research space and per-owner sessions organize work; they
// do not contain it. Restricted research execution is the workflow batch
// step in Python, R, or Julia: a container (each language with its own
// configured image) that sees only the step's declared inputs (no network,
// read-only inputs, a dedicated output area). Its availability is what the
// workflow runner reports for this project, re-read whenever it may have
// changed (settings, trust, project, Refresh environments); nothing ever
// falls back to native execution or moves between the two silently.
import { Ban, ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";

import { Badge, KeyValueList, Section, StatusText } from "../../ui/components";
import { useRunnerAvailabilityEpoch } from "../workflows/availability";
import { workflowCommands } from "../workflows/ipc";

export const TRUSTED_NATIVE_LABEL = "Trusted native — runs with your account's privileges";
export const RESTRICTED_SCOPE =
  "Restricted research execution is available only for workflow batch steps in Python, R, and Julia (a container that sees only declared inputs).";

/** The restricted batch operations, one per language (each with its own image). */
const RESTRICTED_BATCHES = [
  { operation: "research.pythonBatch", language: "Python", key: "python" },
  { operation: "research.rBatch", language: "R", key: "r" },
  { operation: "research.juliaBatch", language: "Julia", key: "julia" },
] as const;

interface LanguageAvailability {
  key: (typeof RESTRICTED_BATCHES)[number]["key"];
  language: string;
  available: boolean;
  reason: string;
}

/** Compact label for session toolbars (the full sentence is its accessible name). */
export function ExecutionLabel() {
  const full = `${TRUSTED_NATIVE_LABEL}. The research folder and session organize work; they are not a sandbox. ${RESTRICTED_SCOPE}`;
  return (
    <Badge
      tone="warning"
      icon={ShieldAlert}
      data-testid="research-execution-mode"
      data-mode="trustedNative"
      title={full}
      aria-label={full}
      role="note"
    >
      Trusted native — your account&rsquo;s privileges
    </Badge>
  );
}

/** Availability of each restricted batch language, as the workflow runner reports it now. */
function useRestrictedAvailability(): LanguageAvailability[] | null {
  const epoch = useRunnerAvailabilityEpoch();
  const [state, setState] = useState<LanguageAvailability[] | null>(null);
  useEffect(() => {
    let live = true;
    workflowCommands
      .operations()
      .then((ops) => {
        if (!live) return;
        setState(
          RESTRICTED_BATCHES.map(({ operation, language, key }) => {
            const runner = ops.find((o) => o.id === operation)?.runner;
            return runner
              ? { key, language, available: runner.available, reason: runner.availability }
              : { key, language, available: false, reason: "No runner is installed for this operation in this build." };
          }),
        );
      })
      .catch(() => {
        if (live) setState(RESTRICTED_BATCHES.map(({ language, key }) => ({ key, language, available: false, reason: "The workflow runner is not available." })));
      });
    return () => {
      live = false;
    };
  }, [epoch]);
  return state;
}

/** Why a language is unavailable, without repeating the "Unavailable" its status already says. */
function unavailableReason(reason: string): string {
  const detail = reason.replace(/^Restricted execution is unavailable:\s*/, "").trim() || "Not available.";
  return detail.charAt(0).toUpperCase() + detail.slice(1);
}

/** Fuller explanation for the Research page: one section, each policy with its label. */
export function ExecutionModes() {
  const restricted = useRestrictedAvailability();
  const available = restricted?.some((l) => l.available) ?? false;
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
            Workflow batch steps in Python, R, and Julia run in a container that sees only their declared inputs, with no
            network. Runs never move between the two modes silently.
          </span>
          {restricted === null ? (
            <StatusText status="running" label="Checking…" />
          ) : (
            <KeyValueList
              items={restricted.map((l) => ({
                key: l.key,
                label: l.language,
                valueProps: { "data-testid": `research-restricted-${l.key}`, "data-available": String(l.available) },
                value: l.available ? (
                  <StatusText status="succeeded" label="Available" />
                ) : (
                  <StatusText status="unavailable" label={unavailableReason(l.reason)} wrap />
                ),
              }))}
            />
          )}
        </div>
      </div>
    </Section>
  );
}
