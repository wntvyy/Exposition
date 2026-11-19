import type { ReactNode } from "react";
import { Radio, RadioGroup } from "react-aria-components";

import { cx } from "./cx";

export interface ChoiceCardOption<K extends string> {
  id: K;
  label: string;
  /** A small decorative picture of the choice (hidden from assistive technology). */
  preview: ReactNode;
}

export interface ChoiceCardsProps<K extends string> {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
  options: readonly ChoiceCardOption<K>[];
  value: K;
  onChange: (id: K) => void;
  className?: string;
}

/**
 * Single choice shown as small pictures with a name under each (the color
 * theme). Radio semantics: Tab enters the group, arrows move and choose. The
 * name is the accessible label; the picture is decoration. Selection is an
 * accent outline plus a semibold name, focus the focus ring.
 */
export function ChoiceCards<K extends string>({ options, value, onChange, className, ...aria }: ChoiceCardsProps<K>) {
  return (
    <RadioGroup
      {...aria}
      className={cx("ui-choice-cards", className)}
      orientation="horizontal"
      value={value}
      onChange={(next) => onChange(next as K)}
    >
      {options.map((option) => (
        <Radio key={option.id} value={option.id} className="ui-choice-card" aria-label={option.label}>
          <span className="ui-choice-card__preview" aria-hidden>
            {option.preview}
          </span>
          <span className="ui-choice-card__label" aria-hidden>
            {option.label}
          </span>
        </Radio>
      ))}
    </RadioGroup>
  );
}
