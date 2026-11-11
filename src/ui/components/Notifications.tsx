import { CircleCheck, CircleX, Info, TriangleAlert, X, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { dismiss, useNotifications, type Notification, type Tone } from "../../app/notifications";
import { Button, IconButton } from "./Button";

const toneIcons: Record<Tone, LucideIcon> = {
  info: Info,
  success: CircleCheck,
  warning: TriangleAlert,
  danger: CircleX,
};

/** Default auto-dismiss per tone; danger stays until dismissed. */
export const defaultTimeouts: Record<Tone, number | null> = {
  info: 6000,
  success: 5000,
  warning: 10000,
  danger: null,
};

function timeoutFor(n: Notification): number | null {
  return n.timeoutMs === undefined ? defaultTimeouts[n.tone] : n.timeoutMs;
}

function NotificationItem({ notification }: { notification: Notification }) {
  const { id, tone, title, body, actions } = notification;
  const Icon = toneIcons[tone];
  const [paused, setPaused] = useState(false);
  const timeout = timeoutFor(notification);

  useEffect(() => {
    if (timeout === null || paused) return;
    const timer = setTimeout(() => dismiss(id), timeout);
    return () => clearTimeout(timer);
  }, [id, timeout, paused, notification]);

  return (
    <li
      className="ui-notification"
      data-tone={tone}
      data-testid="notification"
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocus={() => setPaused(true)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setPaused(false);
      }}
    >
      <Icon className="ui-notification__icon" size={16} aria-hidden />
      <div className="ui-notification__content">
        <p className="ui-notification__title">{title}</p>
        {body && <p className="ui-notification__body">{body}</p>}
        {actions && actions.length > 0 && (
          <div className="ui-notification__actions">
            {actions.map((action, index) => (
              <Button
                key={action.label}
                size="sm"
                variant={index === 0 ? "secondary" : "ghost"}
                onPress={async () => {
                  dismiss(id);
                  await action.run();
                }}
              >
                {action.label}
              </Button>
            ))}
          </div>
        )}
      </div>
      <IconButton
        icon={X}
        size="sm"
        aria-label={`Dismiss notification: ${title}`}
        tooltip={false}
        onPress={() => dismiss(id)}
        className="ui-notification__dismiss"
      />
    </li>
  );
}

function announcement(n: Notification): string {
  return n.body ? `${n.title}. ${n.body}` : n.title;
}

/**
 * Renders the notification queue (`src/app/notifications.ts`) as a stack in
 * the bottom-right corner. New items are announced through persistent live
 * regions (assertive for danger, polite otherwise); the visible list is a
 * labelled landmark reachable with F6 and Tab.
 */
export function NotificationRegion() {
  const items = useNotifications((s) => s.items);
  const [polite, setPolite] = useState("");
  const [assertive, setAssertive] = useState("");
  const announced = useRef(new Map<string, Notification>());

  useEffect(() => {
    for (const item of items) {
      if (announced.current.get(item.id) === item) continue;
      announced.current.set(item.id, item);
      if (item.tone === "danger") setAssertive(announcement(item));
      else setPolite(announcement(item));
    }
    for (const id of [...announced.current.keys()]) {
      if (!items.some((n) => n.id === id)) announced.current.delete(id);
    }
  }, [items]);

  return (
    <>
      <div className="ui-visually-hidden" aria-live="polite" aria-atomic="true" data-testid="live-polite">
        {polite}
      </div>
      <div className="ui-visually-hidden" aria-live="assertive" aria-atomic="true" data-testid="live-assertive">
        {assertive}
      </div>
      {items.length > 0 && (
        <section className="ui-notifications" aria-label="Notifications" data-region="notifications" tabIndex={-1}>
          <ol className="ui-notifications__list">
            {items.map((n) => (
              <NotificationItem key={n.id} notification={n} />
            ))}
          </ol>
        </section>
      )}
    </>
  );
}
