// Outline of the active document: a transient projection of its current
// text (the live buffer, unsaved edits included) computed by the backend's
// source extractors. Nothing is stored; the outline is dropped when the
// document, its path, or the project changes, and answers to requests that
// were overtaken (a newer request, another document or project) are ignored.
import type { DocumentOutline } from "../../bindings/DocumentOutline";
import { commands, errorMessage } from "../../lib/ipc";
import { acceptAnswer, sameTarget, type OutlineTarget } from "./model";

export interface OutlineState {
  target: OutlineTarget | null;
  outline: DocumentOutline | null;
  loading: boolean;
  error: string | null;
}

type Fetch = (documentId: string) => Promise<DocumentOutline>;

const empty: OutlineState = { target: null, outline: null, loading: false, error: null };

export class OutlineStore {
  private state: OutlineState = empty;
  private seq = 0;
  private listeners = new Set<() => void>();

  constructor(private readonly fetch: Fetch = (id) => commands.documentOutline(id)) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getState = (): OutlineState => this.state;

  private set(patch: Partial<OutlineState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  /**
   * Follow another document (or none). A different document, path, or project
   * drops the shown outline at once and invalidates requests in flight.
   */
  setTarget(target: OutlineTarget | null): void {
    if (target === null ? this.state.target === null : sameTarget(target, this.state.target)) return;
    this.seq += 1;
    this.state = { ...empty, target };
    for (const listener of this.listeners) listener();
  }

  /**
   * Request the outline of the target's current text. Resolves with the
   * accepted outline, or null when the answer was overtaken or failed.
   */
  async refresh(): Promise<DocumentOutline | null> {
    const target = this.state.target;
    if (!target) return null;
    const seq = ++this.seq;
    this.set({ loading: true });
    try {
      const answer = await this.fetch(target.documentId);
      if (!acceptAnswer(answer, { seq, target }, { seq: this.seq, target: this.state.target, shown: this.state.outline })) {
        if (seq === this.seq) this.set({ loading: false });
        return null;
      }
      this.set({ outline: answer, loading: false, error: null });
      return answer;
    } catch (error) {
      if (seq === this.seq && sameTarget(target, this.state.target)) this.set({ loading: false, error: errorMessage(error) });
      return null;
    }
  }
}

/** The one outline store of the Editor's Outline view. */
export const outlineStore = new OutlineStore();
