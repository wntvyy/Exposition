// Export PDF from a page view: the session exports the build it shows to a
// file chosen in the native save dialog; this reports the outcome. A PDF that
// is not the current source says so (the freshness the toolbar already shows
// decides it; nothing is rebuilt or saved for an export).
import { notify, type Notification } from "../../app/notifications";
import { errorMessage } from "../../lib/ipc";
import type { PageSession, PdfExport } from "./session";

/** The notification for a finished export. */
export function exportNotice(exported: PdfExport): Omit<Notification, "id"> {
  if (exported.outdated === "failed") {
    return {
      tone: "warning",
      title: "Exported the last successful build",
      body: `${exported.path} — the source has errors now, so this PDF does not include the latest changes.`,
    };
  }
  if (exported.outdated === "changed") {
    return {
      tone: "warning",
      title: "Exported an earlier build",
      body: `${exported.path} — the source changed after this build, so this PDF does not include the latest changes.`,
    };
  }
  return { tone: "success", title: "Exported PDF", body: exported.path };
}

/**
 * Export the session's shown build and report it. A canceled dialog wrote
 * nothing and says nothing; a failure names the reason (the destination and
 * any existing file there are as they were).
 */
export async function exportPdfWithNotice(session: Pick<PageSession, "exportPdf">): Promise<void> {
  try {
    const exported = await session.exportPdf();
    if (exported) notify(exportNotice(exported));
  } catch (error) {
    notify({ tone: "danger", title: "PDF export failed", body: errorMessage(error) });
  }
}
