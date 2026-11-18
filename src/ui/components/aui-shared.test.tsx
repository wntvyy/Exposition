// Shared components added for the Agent UI rework: Slider, Chip, OptionList,
// Button open state, and PopoverDialog anchored to an element.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { File } from "lucide-react";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Button, Chip, DialogTrigger, OptionList, PopoverDialog, Slider, type SliderStop } from ".";

afterEach(cleanup);

type Effort = "low" | "medium" | "high" | "max";
const efforts: SliderStop<Effort>[] = [
  { id: "low", label: "Low" },
  { id: "medium", label: "Medium" },
  { id: "high", label: "High" },
  { id: "max", label: "Max" },
];

function SliderHarness({ onChange, initial = "medium" }: { onChange?: (id: Effort) => void; initial?: Effort }) {
  const [value, setValue] = useState<Effort>(initial);
  return (
    <Slider
      label="Effort"
      values={efforts}
      value={value}
      description="Levels the model supports."
      onChange={(id) => {
        setValue(id);
        onChange?.(id);
      }}
    />
  );
}

const key = (element: Element, k: string) => {
  fireEvent.keyDown(element, { key: k, code: k });
  fireEvent.keyUp(element, { key: k, code: k });
};

describe("Slider", () => {
  it("names the current stop next to the label, under the track, and in aria-valuetext", () => {
    render(<SliderHarness />);
    const slider = screen.getByRole("slider", { name: "Effort" });
    expect(slider.getAttribute("aria-valuetext")).toBe("Medium");
    expect(slider.getAttribute("min")).toBe("0");
    expect(slider.getAttribute("max")).toBe("3");
    expect(slider.getAttribute("step")).toBe("1");
    expect(document.querySelector(".ui-slider__value")?.textContent).toBe("Medium");
    const stops = [...document.querySelectorAll(".ui-slider__stop")];
    expect(stops.map((s) => s.textContent)).toEqual(["Low", "Medium", "High", "Max"]);
    expect(stops[1]!.hasAttribute("data-current")).toBe(true);
    expect(document.body.textContent).not.toMatch(/%/);
    expect(slider.getAttribute("aria-describedby")).toContain(screen.getByText("Levels the model supports.").id);
  });

  it("moves between stops with arrows, Home, and End", () => {
    const onChange = vi.fn();
    render(<SliderHarness onChange={onChange} />);
    const slider = screen.getByRole("slider", { name: "Effort" });
    act(() => slider.focus());
    key(slider, "ArrowRight");
    expect(onChange).toHaveBeenLastCalledWith("high");
    expect(slider.getAttribute("aria-valuetext")).toBe("High");
    key(slider, "End");
    expect(onChange).toHaveBeenLastCalledWith("max");
    expect(slider.getAttribute("aria-valuetext")).toBe("Max");
    key(slider, "Home");
    expect(onChange).toHaveBeenLastCalledWith("low");
    key(slider, "ArrowLeft");
    expect(onChange).toHaveBeenCalledTimes(3);
    key(slider, "ArrowUp");
    expect(onChange).toHaveBeenLastCalledWith("medium");
    expect(document.querySelector(".ui-slider__value")?.textContent).toBe("Medium");
  });

  it("chooses a stop when its label is clicked, and not when disabled", () => {
    const onChange = vi.fn();
    const { rerender } = render(<Slider label="Effort" values={efforts} value="low" onChange={onChange} />);
    fireEvent.click(screen.getByText("High"));
    expect(onChange).toHaveBeenLastCalledWith("high");
    rerender(<Slider label="Effort" values={efforts} value="low" onChange={onChange} isDisabled />);
    fireEvent.click(screen.getByText("Max"));
    expect(onChange).toHaveBeenCalledTimes(1);
    expect((screen.getByRole("slider") as HTMLInputElement).disabled).toBe(true);
  });

  it("uses aria-label when there is no visible label", () => {
    render(<Slider aria-label="Detail level" values={efforts} value="max" onChange={() => {}} />);
    expect(screen.getByRole("slider", { name: "Detail level" }).getAttribute("aria-valuetext")).toBe("Max");
  });

  it("renders one stop as a fixed read-only value without a thumb", () => {
    render(<Slider label="Effort" values={[{ id: "high", label: "High" }]} value="high" onChange={() => {}} />);
    expect(screen.queryByRole("slider")).toBeNull();
    const group = screen.getByRole("group", { name: "Effort" });
    expect(group.getAttribute("data-kind")).toBe("fixed");
    expect(group.textContent).toContain("High");
    expect(group.querySelector(".ui-slider__thumb")).toBeNull();
  });

  it("renders nothing without stops", () => {
    const { container } = render(<Slider label="Effort" values={[]} value={null} onChange={() => {}} />);
    expect(container.innerHTML).toBe("");
  });
});

