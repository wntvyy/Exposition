// The ONE combined decision of a project operation (spec §1): shown only when
// the backend plan has an impact that needs it (`needsDecision`), or when the
// person asked to review an explicitly scoped action. It names the affected
// resources and the lost recovery, offers the plan's choices (defaults are
// never destructive), keeps per-target details on demand, and focuses Cancel.
// It reuses the shared alert dialog host (`confirm`).
import { useState } from "react";

import type { OpPlan } from "../../bindings/OpPlan";
import { Banner, confirm, Section, Select } from "../../ui/components";
import {
  consumerText,
  decisionAction,
  decisionGroups,
  decisionTitle,
  initialChoices,
  isDelete,
  recoveryText,
  targetDetail,
  targetName,
  type TargetLabels,
} from "./model";

export interface DecisionOptions {
  labels?: TargetLabels;
  /** Choice values chosen before planning (explicit scope, e.g. `outputs: delete`). */
  presetChoices?: Record<string, string>;
  /** Shown above the consequences (e.g. "The situation changed since you decided"). */
  note?: string;
}

/** Plain names of explicit scopes a request may preset. */
const SCOPE_NAMES: Record<string, string> = { outputs: "Deleting the outputs" };

function DecisionBody({
  plan,
  labels,
  values,
  note,
  unoffered,
}: {
  plan: OpPlan;
  labels: TargetLabels;
  values: Record<string, string>;
  note?: string;
  unoffered: string[];
}) {
  const [choices, setChoices] = useState(values);
  const groups = decisionGroups(plan);
  const multiple = plan.request.targets.length > 1;
  const choose = (key: string, value: string) => {
    values[key] = value;
    setChoices({ ...values });
  };
  return (
    <div className="xp-ops-decision" data-testid="op-decision-body">
      {note && (
        <Banner tone="warning" title={note}>
          Review the consequences again before continuing.
        </Banner>
      )}
      {unoffered.length > 0 && (
        <Banner tone="warning" title={`${unoffered.map((k) => SCOPE_NAMES[k] ?? k).join(", ")} is not offered for ${plan.request.targets.length === 1 ? "this item" : "these items"}`}>
          Only what is listed below happens.
        </Banner>
      )}
      {groups.length > 0 ? (
        <p className="xp-ops-decision__lead">
          {isDelete(plan.request.kind) ? "Deleting" : "This operation"} {groups.length === 1 && groups[0]!.impacts.length === 1 ? "has a consequence" : "has consequences"} in this project:
        </p>
      ) : (
        <p className="xp-ops-decision__lead">Review what will happen before continuing.</p>
      )}
      {groups.map((group) => (
        <section key={group.kind} className="xp-ops-decision__group" data-impact={group.kind} aria-label={group.heading}>
          <h4 className="xp-ops-decision__heading">{group.heading}</h4>
          <ul className="xp-ops-decision__list">
            {group.impacts.map((impact, index) => (
              <li key={index} className="xp-ops-decision__impact" data-testid="op-impact" data-kind={impact.kind}>
                {multiple && <span className="xp-ops-decision__target">{targetName(impact.target, labels)}: </span>}
                <span>{impact.message}</span>
                {impact.consumers.length > 0 && (
                  <ul className="xp-ops-decision__consumers" aria-label="Affected">
                    {impact.consumers.map((c) => (
                      <li key={`${c.kind}:${c.id}`} data-consumer={c.kind}>
                        {consumerText(c)}
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ul>
        </section>
      ))}
      {plan.choices.length > 0 && (
        <div className="xp-ops-decision__choices">
          {plan.choices.map((choice) => (
            <div key={choice.key} data-choice={choice.key} data-value={choices[choice.key] ?? choice.default}>
              <Select<string>
                label={choice.label}
                options={choice.options.map((o) => ({ id: o.value, label: o.label }))}
                value={choices[choice.key] ?? choice.default}
                onChange={(value) => choose(choice.key, value)}
              />
            </div>
          ))}
        </div>
      )}
      {plan.refusals.length > 0 && (
        <Banner tone="warning" title={`${plan.refusals.length} of ${plan.request.targets.length} will not be changed`}>
          <ul className="xp-ops-decision__list" data-testid="op-refusals">
            {plan.refusals.map((r, i) => (
              <li key={i} data-code={r.code}>
                {targetDetail(r.target, labels)}: {r.message}
              </li>
            ))}
          </ul>
        </Banner>
      )}
      {plan.effects.length > 0 && (
        <Section title={`Details (${plan.effects.length})`} collapsible defaultExpanded={false} className="xp-ops-decision__details">
          <ul className="xp-ops-decision__list" data-testid="op-effects">
            {plan.effects.map((effect, i) => (
              <li key={i}>
                <span className="xp-ops-decision__target">{targetDetail(effect.target, labels)}</span> — {effect.description}
                {isDelete(plan.request.kind) && !/recycle bin|trash|restor|permanent/i.test(effect.description) && (
                  <span className="xp-ops-decision__recovery"> ({recoveryText(effect.recovery)})</span>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}
    </div>
  );
}

/**
 * Ask for the plan's combined decision. Resolves with the chosen values of the
 * plan's choices, or `null` when the person cancels (nothing changes).
 */
export async function requestDecision(plan: OpPlan, options: DecisionOptions = {}): Promise<Record<string, string> | null> {
  const labels = options.labels ?? new Map<string, string>();
  const values = initialChoices(plan.choices, options.presetChoices);
  // A preset scope the plan does not offer is said plainly, never implied.
  const unoffered = Object.keys(options.presetChoices ?? {}).filter((key) => !plan.choices.some((c) => c.key === key));
  const destructive = isDelete(plan.request.kind);
  const answer = await confirm<"cancel" | "proceed">({
    title: decisionTitle(plan, labels),
    message: <DecisionBody plan={plan} labels={labels} values={values} note={options.note} unoffered={unoffered} />,
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "proceed", label: decisionAction(plan), variant: destructive ? "danger" : "primary" },
    ],
    cancelId: "cancel",
    defaultId: "cancel",
    tone: plan.request.kind.kind === "delete" && plan.request.kind.permanent ? "danger" : "warning",
    size: "md",
    testId: "op-decision",
  });
  return answer === "proceed" ? { ...values } : null;
}
