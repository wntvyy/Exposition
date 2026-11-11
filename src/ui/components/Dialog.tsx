import { CircleAlert, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import {
  Dialog as AriaDialog,
  DialogTrigger,
  Heading,
  Modal,
  ModalOverlay,
  type ModalOverlayProps,
} from "react-aria-components";
import { create } from "zustand";

import { Button, type ButtonVariant } from "./Button";
import { cx } from "./cx";

export { DialogTrigger };

export interface DialogProps {
  title: string;
  children: ReactNode;
  /** Footer buttons; the primary action last. Use `slot="close"` on a Button to dismiss. */
  footer?: ReactNode;
  /** `alertdialog` for interruptions that need a decision (unsaved changes, destructive actions). */
  role?: "dialog" | "alertdialog";
  size?: "sm" | "md" | "lg";
  /** Dismiss by clicking the backdrop (default true for `dialog`, false for `alertdialog`). */
  isDismissable?: boolean;
  isOpen?: ModalOverlayProps["isOpen"];
  onOpenChange?: ModalOverlayProps["onOpenChange"];
  className?: string;
}

/**
 * Modal dialog: focus is trapped and restored on close, Escape dismisses.
 * Use inside `DialogTrigger` (uncontrolled) or with `isOpen`/`onOpenChange`.
 */
export function Dialog({
  title,
  children,
  footer,
  role = "dialog",
  size = "md",
  isDismissable,
  isOpen,
  onOpenChange,
  className,
}: DialogProps) {
  return (
    <ModalOverlay
      className="ui-modal-overlay"
      isDismissable={isDismissable ?? role === "dialog"}
      isKeyboardDismissDisabled={false}
      isOpen={isOpen}
      onOpenChange={onOpenChange}
    >
      <Modal className="ui-modal" data-size={size}>
        <AriaDialog className={cx("ui-dialog", className)} role={role}>
          <Heading slot="title" className="ui-dialog__title">
            {title}
          </Heading>
          <div className="ui-dialog__body">{children}</div>
          {footer && <div className="ui-dialog__footer">{footer}</div>}
        </AriaDialog>
      </Modal>
    </ModalOverlay>
  );
}

/** Dialog with `alertdialog` semantics for decisions that interrupt the task; not dismissed by clicking outside. */
export function AlertDialog(props: Omit<DialogProps, "role">) {
  return <Dialog {...props} role="alertdialog" />;
}

export interface ConfirmAction<A extends string> {
  id: A;
  label: string;
  variant?: ButtonVariant;
}

export interface ConfirmOptions<A extends string> {
  title: string;
  message: ReactNode;
  /** Buttons in display order (cancel first, primary last). */
  actions: ConfirmAction<A>[];
  /** Result when dismissed with Escape. */
  cancelId: A;
  /** Action focused on open; defaults to the last action. Avoid defaulting to destructive actions. */
  defaultId?: A;
  tone?: "warning" | "danger";
}

interface PendingConfirm {
  id: number;
  options: ConfirmOptions<string>;
  resolve: (result: string) => void;
}

const useConfirmQueue = create<{ queue: PendingConfirm[] }>(() => ({ queue: [] }));
let confirmCounter = 0;

/**
 * Ask the user to choose an action in an alert dialog. Resolves with the
 * chosen action id (or `cancelId` on Escape). Requires `<ConfirmHost />`
 * mounted once (the workbench does this).
 */
export function confirm<A extends string>(options: ConfirmOptions<A>): Promise<A> {
  return new Promise<A>((resolve) => {
    const entry: PendingConfirm = {
      id: ++confirmCounter,
      options: options as ConfirmOptions<string>,
      resolve: (result) => resolve(result as A),
    };
    useConfirmQueue.setState((s) => ({ queue: [...s.queue, entry] }));
  });
}

export type UnsavedChoice = "save" | "discard" | "cancel";

/** Standard unsaved-changes prompt for close guards: Save / Don't Save / Cancel. */
export function confirmUnsaved(name: string): Promise<UnsavedChoice> {
  return confirm<UnsavedChoice>({
    title: `Save changes to ${name}?`,
    message: "Your changes will be lost if you don't save them.",
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "discard", label: "Don't Save", variant: "secondary" },
      { id: "save", label: "Save", variant: "primary" },
    ],
    cancelId: "cancel",
    defaultId: "save",
    tone: "warning",
  });
}

/** Renders queued `confirm()` dialogs one at a time. Mount once near the app root. */
export function ConfirmHost() {
  const current = useConfirmQueue((s) => s.queue[0]);
  if (!current) return null;
  const { options } = current;
  const finish = (result: string) => {
    useConfirmQueue.setState((s) => ({ queue: s.queue.filter((q) => q.id !== current.id) }));
    current.resolve(result);
  };
  const defaultId = options.defaultId ?? options.actions.at(-1)?.id;
  const Icon = options.tone === "danger" ? CircleAlert : TriangleAlert;
  return (
    <ModalOverlay
      key={current.id}
      className="ui-modal-overlay"
      isOpen
      isDismissable={false}
      onOpenChange={(open) => {
        if (!open) finish(options.cancelId);
      }}
    >
      <Modal className="ui-modal" data-size="sm">
        <AriaDialog className="ui-dialog" role="alertdialog" data-tone={options.tone}>
          <div className="ui-dialog__alert-header">
            {options.tone && <Icon className="ui-dialog__tone-icon" size={20} aria-hidden />}
            <Heading slot="title" className="ui-dialog__title">
              {options.title}
            </Heading>
          </div>
          <div className="ui-dialog__body">{options.message}</div>
          <div className="ui-dialog__footer">
            {options.actions.map((action) => (
              <Button
                key={action.id}
                variant={action.variant ?? "secondary"}
                autoFocus={action.id === defaultId}
                onPress={() => finish(action.id)}
              >
                {action.label}
              </Button>
            ))}
          </div>
        </AriaDialog>
      </Modal>
    </ModalOverlay>
  );
}
