// Notification contract. Features call `notify`; the workbench renders the
// queue in an accessible live region with the shared status treatments.
import { create } from "zustand";

export type Tone = "info" | "success" | "warning" | "danger";

export interface NotificationAction {
  label: string;
  run: () => void | Promise<void>;
}

export interface Notification {
  id: string;
  tone: Tone;
  title: string;
  body?: string;
  actions?: NotificationAction[];
  /** Auto-dismiss delay; `null` keeps the notification until dismissed. Defaults by tone. */
  timeoutMs?: number | null;
}

export const useNotifications = create<{ items: Notification[] }>(() => ({ items: [] }));

let counter = 0;

/** Show (or replace, when `id` matches) a notification. Returns its id. */
export function notify(notification: Omit<Notification, "id"> & { id?: string }): string {
  const id = notification.id ?? `n-${++counter}`;
  const item: Notification = { ...notification, id };
  useNotifications.setState((state) => ({
    items: [...state.items.filter((n) => n.id !== id), item],
  }));
  return id;
}

export function dismiss(id: string): void {
  useNotifications.setState((state) => ({ items: state.items.filter((n) => n.id !== id) }));
}
