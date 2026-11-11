import { Check, ChevronDown, Minus, Search, X } from "lucide-react";
import type { ReactNode } from "react";
import {
  Button as AriaButton,
  Checkbox as AriaCheckbox,
  FieldError,
  Input,
  Label,
  ListBox,
  ListBoxItem,
  SearchField as AriaSearchField,
  Select as AriaSelect,
  SelectValue,
  Switch as AriaSwitch,
  Text,
  TextField as AriaTextField,
  type CheckboxProps as AriaCheckboxProps,
  type Key,
  type SearchFieldProps as AriaSearchFieldProps,
  type SelectProps as AriaSelectProps,
  type SwitchProps as AriaSwitchProps,
  type TextFieldProps as AriaTextFieldProps,
} from "react-aria-components";

import { cx } from "./cx";
import { Popover } from "./Menu";

interface FieldChrome {
  /** Visible label. Use `aria-label` instead only when the context labels the field visually. */
  label?: string;
  description?: string;
  errorMessage?: string;
  className?: string;
}

export interface TextFieldProps extends Omit<AriaTextFieldProps, "className" | "children">, FieldChrome {
  placeholder?: string;
  /** Monospace input for paths, commands, identifiers. */
  code?: boolean;
}

/** Single-line text input with label, description, and validation message. IME composition safe (native input). */
export function TextField({ label, description, errorMessage, placeholder, code, className, ...props }: TextFieldProps) {
  return (
    <AriaTextField {...props} className={cx("ui-field", className)}>
      {label && <Label className="ui-field__label">{label}</Label>}
      <Input className="ui-input" placeholder={placeholder} data-code={code || undefined} />
      {description && (
        <Text slot="description" className="ui-field__description">
          {description}
        </Text>
      )}
      <FieldError className="ui-field__error">{errorMessage}</FieldError>
    </AriaTextField>
  );
}

export interface SearchFieldProps extends Omit<AriaSearchFieldProps, "className" | "children">, FieldChrome {
  placeholder?: string;
}

/** Search/filter input with a clear button; Escape clears. */
export function SearchField({ label, description, placeholder = "Search", className, ...props }: SearchFieldProps) {
  return (
    <AriaSearchField {...props} className={cx("ui-field ui-search-field", className)}>
      {label && <Label className="ui-field__label">{label}</Label>}
      <div className="ui-search-field__control">
        <Search size={14} aria-hidden className="ui-search-field__icon" />
        <Input className="ui-input" placeholder={placeholder} />
        <AriaButton className="ui-search-field__clear" aria-label="Clear search">
          <X size={14} aria-hidden />
        </AriaButton>
      </div>
      {description && (
        <Text slot="description" className="ui-field__description">
          {description}
        </Text>
      )}
    </AriaSearchField>
  );
}

export interface SelectOption<K extends Key> {
  id: K;
  label: string;
  description?: string;
}

export interface SelectProps<K extends Key>
  extends Omit<AriaSelectProps<SelectOption<K>, "single">, "className" | "children" | "items" | "value" | "onChange">, FieldChrome {
  options: SelectOption<K>[];
  value?: K | null;
  onChange?: (value: K) => void;
  placeholder?: string;
}

/** Choose one value from a short list (use a SearchField + list for long lists). */
export function Select<K extends Key>({
  label,
  description,
  errorMessage,
  options,
  value,
  onChange,
  className,
  placeholder,
  ...props
}: SelectProps<K>) {
  return (
    <AriaSelect
      {...props}
      placeholder={placeholder}
      className={cx("ui-field ui-select", className)}
      value={value ?? null}
      onChange={(key) => {
        if (key !== null) onChange?.(key as K);
      }}
    >
      {label && <Label className="ui-field__label">{label}</Label>}
      <AriaButton className="ui-select__button">
        <SelectValue className="ui-select__value" />
        <ChevronDown size={14} aria-hidden />
      </AriaButton>
      {description && (
        <Text slot="description" className="ui-field__description">
          {description}
        </Text>
      )}
      <FieldError className="ui-field__error">{errorMessage}</FieldError>
      <Popover placement="bottom start" className="ui-select__popover">
        <ListBox className="ui-listbox" items={options}>
          {(option) => (
            <ListBoxItem id={option.id} textValue={option.label} className="ui-listbox-item">
              {({ isSelected }) => (
                <>
                  <span className="ui-menu-item__check">{isSelected && <Check size={14} aria-hidden />}</span>
                  <span className="ui-menu-item__text">
                    <Text slot="label">{option.label}</Text>
                    {option.description && (
                      <Text slot="description" className="ui-menu-item__description">
                        {option.description}
                      </Text>
                    )}
                  </span>
                </>
              )}
            </ListBoxItem>
          )}
        </ListBox>
      </Popover>
    </AriaSelect>
  );
}

export interface CheckboxProps extends Omit<AriaCheckboxProps, "className" | "children"> {
  children?: ReactNode;
  className?: string;
}

export function Checkbox({ children, className, ...props }: CheckboxProps) {
  return (
    <AriaCheckbox {...props} className={cx("ui-checkbox", className)}>
      {({ isIndeterminate, isSelected }) => (
        <>
          <span className="ui-checkbox__box" aria-hidden>
            {isIndeterminate ? <Minus size={12} strokeWidth={3} /> : isSelected && <Check size={12} strokeWidth={3} />}
          </span>
          {children && <span className="ui-checkbox__label">{children}</span>}
        </>
      )}
    </AriaCheckbox>
  );
}

export interface SwitchProps extends Omit<AriaSwitchProps, "className" | "children"> {
  children?: ReactNode;
  className?: string;
}

/** Immediate on/off setting. Use Checkbox for choices applied later (forms, dialogs). */
export function Switch({ children, className, ...props }: SwitchProps) {
  return (
    <AriaSwitch {...props} className={cx("ui-switch", className)}>
      <span className="ui-switch__track" aria-hidden>
        <span className="ui-switch__thumb" />
      </span>
      {children && <span className="ui-switch__label">{children}</span>}
    </AriaSwitch>
  );
}
