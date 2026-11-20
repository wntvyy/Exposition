// Export PDF from a page view (QC 2.4): the destination is chosen in the
// backend's native save dialog, so the view sends only the identity of what
// it shows (the build and its document), never a path. Canceling reports
// nothing; an export that is not the current source says so; failures are
// shown with the backend's reason.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => ({
  exportPdf: vi.fn(),
  notify: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: 1 }, text: "= Title" }),
    typesettingWatch: async () => null,
    typesettingUnwatch: async () => {},
    typesettingExportPdf: backend.exportPdf,
    pageImage: async () => new ArrayBuffer(4),
    pageCaret: async () => null,
  },
  events: {
    documents: async () => () => {},
    typesettingBuild: async () => () => {},
    trustChanged: async () => () => {},
  },
  errorMessage: (e: unknown) => (e as { message?: string }).message ?? String(e),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e && "message" in e,
  newId: (prefix: string) => `${prefix}-1`,
}));
vi.mock("../editor", () => ({
  openDocument: async (path: string) => ({ id: `doc:${path}`, editReason: null, subscribe: () => () => {} }),
}));
vi.mock("../../ui/components", () => ({ confirm: vi.fn() }));
vi.mock("../../app/notifications", () => ({ notify: backend.notify, dismiss: vi.fn() }));

import type { BuildEvent } from "../../bindings/BuildEvent";
import { exportPdfWithNotice } from "./exportPdf";
import { PageSession } from "./session";

function build(entry: string, seq: number, revision: number, ok = true): BuildEvent {
  const result = {
    id: `typst-${seq}`,
    target: { format: "typst", engine: "typst", entry },
    snapshot: "",
    sources: [{ path: entry, document: `doc:${entry}`, revision, contentHash: "" }],
    configuration: null,
    status: ok ? { kind: "succeeded" } : { kind: "failed" },
    diagnostics: ok ? [] : [{ severity: "error", message: "unknown variable" }],
    pages: [],
    mapping: "glyph",
    exports: [],
    durationMs: 1,
  };
  return { result, timings: {}, lastGood: ok ? result : null, refusal: null } as unknown as BuildEvent;
}

const artifact = (path: string) => ({ kind: "pdf", path, contentHash: "h" });

describe("Export PDF from a page view", () => {
  let main: PageSession;
  let other: PageSession;

  beforeEach(async () => {
    backend.exportPdf.mockReset();
    backend.notify.mockReset();
    main = new PageSession("main.typ");
    other = new PageSession("chapters/intro.typ");
    await main.attach();
    await other.attach();
  });

  afterEach(() => {
    main.detach();
    other.detach();
  });

  it("exports nothing before a build is shown", async () => {
    await exportPdfWithNotice(main);
    expect(backend.exportPdf).not.toHaveBeenCalled();
    expect(backend.notify).toHaveBeenCalledWith(expect.objectContaining({ tone: "danger", title: "PDF export failed" }));
  });

  it("sends the shown build and its document, and no destination", async () => {
    await main.onBuild(build("main.typ", 1, 1));
    await other.onBuild(build("chapters/intro.typ", 2, 1));
    // Builds of other entries never become this view's build.
    await main.onBuild(build("chapters/intro.typ", 3, 1));
    backend.exportPdf.mockResolvedValue(artifact("D:\\Exports — été\\main.pdf"));

    await exportPdfWithNotice(main);
    expect(backend.exportPdf).toHaveBeenCalledTimes(1);
    // (build, entry): the backend opens the dialog; the view supplies no path.
    expect(backend.exportPdf).toHaveBeenLastCalledWith("typst-1", "main.typ");
    expect(backend.notify).toHaveBeenLastCalledWith({
      tone: "success",
      title: "Exported PDF",
      body: "D:\\Exports — été\\main.pdf",
    });

    await exportPdfWithNotice(other);
    expect(backend.exportPdf).toHaveBeenLastCalledWith("typst-2", "chapters/intro.typ");
  });

  it("a canceled dialog reports nothing", async () => {
    await main.onBuild(build("main.typ", 1, 1));
    backend.exportPdf.mockRejectedValue({ code: "typesetting.exportCanceled", message: "The export was canceled." });
    await exportPdfWithNotice(main);
    expect(backend.exportPdf).toHaveBeenCalledTimes(1);
    expect(backend.notify).not.toHaveBeenCalled();
  });

  it("says when the exported PDF is the last successful build of a source with errors", async () => {
    await main.onBuild(build("main.typ", 1, 1));
    await main.onBuild(build("main.typ", 2, 2, false));
    expect(main.getModel()).toMatchObject({ buildId: "typst-1", failed: true });
    backend.exportPdf.mockResolvedValue(artifact("C:\\out\\main.pdf"));
    await exportPdfWithNotice(main);
    // The pages shown (the last good build) are what is exported.
    expect(backend.exportPdf).toHaveBeenLastCalledWith("typst-1", "main.typ");
    const notice = backend.notify.mock.calls.at(-1)?.[0];
    expect(notice).toMatchObject({ tone: "warning", title: "Exported the last successful build" });
    expect(notice.body).toContain("C:\\out\\main.pdf");
    expect(notice.body).toContain("does not include the latest changes");
  });

  it("says when the source changed after the exported build, also while the dialog was open", async () => {
    await main.onBuild(build("main.typ", 1, 1));
    // A newer build arrives while the person chooses the destination.
    backend.exportPdf.mockImplementation(async () => {
      await main.onBuild(build("main.typ", 2, 2));
      return artifact("C:\\out\\main.pdf");
    });
    await exportPdfWithNotice(main);
    expect(backend.exportPdf).toHaveBeenLastCalledWith("typst-1", "main.typ");
    expect(main.getModel().buildId).toBe("typst-2");
    const notice = backend.notify.mock.calls.at(-1)?.[0];
    expect(notice).toMatchObject({ tone: "warning", title: "Exported an earlier build" });
    expect(notice.body).toContain("C:\\out\\main.pdf");

    // Exported again, the current build is reported plainly.
    backend.exportPdf.mockResolvedValue(artifact("C:\\out\\main.pdf"));
    await exportPdfWithNotice(main);
    expect(backend.exportPdf).toHaveBeenLastCalledWith("typst-2", "main.typ");
    expect(backend.notify).toHaveBeenLastCalledWith(expect.objectContaining({ tone: "success", title: "Exported PDF" }));
  });

  it("shows why an export failed", async () => {
    await main.onBuild(build("main.typ", 1, 1));
    const message =
      "export failed: C:\\out\\main.pdf cannot be written: permission was denied (the folder or file is read-only, protected, or open in another program). The existing file was not changed.";
    backend.exportPdf.mockRejectedValue({ code: "typesetting.export", message });
    await exportPdfWithNotice(main);
    expect(backend.notify).toHaveBeenLastCalledWith({ tone: "danger", title: "PDF export failed", body: message });
  });
});
