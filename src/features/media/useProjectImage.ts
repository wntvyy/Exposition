// React binding of the image loader: load a project image while a component
// shows it, follow changes on disk, and release it when the component is gone.
import { useEffect, useRef, useState } from "react";

import { acquireImage, ImageLoadError, onImageChange, type ImageHandle, type LoadedImage } from "./loader";

export type ImageState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; image: LoadedImage }
  | { status: "error"; error: ImageLoadError };

/**
 * The image at `path` (null or `enabled` false: nothing is loaded). When the
 * file changes on disk the image is loaded again; the previous picture stays
 * (and keeps its URL) until the new result replaces it.
 */
export function useProjectImage(
  path: string | null,
  variant: "full" | "preview" = "full",
  enabled = true,
): ImageState & { retry: () => void } {
  const [state, setState] = useState<ImageState>({ status: path && enabled ? "loading" : "idle" });
  const [nonce, setNonce] = useState(0);
  /** The handle whose image is shown now (released when replaced or unmounted). */
  const shown = useRef<ImageHandle | null>(null);
  const key = path && enabled ? `${variant}:${path}` : null;
  const shownKey = useRef<string | null>(null);

  useEffect(() => {
    if (!path || !enabled) return;
    return onImageChange(path, () => setNonce((n) => n + 1));
  }, [path, enabled]);

  useEffect(() => {
    if (!key || !path) {
      shown.current?.release();
      shown.current = null;
      shownKey.current = null;
      setState({ status: "idle" });
      return;
    }
    let live = true;
    // Another file: never keep the previous file's picture.
    const sameFile = shownKey.current === key;
    setState((s) => (sameFile && s.status === "ready" ? s : { status: "loading" }));
    const handle = acquireImage(path, variant);
    const settle = (next: ImageState) => {
      if (!live) {
        handle.release();
        return;
      }
      if (shown.current !== handle) shown.current?.release();
      shown.current = handle;
      shownKey.current = key;
      setState(next);
    };
    handle.promise.then(
      (image) => settle({ status: "ready", image }),
      (error: unknown) => {
        // A superseded load (file changed, project switched) is followed by a
        // change notification that loads again; keep what is shown meanwhile.
        if (error instanceof ImageLoadError && error.superseded) {
          handle.release();
          return;
        }
        settle({
          status: "error",
          error: error instanceof ImageLoadError ? error : new ImageLoadError("unavailable", String(error)),
        });
      },
    );
    return () => {
      live = false;
      if (shown.current !== handle) handle.release();
    };
  }, [key, path, variant, nonce]);

  // Release whatever is shown on unmount.
  useEffect(
    () => () => {
      shown.current?.release();
      shown.current = null;
    },
    [],
  );

  return { ...state, retry: () => setNonce((n) => n + 1) };
}
