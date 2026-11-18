// TEMPORARY stand-in for the settings lane's ModelMenu (same props), wrapping
// the existing picker so the composer compiles. The coordinator takes the
// settings lane's file at merge.
import type { Effort } from "../../bindings/Effort";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import { ModelChoice } from "./ModelPicker";

export interface ModelMenuProps {
  value: ProviderChoice | null;
  onChange: (c: ProviderChoice | null) => void;
  status: ProviderStatus | null;
  running?: { provider: ProviderKind; model: string; effort: Effort | null } | null;
  onManageProviders: () => void;
  autoFocus?: boolean;
}

export function ModelMenu({ value, onChange, status, running, onManageProviders }: ModelMenuProps) {
  const inherited: ProviderChoice = running
    ? { provider: running.provider, model: running.model, effort: running.effort ?? "medium" }
    : status
      ? { provider: status.settings.provider, model: status.settings.model, effort: status.settings.effort }
      : { provider: "anthropic", model: "", effort: "medium" };
  return (
    <ModelChoice
      value={value}
      inherited={inherited}
      inheritedLabel={running ? null : "Default"}
      chooseLabel={running ? "Change model…" : "Choose…"}
      resetLabel={running ? "Keep current model" : "Use default"}
      onChange={onChange}
      status={status}
      onConfigure={onManageProviders}
      testId="agents-model-choice"
    />
  );
}
