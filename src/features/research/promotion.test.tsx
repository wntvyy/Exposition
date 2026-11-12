// Reviewed promotion in the Research UI: "Insert into document" only prepares
// a review (exact markup, target revision, digest); nothing is applied until
// the person approves; a stale approval is refused with "Review again"; a
// failed graph write shows a pending record with Reconcile.
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { PromotionOutcome } from "../../bindings/PromotionOutcome";
import type { PromotionPreview } from "../../bindings/PromotionPreview";
import type { RunSummary } from "../../bindings/RunSummary";

vi.hoisted(() => {
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
});

const ipc = vi.hoisted(() => ({
  documentList: vi.fn(),
  documentState: vi.fn(),
}));
const research = vi.hoisted(() => ({
  preparePromotion: vi.fn(),
  approvePromotion: vi.fn(),
  cancelPromotion: vi.fn(),
  reconcilePromotion: vi.fn(),
  promotions: vi.fn(),
  listing: vi.fn(),
  retain: vi.fn(),
  setResultState: vi.fn(),
}));
const graph = vi.hoisted(() => ({ snapshot: vi.fn() }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { research: vi.fn(), researchResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => (e && typeof e === "object" && "message" in e ? String((e as { message: string }).message) : String(e)),
  isCommandError: (e: unknown) => Boolean(e && typeof e === "object" && "code" in e),
}));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: graph, graphEvents: { graph: vi.fn(), graphResync: vi.fn() } }));
vi.mock("./ipc", () => ({ researchCommands: research }));
vi.mock("../editor/documentClient", () => ({
  openDocument: vi.fn(),
  fileKey: (p: string) => `file:${p}`,
  handleForPath: vi.fn(),
  saveDocument: vi.fn(),
}));

const { PromotionControls, RetainControls, lifecyclePill } = await import("./Promotion");
const { useSpace } = await import("./space");

const artifact: ArtifactRecord = {
  id: "run-1/output-1.png",
  kind: "figure",
  path: ".exposition/artifacts/run-1/output-1.png",
  scratchPath: null,
  mediaType: "image/png",
  sizeBytes: 100,
  contentHash: "sha256:abcdef0123456789",
};

const run = { id: "run-1", status: { kind: "succeeded" }, outputs: [artifact] } as unknown as RunSummary;

const preview: PromotionPreview = {
  promotion: "promo-1",
  document: "doc-1",
  path: "paper.typ",
  format: "typst",
  revision: 4,
  attachmentKey: "fig-abcdef01",
  update: false,
  edits: [{ range: { start: 10, end: 10 }, line: 3, replaced: "", text: "// exposition:begin fig-abcdef01\n#figure(...)\n// exposition:end fig-abcdef01\n" }],
  runId: "run-1",
  artifact,
  retained: null,
  warnings: [],
  expiresAtMs: 0,
};

beforeEach(() => {
  ipc.documentList.mockResolvedValue([{ id: "doc-1", path: "paper.typ", readOnly: null }]);
  ipc.documentState.mockResolvedValue({ info: { revision: 4 }, text: "= Paper\n\nText.\n" });
  graph.snapshot.mockResolvedValue({ entities: [], revision: 1 });
  research.preparePromotion.mockResolvedValue(preview);
  research.listing.mockResolvedValue({ results: [] });
  research.promotions.mockResolvedValue({ unresolved: [], tracked: [] });
  useSpace.setState({ listing: null });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

async function openReview() {
  render(<PromotionControls run={run} artifact={artifact} />);
  fireEvent.click(screen.getByRole("button", { name: "Insert or update in document…" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Insert into document" })).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Insert into document" }));
  await screen.findByTestId("research-promotion-review");
}

describe("reviewed promotion", () => {
  it("prepares a review with the exact markup, revision, and digest before anything is applied", async () => {
    await openReview();
    expect(research.preparePromotion).toHaveBeenCalledWith(
      expect.objectContaining({ runId: "run-1", artifactId: artifact.id, document: "doc-1", baseRevision: 4 }),
    );
    expect(research.approvePromotion).not.toHaveBeenCalled();
    expect(screen.getByTestId("research-promotion-markup").textContent).toContain("// exposition:begin fig-abcdef01");
    expect(screen.getByTestId("research-promotion-revision").textContent).toBe("4");
    expect(screen.getByTestId("research-promotion-digest").textContent).toContain("abcdef0123456789");
  });

  it("applies only on approval, by the prepared id", async () => {
    const outcome: PromotionOutcome = {
      operation: "op-1",
      document: "doc-1",
      path: "paper.typ",
      revision: 5,
      transaction: "attach-doc-1-4-fig",
      attachmentKey: "fig-abcdef01",
      range: { start: 10, end: 40 },
      warnings: [],
      graph: { kind: "recorded", graphRevision: 9, manuscript: "ent-m", artifact: "ent-a", relation: "rel-1" },
    };
    research.approvePromotion.mockResolvedValue(outcome);
    await openReview();
    fireEvent.click(screen.getByRole("button", { name: "Approve insertion" }));
    await waitFor(() => expect(research.approvePromotion).toHaveBeenCalledWith("promo-1"));
    await waitFor(() => expect(screen.queryByTestId("research-promotion-review")).toBeNull());
  });

  it("refuses a stale approval and offers to review again", async () => {
    research.approvePromotion.mockRejectedValue({ code: "promotion.stale", message: "paper.typ changed since you reviewed" });
    await openReview();
    fireEvent.click(screen.getByRole("button", { name: "Approve insertion" }));
    const error = await screen.findByTestId("research-promotion-error");
    expect(error.getAttribute("data-code")).toBe("promotion.stale");
    expect(screen.queryByRole("button", { name: "Approve insertion" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Review again" }));
    await waitFor(() => expect(research.preparePromotion).toHaveBeenCalledTimes(2));
  });

  it("shows a pending reconciliation record when the graph write failed", async () => {
    research.approvePromotion.mockResolvedValue({
      operation: "op-2",
      document: "doc-1",
      path: "paper.typ",
      revision: 5,
      transaction: "t",
      attachmentKey: "fig-abcdef01",
      range: { start: 0, end: 1 },
      warnings: [],
      graph: { kind: "pendingReconciliation", reason: "the graph write failed" },
    } satisfies PromotionOutcome);
    research.reconcilePromotion.mockResolvedValue({ record: { state: { kind: "resolved", resolution: "ok" } }, attachmentPresent: true });
    await openReview();
    fireEvent.click(screen.getByRole("button", { name: "Approve insertion" }));
    const pending = await screen.findByTestId("research-promotion-pending");
    expect(pending.getAttribute("data-operation")).toBe("op-2");
    fireEvent.click(screen.getByRole("button", { name: "Reconcile now" }));
    await waitFor(() => expect(research.reconcilePromotion).toHaveBeenCalledWith("op-2"));
  });
});

describe("retention controls", () => {
  it("labels unretained output and only offers Discard for a retained result", async () => {
    render(<RetainControls run={run} artifact={artifact} />);
    expect(screen.getByTestId("research-retain").getAttribute("data-state")).toBe("none");
    expect(screen.queryByRole("button", { name: "Discard" })).toBeNull();
    research.retain.mockResolvedValue({ result: { path: artifact.path }, evidence: "evd-1" });
    fireEvent.click(screen.getByRole("button", { name: "Keep" }));
    await waitFor(() =>
      expect(research.retain).toHaveBeenCalledWith({ runId: "run-1", artifactId: artifact.id, state: "active", experiment: null }),
    );
    expect(lifecyclePill(undefined).label).toBe("Not retained");
  });
});
