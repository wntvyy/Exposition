// Coalescing of re-reads driven by event bursts. A workflow run emits several
// events per step attempt; a view that re-reads on each of them needs only
// the state after the last one.

/**
 * Wrap `task` (a re-read) so that it runs at once when idle, and at most once
 * more after the run in flight finishes however many calls arrived meanwhile.
 * The run after the last call always starts after that call, so the final
 * state is never missed and nothing waits on a timer.
 */
export function coalesced(task: () => Promise<unknown>): () => void {
  let running = false;
  let again = false;
  const run = (): void => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    let read: Promise<unknown>;
    try {
      read = task();
    } catch (error) {
      // Thrown before a promise existed: a failed read like any other. It
      // must not leave `running` set, or no later call would ever read.
      running = false;
      again = false;
      throw error;
    }
    void read
      .catch(() => undefined)
      .finally(() => {
        running = false;
        if (again) {
          again = false;
          try {
            run();
          } catch {
            // A trailing read that failed at once; the next call reads again.
          }
        }
      });
  };
  return run;
}