describe("Chip", () => {
  it("truncates its label with a title, shows meta, and removes through a named button", () => {
    const onRemove = vi.fn();
    render(<Chip icon={File} label="observations-2026-final.csv" meta="16 KB" onRemove={onRemove} role="listitem" />);
    const label = screen.getByText("observations-2026-final.csv");
    expect(label.getAttribute("title")).toBe("observations-2026-final.csv");
    expect(screen.getByText("16 KB").className).toBe("ui-chip__meta");
    const chip = label.closest(".ui-chip")!;
    expect(chip.getAttribute("role")).toBe("listitem");
    expect(chip.getAttribute("data-tone")).toBe("neutral");
    fireEvent.click(screen.getByRole("button", { name: "Remove observations-2026-final.csv" }));
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  it("has no remove button without onRemove, and a disabled one when disabled", () => {
    const onRemove = vi.fn();
    const { rerender } = render(<Chip label="About: Which baseline?" tone="accent" />);
    expect(screen.queryByRole("button")).toBeNull();
    rerender(<Chip label="About: Which baseline?" tone="accent" removeLabel="Remove About" onRemove={onRemove} isDisabled />);
    const remove = screen.getByRole("button", { name: "Remove About" });
    expect(remove.hasAttribute("disabled") || remove.getAttribute("aria-disabled") === "true").toBe(true);
    fireEvent.click(remove);
    expect(onRemove).not.toHaveBeenCalled();
  });

  it("shows a thumbnail instead of the icon", () => {
    render(<Chip icon={File} label="figure.png" thumbnail={<img alt="" src="data:," />} />);
    const chip = screen.getByText("figure.png").closest(".ui-chip")!;
    expect(chip.querySelector(".ui-chip__thumbnail img")).not.toBeNull();
    expect(chip.querySelector(".ui-chip__icon")).toBeNull();
  });
});

describe("OptionList", () => {
  const options = [
    { id: "refit", label: "Refit with the outlier removed", description: "Replaces figure 2.", recommended: true },
    { id: "keep", label: "Keep the current fit", description: "Nothing changes." },
    { id: "ask", label: "Ask the author first" },
  ] as const;

  function Harness({ onChange, initial = null }: { onChange: (id: string) => void; initial?: string | null }) {
    const [value, setValue] = useState<string | null>(initial);
    return (
      <OptionList
        label="How should the fit change?"
        options={[...options]}
        value={value}
        onChange={(id) => {
          setValue(id);
          onChange(id);
        }}
      />
    );
  }

  it("is a radio group with nothing preselected and a dashed Recommended badge", () => {
    render(<Harness onChange={() => {}} />);
    const group = screen.getByRole("radiogroup", { name: "How should the fit change?" });
    const radios = screen.getAllByRole("radio") as HTMLInputElement[];
    expect(radios).toHaveLength(3);
    expect(radios.every((r) => !r.checked)).toBe(true);
    const badge = screen.getByText("Recommended").closest(".ui-badge")!;
    expect(badge.getAttribute("data-tone")).toBe("accent");
    expect(badge.hasAttribute("data-dashed")).toBe(true);
    expect(badge.closest(".ui-option-list__option")?.textContent).toContain("Refit with the outlier removed");
    expect(group.querySelectorAll(".ui-badge")).toHaveLength(1);
    expect(screen.getByText("Replaces figure 2.").className).toBe("ui-list-row__meta");
    expect(group.querySelectorAll(".ui-option-list__option.ui-list-row")).toHaveLength(3);
  });

  it("selects on click and moves the choice with arrow keys", () => {
    const onChange = vi.fn();
    render(<Harness onChange={onChange} />);
    fireEvent.click(screen.getByText("Keep the current fit"));
    expect(onChange).toHaveBeenLastCalledWith("keep");
    const keep = screen.getByRole("radio", { name: /Keep the current fit/ }) as HTMLInputElement;
    expect(keep.checked).toBe(true);
    expect(keep.closest(".ui-option-list__option")?.hasAttribute("data-selected")).toBe(true);
    act(() => keep.focus());
    key(keep, "ArrowDown");
    expect(onChange).toHaveBeenLastCalledWith("ask");
    expect((screen.getByRole("radio", { name: /Ask the author first/ }) as HTMLInputElement).checked).toBe(true);
  });

  it("shows a given value as chosen and disables every option", () => {
    render(<OptionList aria-label="Choice" options={[...options]} value="keep" onChange={() => {}} isDisabled />);
    const radios = screen.getAllByRole("radio") as HTMLInputElement[];
    expect(radios.map((r) => r.checked)).toEqual([false, true, false]);
    expect(radios.every((r) => r.disabled)).toBe(true);
  });
});

describe("Button open state", () => {
  it("carries aria-expanded while its popover is open", () => {
    render(
      <DialogTrigger>
        <Button>Options</Button>
        <PopoverDialog title="Options">
          <p>Body</p>
        </PopoverDialog>
      </DialogTrigger>,
    );
    const button = screen.getByRole("button", { name: "Options" });
    expect(button.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(button.className).toContain("ui-button");
  });
});

describe("PopoverDialog anchored to an element", () => {
  function Anchored({ onOpenChange, width, maxHeight }: { onOpenChange?: (open: boolean) => void; width?: "anchor"; maxHeight?: number }) {
    const anchorRef = useRef<HTMLDivElement>(null);
    const [open, setOpen] = useState(false);
    return (
      <>
        <div ref={anchorRef} className="composer">
          <Button aria-expanded={open} onPress={() => setOpen(true)}>
            Model
          </Button>
        </div>
        <button type="button">Elsewhere</button>
        <PopoverDialog
          title="Model and effort"
          hideTitle
          triggerRef={anchorRef}
          isOpen={open}
          onOpenChange={(next) => {
            setOpen(next);
            onOpenChange?.(next);
          }}
          placement="top"
          width={width}
          maxHeight={maxHeight}
        >
          <Button>Inside</Button>
        </PopoverDialog>
      </>
    );
  }

  it("follows the controlled open state", () => {
    function Controlled({ isOpen }: { isOpen: boolean }) {
      const ref = useRef<HTMLDivElement>(null);
      return (
        <>
          <div ref={ref}>anchor</div>
          <PopoverDialog title="Sheet" triggerRef={ref} isOpen={isOpen} onOpenChange={() => {}}>
            <p>Sheet body</p>
          </PopoverDialog>
        </>
      );
    }
    const { rerender } = render(<Controlled isOpen={false} />);
    expect(screen.queryByRole("dialog")).toBeNull();
    rerender(<Controlled isOpen />);
    expect(screen.getByRole("dialog", { name: "Sheet" })).toBeTruthy();
    expect(screen.getByText("Sheet body")).toBeTruthy();
    rerender(<Controlled isOpen={false} />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("closes on Escape and returns focus to the trigger", async () => {
    const onOpenChange = vi.fn();
    render(<Anchored onOpenChange={onOpenChange} width="anchor" maxHeight={300} />);
    const trigger = screen.getByRole("button", { name: "Model" });
    act(() => trigger.focus());
    fireEvent.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    const dialog = screen.getByRole("dialog", { name: "Model and effort" });
    expect(dialog.closest(".ui-popover-dialog")?.getAttribute("data-width")).toBe("anchor");
    expect(dialog.style.maxHeight).toBe("300px");
    expect(dialog.hasAttribute("data-bounded")).toBe(true);
    const inside = screen.getByRole("button", { name: "Inside" });
    act(() => inside.focus());
    fireEvent.keyDown(inside, { key: "Escape", code: "Escape" });
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });

  it("closes on a click outside", () => {
    const onOpenChange = vi.fn();
    render(<Anchored onOpenChange={onOpenChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Model" }));
    expect(screen.getByRole("dialog")).toBeTruthy();
    const outside = screen.getByRole("button", { name: "Elsewhere", hidden: true });
    fireEvent.pointerDown(outside, { pointerId: 1, button: 0 });
    fireEvent.mouseDown(outside, { button: 0 });
    fireEvent.pointerUp(outside, { pointerId: 1, button: 0 });
    fireEvent.mouseUp(outside, { button: 0 });
    fireEvent.click(outside);
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("bounds the content to the space between the anchor and the boundary", () => {
    function Bounded() {
      const anchorRef = useRef<HTMLDivElement>(null);
      const [panel, setPanel] = useState<HTMLDivElement | null>(null);
      return (
        <div ref={setPanel} data-rect="panel">
          <div ref={anchorRef} data-rect="anchor">
            composer
          </div>
          {panel && (
            <PopoverDialog title="Options" triggerRef={anchorRef} isOpen onOpenChange={() => {}} placement="top" boundaryElement={panel}>
              <p>Options body</p>
            </PopoverDialog>
          )}
        </div>
      );
    }
    const rect = (top: number, bottom: number) =>
      ({ top, bottom, left: 0, right: 360, width: 360, height: bottom - top, x: 0, y: top, toJSON: () => ({}) }) as DOMRect;
    const spy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.dataset.rect === "anchor") return rect(500, 600);
      if (this.dataset.rect === "panel") return rect(100, 700);
      return rect(0, 0);
    });
    try {
      render(<Bounded />);
      const dialog = screen.getByRole("dialog", { name: "Options" });
      // 500 - 100 = 400 available, minus offset 4 and padding 8 (jsdom has no popover padding).
      expect(dialog.style.maxHeight).toBe("388px");
    } finally {
      spy.mockRestore();
    }
  });
});
