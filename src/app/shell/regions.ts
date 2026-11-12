// Region focus navigation (F6 / Shift+F6). Workbench regions are elements
// with `data-region`; focus returns to the element last focused inside the
// target region, else its first tabbable element, else the region itself.
//
// Regions may nest: a page container is a region, and a page can mark its own
// parts (toolbar, main view, inspector) as regions. Only the innermost shown
// regions take part in cycling, so a page without inner regions is one stop
// and a page with inner regions contributes each of them.

const lastFocused = new WeakMap<Element, HTMLElement>();
let tracking = false;

const TABBABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function isShown(element: Element): boolean {
  if (!(element instanceof HTMLElement) || element.closest("[inert], [hidden]")) return false;
  // `checkVisibility` is absent in non-layout DOMs (tests); fall back to the attribute checks above.
  return typeof element.checkVisibility === "function" ? element.checkVisibility() : true;
}

/** Remember the last focused element per region. Idempotent. */
export function trackRegionFocus(doc: Document = document): void {
  if (tracking) return;
  tracking = true;
  doc.addEventListener("focusin", (event) => {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;
    const region = target.closest("[data-region]");
    if (region && region !== target) lastFocused.set(region, target);
  });
}

/** Shown regions in document order, innermost only (a region containing other shown regions is skipped). */
export function regionElements(doc: Document = document): HTMLElement[] {
  const shown = [...doc.querySelectorAll<HTMLElement>("[data-region]")].filter(isShown);
  return shown.filter((region) => !shown.some((other) => other !== region && region.contains(other)));
}

export function focusRegion(region: HTMLElement): void {
  const remembered = lastFocused.get(region);
  if (remembered && remembered.isConnected && region.contains(remembered) && isShown(remembered)) {
    remembered.focus();
    if (region.contains(document.activeElement)) return;
  }
  const first = [...region.querySelectorAll<HTMLElement>(TABBABLE)].find(isShown);
  if (first) {
    first.focus();
    if (region.contains(document.activeElement)) return;
  }
  if (!region.hasAttribute("tabindex")) region.tabIndex = -1;
  region.focus();
}

/** Move focus to the next (or previous) visible region, wrapping around. */
export function focusNextRegion(backwards = false, doc: Document = document): void {
  const regions = regionElements(doc);
  if (!regions.length) return;
  const active = doc.activeElement;
  let current = regions.findIndex((r) => r === active || r.contains(active));
  if (current < 0 && active) {
    // Focus is in a region's chrome outside its inner regions (e.g. a page
    // header): continue from the last inner region that precedes it.
    const before = regions.filter((r) => r.compareDocumentPosition(active) & Node.DOCUMENT_POSITION_FOLLOWING);
    if (before.length) current = regions.indexOf(before[before.length - 1]!);
  }
  const step = backwards ? -1 : 1;
  const next = current < 0 ? (backwards ? regions.length - 1 : 0) : (current + step + regions.length) % regions.length;
  focusRegion(regions[next]!);
}

/** Focus a region by name (`sidebar`, `editor`, `panel`, `auxiliary`, `statusbar`). Returns false if hidden. */
export function focusRegionByName(name: string, doc: Document = document): boolean {
  const region = regionElements(doc).find((r) => r.dataset.region === name);
  if (!region) return false;
  focusRegion(region);
  return true;
}
