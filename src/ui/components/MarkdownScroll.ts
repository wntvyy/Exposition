// Scroll regions in rendered Markdown (code, tables, display math): a block
// that overflows its width becomes a keyboard tab stop so it can be scrolled
// without a pointer; a block that fits stays out of the tab order.
import { useEffect, useRef, useState } from "react";

export function useOverflows<T extends HTMLElement>(content: unknown) {
  const ref = useRef<T>(null);
  const [overflows, setOverflows] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setOverflows(el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    if (el.firstElementChild) observer.observe(el.firstElementChild);
    return () => observer.disconnect();
  }, [content]);
  return [ref, overflows] as const;
}
