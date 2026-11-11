// Local persistence for UI preferences. Storage can be unavailable (blocked,
// full, or corrupted profile); every access is guarded and callers supply safe
// defaults. Never store document content or secrets here.

export interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function defaultStorage(): KeyValueStorage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function readStored(key: string, storage: KeyValueStorage | null = defaultStorage()): string | null {
  try {
    return storage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

/** Returns false when the value could not be written. */
export function writeStored(key: string, value: string, storage: KeyValueStorage | null = defaultStorage()): boolean {
  try {
    if (!storage) return false;
    storage.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

/** Parse JSON without throwing; `undefined` when absent or malformed. */
export function parseJson(raw: string | null): unknown {
  if (raw === null) return undefined;
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return undefined;
  }
}
