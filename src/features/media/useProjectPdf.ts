// React binding of the PDF view: open a project PDF while a component shows
// it, follow changes on disk, and release it when the component is gone.
import { useEffect, useRef, useState } from "react";

import { commands } from "../../lib/ipc";
import { onImageChange } from "./loader";
import { PdfLoadError, PdfPages, pdfProblemOf, type OpenPdf } from "./pdf";

export type PdfState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; pdf: OpenPdf }
  | { status: "error"; error: PdfLoadError };

/**
 * The PDF at `path` (null: nothing is opened). When the file changes on disk
 * (or the Rust side dropped the view) it is opened again; the pages shown stay
 * until the new result replaces them. A result for a previous path, an earlier
 * load, or another project is discarded and its view released.
 */
export function useProjectPdf(path: string | null): PdfState & { retry: () => void } {
  const [state, setState] = useState<PdfState>({ status: path ? "loading" : "idle" });
  const [nonce, setNonce] = useState(0);
  /** The view whose pages are shown now (disposed when replaced or unmounted). */
  const shown = useRef<OpenPdf | null>(null);

  // The shared project-file change tracker (watcher events, project opened/closed).
  useEffect(() => {
    if (!path) return;
    return onImageChange(path, () => setNonce((n) => n + 1));
  }, [path]);

  useEffect(() => {
    const release = () => {
      shown.current?.pages.dispose();
      shown.current = null;
    };
    if (!path) {
      release();
      setState({ status: "idle" });
      return;
    }
    let live = true;
    // Another file: never keep the previous file's pages.
    if (shown.current && shown.current.path !== path) release();
    setState((s) => (shown.current && s.status === "ready" ? s : { status: "loading" }));
    commands.mediaPdfOpen(path).then(
      (info) => {
        if (!live) {
          // Superseded (file changed again, tab closed, project switched): release it in Rust.
          void commands.mediaPdfClose(info.token).catch(() => {});
          return;
        }
        release();
        const pdf: OpenPdf = { path, info, pages: new PdfPages(info.token, () => live && setNonce((n) => n + 1)) };
        shown.current = pdf;
        setState({ status: "ready", pdf });
      },
      (error: unknown) => {
        if (!live) return;
        const problem = pdfProblemOf(error);
        // A project switch is followed by a change notification (or the tab closes).
        if (problem.superseded) return;
        release();
        setState({ status: "error", error: problem });
      },
    );
    return () => {
      live = false;
    };
  }, [path, nonce]);

  // Release whatever is shown on unmount.
  useEffect(
    () => () => {
      shown.current?.pages.dispose();
      shown.current = null;
    },
    [],
  );

  return { ...state, retry: () => setNonce((n) => n + 1) };
}
