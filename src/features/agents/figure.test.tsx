// Figures in the conversation: bytes fetched lazily through agents_figure,
// a blob URL with the recorded media type rendered only as <img> (SVG too)
// and revoked on unmount, the oversize refusal, unavailable and error
// states, Expand, Open in Research, and Research's promotion flow reused.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { FigureRef } from "../../bindings/FigureRef";
import type { RunSummary } from "../../bindings/RunSummary";

const ipc = vi.hoisted(() => ({ agentsFigure: vi.fn() }));
const research = vi.hoisted(() => ({ revealInResearch: vi.fn(), promotion: vi.fn() }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: {},
  errorMessage: (e: unknown) =>
    typeof e === "object" && e !== null && "message" in e ? String((e as { message: unknown }).message) : String(e),
}));
vi.mock("../research/space", () => ({ revealInResearch: research.revealInResearch }));
vi.mock("../research/Promotion", () => ({
  PromotionControls: (props: { run: RunSummary; artifact: { id: string } }) => {
    research.promotion(props);
    return <div data-testid="promotion-controls" data-artifact={props.artifact.id} />;
  },
}));
vi.mock("../research/store", async () => {
  const { create } = await import("zustand");
  return { useResearch: create<{ runs: RunSummary[] }>(() => ({ runs: [] })) };
});

const { FigureCard, FIGURE_MAX_BYTES, FIGURE_NOTE, shortRunId, unavailableText } = await import("./FigureCard");
const { useResearch } = (await import("../research/store")) as unknown as {
  useResearch: { setState: (s: { runs: RunSummary[] }) => void };
};

const figure: FigureRef = {
  runId: "run-4f2a9c1e00112233",
  artifactId: "run-4f2a9c1e00112233/fig_arrhenius.png",
  mediaType: "image/png",
  name: "fig_arrhenius.png",
  caption: "Arrhenius plot with a fitted line",
};

let created: Blob[] = [];
const createObjectURL = vi.fn((blob: Blob) => {
  created.push(blob);
  return `blob:figure-${created.length}`;
});
const revokeObjectURL = vi.fn();
const originalObserver = globalThis.IntersectionObserver;

beforeEach(() => {
  created = [];
  createObjectURL.mockClear();
  revokeObjectURL.mockClear();
  ipc.agentsFigure.mockReset();
  research.revealInResearch.mockReset();
  research.promotion.mockReset();
  URL.createObjectURL = createObjectURL as unknown as typeof URL.createObjectURL;
  URL.revokeObjectURL = revokeObjectURL;
  useResearch.setState({ runs: [] });
});

afterEach(() => {
  cleanup();
  globalThis.IntersectionObserver = originalObserver;
});

const png = () => new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer;

