import { useSyncExternalStore } from "react";

import { registry } from "../registry";

/** Re-render when contributions are registered or removed. Returns the registry version. */
export function useRegistryVersion(): number {
  return useSyncExternalStore(registry.subscribe, registry.getVersion, registry.getVersion);
}
