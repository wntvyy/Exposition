import type { ReactNode } from "react";
import { Label, Radio, RadioGroup, Text } from "react-aria-components";

import { Badge } from "./Badge";
import { cx } from "./cx";

export interface OptionListOption<K extends string> {
  id: K;
  label: string;
  /** The consequence of choosing this option (muted line under the label). */
  description?: ReactNode;
  /** Shows a dashed accent "Recommended" badge. */
  recommended?: boolean;
  /** Another fact badge after the label (a `Badge`). */
  badge?: ReactNode;
}

export interface OptionListProps<K extends string> {
  /** Visible label. Use `aria-label` instead only when the context labels the list visually. */
  label?: string;
  "aria-label"?: string;
  options: readonly OptionListOption<K>[];
  /** Chosen option; nothing is preselected when this is null or undefined. */
  value?: K | null;
  onChange: (id: K) => void;
  isDisabled?: boolean;
  /** Muted line under the label. */
  description?: string;
  className?: string;
}

/**
 * Single choice among options with consequences (a structured question, a
 * decision). Radio semantics: Tab enters the group, arrows move and choose.
 * Rows are separated by dividers; no cards.
 */
export function OptionList<K extends string>({
  label,
  "aria-label": ariaLabel,
  options,
  value,
  onChange,
  isDisabled,
  description,
  className,
}: OptionListProps<K>) {
  return (
    <RadioGroup
      className={cx("ui-option-list", className)}
      aria-label={label ? undefined : ariaLabel}
      value={value ?? null}
      onChange={(next) => onChange(next as K)}
      isDisabled={isDisabled}
    >
      {label && <Label className="ui-field__label">{label}</Label>}
      {description && (
        <Text slot="description" className="ui-field__description">
          {description}
        </Text>
      )}
      <div className="ui-option-list__options">
        {options.map((option) => (
          <Radio key={option.id} value={option.id} className="ui-list-row ui-option-list__option">
            <span className="ui-radio-mark" aria-hidden />
            <span className="ui-list-row__main">
              <span className="ui-option-list__title">
                <span className="ui-list-row__title">{option.label}</span>
                {option.recommended && (
                  <Badge tone="accent" dashed>
                    Recommended
                  </Badge>
                )}
                {option.badge}
              </span>
              {option.description !== undefined && <span className="ui-list-row__meta">{option.description}</span>}
            </span>
          </Radio>
        ))}
      </div>
    </RadioGroup>
  );
}
