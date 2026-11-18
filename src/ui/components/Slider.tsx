import { useId, useLayoutEffect, useRef, type RefObject } from "react";
import { Label, Slider as AriaSlider, SliderOutput, SliderThumb, SliderTrack } from "react-aria-components";

import { cx } from "./cx";

export interface SliderStop<K extends string> {
  id: K;
  /** Visible name of the stop; also the slider's `aria-valuetext` when current. */
  label: string;
}

export interface SliderProps<K extends string> {
  /** Visible label. Use `aria-label` instead only when the context labels the slider visually. */
  label?: string;
  "aria-label"?: string;
  /** The stops, in order. One stop renders a fixed read-only value; none renders nothing. */
  values: readonly SliderStop<K>[];
  /** Current stop; an unknown or missing id shows the first stop. */
  value: K | null | undefined;
  onChange: (id: K) => void;
  isDisabled?: boolean;
  /** Muted line under the slider. */
  description?: string;
  className?: string;
}

/**
 * Discrete slider over named stops (effort levels, detail levels). The stops
 * are labelled under the track and the current one is named next to the
 * label; dragging snaps to a stop; arrows, Home, and End move between stops;
 * `aria-valuetext` is the stop's name. No percentages, no values in between.
 */
export function Slider<K extends string>({
  label,
  "aria-label": ariaLabel,
  values,
  value,
  onChange,
  isDisabled,
  description,
  className,
}: SliderProps<K>) {
  const labelId = useId();
  const descriptionId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  if (values.length === 0) return null;

  const found = values.findIndex((stop) => stop.id === value);
  const index = found < 0 ? 0 : found;
  const current = values[index]!;
  const descriptionNode = description && (
    <p className="ui-field__description" id={descriptionId}>
      {description}
    </p>
  );

  if (values.length === 1) {
    return (
      <div
        className={cx("ui-slider", className)}
        data-kind="fixed"
        role="group"
        aria-labelledby={label ? labelId : undefined}
        aria-label={label ? undefined : ariaLabel}
        aria-describedby={description ? descriptionId : undefined}
      >
        <div className="ui-slider__header">
          {label && (
            <span className="ui-field__label" id={labelId}>
              {label}
            </span>
          )}
          <span className="ui-slider__value">{current.label}</span>
        </div>
        {descriptionNode}
      </div>
    );
  }

  const last = values.length - 1;
  return (
    <AriaSlider
      className={cx("ui-slider", className)}
      data-kind="slider"
      aria-label={label ? undefined : ariaLabel}
      minValue={0}
      maxValue={last}
      step={1}
      value={index}
      isDisabled={isDisabled}
      onChange={(next) => {
        const stop = values[Array.isArray(next) ? next[0]! : next];
        if (stop && stop.id !== value) onChange(stop.id);
      }}
    >
      <div className="ui-slider__header">
        {label && <Label className="ui-field__label">{label}</Label>}
        <SliderOutput className="ui-slider__value">{() => current.label}</SliderOutput>
      </div>
      <SliderTrack className="ui-slider__track">
        {({ state }) => (
          <>
            <div className="ui-slider__rail" />
            <div className="ui-slider__fill" style={{ width: `${state.getThumbPercent(0) * 100}%` }} />
            {values.map((stop, i) => (
              <span
                key={stop.id}
                className="ui-slider__tick"
                data-reached={i <= index || undefined}
                style={{ left: `${(i / last) * 100}%` }}
                aria-hidden
              />
            ))}
            <SliderThumb
              className="ui-slider__thumb"
              inputRef={inputRef}
              aria-describedby={description ? descriptionId : undefined}
            />
            {/* After the thumb, so its input ref is attached when this runs. */}
            <ValueText inputRef={inputRef} text={current.label} />
          </>
        )}
      </SliderTrack>
      <div className="ui-slider__stops" aria-hidden>
        {values.map((stop, i) => (
          <span
            key={stop.id}
            className="ui-slider__stop"
            data-current={i === index || undefined}
            data-edge={i === 0 ? "start" : i === last ? "end" : undefined}
            style={{ left: `${(i / last) * 100}%` }}
            onClick={() => {
              if (!isDisabled && stop.id !== value) onChange(stop.id);
            }}
          >
            {stop.label}
          </span>
        ))}
      </div>
      {descriptionNode}
    </AriaSlider>
  );
}

/**
 * react-aria derives `aria-valuetext` from a number formatter; name the stop
 * instead. Rendered inside the track so it runs after every slider render.
 */
function ValueText({ inputRef, text }: { inputRef: RefObject<HTMLInputElement | null>; text: string }) {
  useLayoutEffect(() => {
    inputRef.current?.setAttribute("aria-valuetext", text);
  });
  return null;
}