describe("FigureCard", () => {
  it("fetches the bytes once visible, shows them as an image from a blob URL, and revokes it on unmount", async () => {
    let trigger: ((entries: { isIntersecting: boolean }[]) => void) | null = null;
    const disconnect = vi.fn();
    globalThis.IntersectionObserver = class {
      constructor(callback: (entries: { isIntersecting: boolean }[]) => void) {
        trigger = callback;
      }
      observe() {}
      disconnect = disconnect;
    } as unknown as typeof IntersectionObserver;
    ipc.agentsFigure.mockResolvedValue(png());
    const { unmount } = render(<FigureCard task="task-1" figure={figure} />);

    // Not on screen yet: nothing fetched, a placeholder the size of a preview.
    expect(ipc.agentsFigure).not.toHaveBeenCalled();
    expect(screen.getByRole("status", { name: "Loading fig_arrhenius.png" })).toBeTruthy();
    act(() => trigger?.([{ isIntersecting: true }]));
    expect(disconnect).toHaveBeenCalled();
    expect(ipc.agentsFigure).toHaveBeenCalledWith("task-1", figure.runId, figure.artifactId);

    const img = (await screen.findByRole("img", { name: "Arrhenius plot with a fitted line" })) as HTMLImageElement;
    expect(img.getAttribute("src")).toBe("blob:figure-1");
    expect(created[0]?.type).toBe("image/png");
    expect(screen.getByText("fig_arrhenius.png").tagName).toBe("CODE");
    expect(screen.getByText("Run 4f2a9c1 · Python").getAttribute("title")).toBe(figure.runId);
    expect(screen.getByText("Arrhenius plot with a fitted line", { selector: "span" })).toBeTruthy();
    expect(screen.getByText(FIGURE_NOTE)).toBeTruthy();

    unmount();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:figure-1");
    expect(ipc.agentsFigure).toHaveBeenCalledTimes(1);
  });

  it("fetches immediately without IntersectionObserver and renders an SVG only as an <img>", async () => {
    // @ts-expect-error -- the fallback path
    delete globalThis.IntersectionObserver;
    const svg = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>');
    ipc.agentsFigure.mockResolvedValue(svg.buffer);
    const { container } = render(
      <FigureCard task="task-1" figure={{ ...figure, mediaType: "image/svg+xml", name: "plot.svg", caption: null }} />,
    );
    const img = await screen.findByRole("img", { name: "plot.svg" });
    expect(img.tagName).toBe("IMG");
    expect(created[0]?.type).toBe("image/svg+xml");
    expect(container.querySelector("svg:not([aria-hidden])")).toBeNull();
    expect(container.querySelector("script, iframe, object, embed")).toBeNull();
    expect(container.innerHTML).not.toContain("alert(1)");
  });

  it("refuses a figure larger than 20 MiB with a message and creates no URL", async () => {
    ipc.agentsFigure.mockResolvedValue(new ArrayBuffer(FIGURE_MAX_BYTES + 3 * 1024 * 1024));
    render(<FigureCard task="task-1" figure={figure} />);
    const note = await screen.findByTestId("agents-figure-unavailable");
    expect(note.textContent).toMatch(/Figure too large to show here \(23\.0 MiB; the limit is 20\.0 MiB\)/);
    expect(createObjectURL).not.toHaveBeenCalled();
    expect(screen.queryByRole("img")).toBeNull();
  });

  it("shows the unavailable reason without actions", async () => {
    ipc.agentsFigure.mockRejectedValue({ code: "agents.figureUnavailable", message: "Purged from Storage & History" });
    render(<FigureCard task="task-1" figure={figure} />);
    expect((await screen.findByTestId("agents-figure-unavailable")).textContent).toBe(
      "Figure no longer available — purged from Storage & History",
    );
    expect(screen.queryByRole("button")).toBeNull();
    expect(unavailableText("Figure no longer available — not this task's")).toBe("Figure no longer available — not this task's");
  });

  it("does not fetch a media type that is not a figure image", () => {
    render(<FigureCard task="task-1" figure={{ ...figure, mediaType: "text/html", name: "page.html" }} />);
    expect(screen.getByTestId("agents-figure-unavailable").textContent).toContain("text/html");
    expect(ipc.agentsFigure).not.toHaveBeenCalled();
  });

  it("reports other errors and retries", async () => {
    ipc.agentsFigure.mockRejectedValueOnce({ code: "io", message: "disk busy" }).mockResolvedValueOnce(png());
    render(<FigureCard task="task-1" figure={figure} />);
    expect((await screen.findByTestId("agents-figure-error")).textContent).toContain("Could not load the figure: disk busy");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("img")).toBeTruthy();
    expect(ipc.agentsFigure).toHaveBeenCalledTimes(2);
  });

  it("expands in a dialog and opens the run in Research", async () => {
    ipc.agentsFigure.mockResolvedValue(png());
    render(<FigureCard task="task-1" figure={figure} />);
    await screen.findByRole("img");
    fireEvent.click(screen.getByRole("button", { name: "Open in Research" }));
    expect(research.revealInResearch).toHaveBeenCalledWith({ kind: "run", runId: figure.runId });
    fireEvent.click(screen.getByRole("button", { name: "Expand" }));
    const dialog = await screen.findByRole("dialog", { name: "fig_arrhenius.png" });
    expect(dialog.querySelector("img")?.getAttribute("src")).toBe("blob:figure-1");
  });

  it("offers Research's promotion flow only when Research lists the run's figure", async () => {
    ipc.agentsFigure.mockResolvedValue(png());
    const { rerender } = render(<FigureCard task="task-1" figure={figure} />);
    await screen.findByRole("img");
    expect(screen.queryByTestId("promotion-controls")).toBeNull();
    const run = {
      id: figure.runId,
      status: { kind: "succeeded" },
      outputs: [
        {
          id: figure.artifactId,
          kind: "figure",
          path: ".exposition/artifacts/run-4f2a9c1e00112233/fig_arrhenius.png",
          scratchPath: null,
          mediaType: "image/png",
          sizeBytes: 4,
          contentHash: "sha256:00",
        },
      ],
    } as unknown as RunSummary;
    act(() => useResearch.setState({ runs: [run] }));
    rerender(<FigureCard task="task-1" figure={figure} />);
    expect(screen.getByTestId("promotion-controls").getAttribute("data-artifact")).toBe(figure.artifactId);
    expect(research.promotion).toHaveBeenLastCalledWith({ run, artifact: run.outputs[0] });
  });

  it("names runs briefly", () => {
    expect(shortRunId("run-4f2a9c1e00112233")).toBe("4f2a9c1");
  });
});
