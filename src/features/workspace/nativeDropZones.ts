// Surfaces other than the Files view that take files dragged from the
// operating system (the Agent chat). The one native listener and the routing
// are in `dropTarget.ts`; a surface registers a zone here. This module
// imports nothing, so any feature can register without loading the explorer.

/** A surface that takes OS file drops. */
export interface NativeDropZone {
  /** A drag is over `element`: highlight this zone's target and return true to claim the drag, or return false. */
  hover(element: Element | null): boolean;
  /** Files were dropped on `element`: take them and return true, or return false. Never both this and a project import. */
  drop(element: Element | null, paths: string[]): boolean;
  /** The drag left, ended, or belongs to another target: remove the highlight. */
  clear(): void;
}

const zones: NativeDropZone[] = [];

/** The registered zones, in registration order (asked before the Files view). */
export function nativeDropZones(): readonly NativeDropZone[] {
  return zones;
}

/** Register a zone; returns its removal. */
export function registerNativeDropZone(zone: NativeDropZone): () => void {
  zones.push(zone);
  return () => {
    const index = zones.indexOf(zone);
    if (index >= 0) zones.splice(index, 1);
  };
}
