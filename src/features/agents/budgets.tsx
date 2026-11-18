// Budgets in the composer's Options sheet: the editable limits of a new task
// (defaults and minimums as before; the input-token ceiling is fixed), and
// for a started task its recorded usage against the limits it runs with.
// A follow-up sends no budgets: it keeps the task's limits and its usage.
import { RotateCcw } from "lucide-react";

import type { Budgets } from "../../bindings/Budgets";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { Banner, Button, KeyValueList, ProgressBar, Section, StatusText, TextField } from "../../ui/components";
import { asSentence, budgetRows, statusLabel } from "./format";

export const DEFAULT_BUDGETS: Budgets = {
  maxTurns: 40,
  maxOutputTokens: 400_000,
  maxInputTokens: 4_000_000,
  maxWallMs: 30 * 60 * 1000,
  maxToolCalls: 150,
  maxCommands: 20,
};

/** The sentence every started task's budgets carry (usage counts across runs). */
export const FOLLOW_UP_BUDGETS = "A follow-up keeps the same budgets and the usage so far.";

/** Whether limits differ from the defaults ("· custom budgets"). */
export function budgetsCustom(b: Budgets): boolean {
  return (Object.keys(DEFAULT_BUDGETS) as (keyof Budgets)[]).some((k) => b[k] !== DEFAULT_BUDGETS[k]);
}

const count = (n: number) => n.toLocaleString("en-US");

function NumberField(props: { label: string; value: number; min: number; defaultValue: number; onChange: (v: number) => void }) {
  return (
    <TextField
      label={props.label}
      type="number"
      inputMode="numeric"
      value={String(props.value)}
      description={`Default ${count(props.defaultValue)}`}
      onChange={(raw) => props.onChange(Math.max(props.min, Number(raw) || props.min))}
    />
  );
}

/** A new task's limits. */
export function BudgetFields({ value, onChange }: { value: Budgets; onChange: (next: Budgets) => void }) {
  const d = DEFAULT_BUDGETS;
  return (
    <Section
      title="This task"
      actions={
        budgetsCustom(value) ? (
          <Button size="sm" variant="ghost" icon={RotateCcw} onPress={() => onChange(DEFAULT_BUDGETS)} data-testid="agents-budgets-reset">
            Reset to defaults
          </Button>
        ) : undefined
      }
      data-testid="agents-budgets"
    >
      <div className="agents-grid">
        <NumberField label="Model turns" min={1} defaultValue={d.maxTurns} value={value.maxTurns} onChange={(v) => onChange({ ...value, maxTurns: v })} />
        <NumberField label="Tool calls" min={0} defaultValue={d.maxToolCalls} value={value.maxToolCalls} onChange={(v) => onChange({ ...value, maxToolCalls: v })} />
        <NumberField label="Commands" min={0} defaultValue={d.maxCommands} value={value.maxCommands} onChange={(v) => onChange({ ...value, maxCommands: v })} />
        <NumberField
          label="Output tokens"
          min={1024}
          defaultValue={d.maxOutputTokens}
          value={value.maxOutputTokens}
          onChange={(v) => onChange({ ...value, maxOutputTokens: v })}
        />
        <NumberField
          label="Minutes"
          min={1}
          defaultValue={d.maxWallMs / 60000}
          value={Math.round(value.maxWallMs / 60000)}
          onChange={(v) => onChange({ ...value, maxWallMs: v * 60000 })}
        />
      </div>
      <KeyValueList items={[{ key: "input", label: "Input tokens", value: `${count(value.maxInputTokens)} (fixed)` }]} />
      <p className="ui-meta">Time excludes waiting for your approval. {FOLLOW_UP_BUDGETS}</p>
    </Section>
  );
}

/** A started task's usage against its limits, and why it stopped when a budget ran out. */
export function BudgetUsage({ task }: { task: TaskSummary }) {
  const rows = budgetRows(task);
  return (
    <Section title="This task" data-testid="agents-budget-meter">
      {task.status.kind === "budgetExceeded" && (
        <Banner tone="warning" title={statusLabel(task.status).label}>
          {asSentence(task.status.detail)}
        </Banner>
      )}
      <div className="agents-stack agents-stack--tight">
        {rows.map((row) => {
          const reached = row.limit > 0 && row.used >= row.limit;
          const near = !reached && row.limit > 0 && row.used >= row.limit * 0.9;
          return (
            <div key={row.key} className="agents-stack agents-stack--tight">
              <ProgressBar
                label={row.label}
                value={row.limit > 0 ? Math.min(100, (row.used / row.limit) * 100) : 0}
                valueLabel={row.text}
                tone={reached ? "danger" : "accent"}
              />
              {near && <StatusText status="warning" label="Near limit" />}
            </div>
          );
        })}
      </div>
      <p className="ui-meta">{FOLLOW_UP_BUDGETS}</p>
    </Section>
  );
}
