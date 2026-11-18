import { CircleAlert, TriangleAlert } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import {
  Dialog as AriaDialog,
  DialogTrigger,
  Heading,
  Modal,
  ModalOverlay,
  PopoverContext,
  useSlottedContext,
  type ModalOverlayProps,
} from "react-aria-components";
import { create } from "zustand";

import { Button, type ButtonVariant } from "./Button";
import { cx } from "./cx";
import { Popover, type PopoverProps } from "./Menu";

export { DialogTrigger };

export interface DialogProps {
  title: string;
  children: ReactNode;
  /** Footer buttons; the primary action last. Use `slot="close"` on a Button to dismiss. */
  footer?: ReactNode;
  /** `alertdialog` for interruptions that need a decision (unsaved changes, destructive actions). */
  role?: "dialog" | "alertdialog";
  /** `xl`: the content reading width (long reviews such as a workflow plan). */
  size?: "sm" | "md" | "lg" | "xl";
  /**
   * `message` (default): a short message in muted 14px prose. `content`: the
   * body is the subject itself (a plan, a form, a review) in 13px text color.
   */
  body?: "message" | "content";
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
  body = "message",
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
        <AriaDialog className={cx("ui-dialog", className)} role={role} data-body={body}>
          <Heading slot="title" className="ui-dialog__title">
            {title}
          </Heading>
          <DialogBody>{children}</DialogBody>
          {footer && <div className="ui-dialog__footer">{footer}</div>}
        </AriaDialog>
      </Modal>
    </ModalOverlay>
  );
}

/**
 * Scrollable dialog body. When its content overflows, the body gets
 * `data-overflow` and the footer below it shows a divider, so long dialogs
 * (plan review, impact decisions) keep their actions visible and separated.
 */
function DialogBody({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [overflow, setOverflow] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const update = () => setOverflow(element.scrollHeight > element.clientHeight + 1);
    const observer = new ResizeObserver(update);
    observer.observe(element);
    for (const child of Array.from(element.children)) observer.observe(child);
    update();
    return () => observer.disconnect();
  }, []);
  return (
    <div ref={ref} className="ui-dialog__body" data-overflow={overflow || undefined}>
      {children}
    </div>
  );
}

export interface PopoverDialogProps {
  /** Accessible name (and visible heading unless `hideTitle`). */
  title: string;
  hideTitle?: boolean;
  children: ReactNode;
  placement?: PopoverProps["placement"];
  className?: string;
  /**
   * Element to anchor to when the popover is not inside a `DialogTrigger`
   * (open it with `isOpen`/`onOpenChange`), e.g. a composer the popover opens above.
   */
  triggerRef?: RefObject<Element | null>;
  /** Controlled open state; omit inside a `DialogTrigger`, which controls it. */
  isOpen?: boolean;
  onOpenChange?: (isOpen: boolean) => void;
  /** `anchor`: exactly the anchor's width (fits a narrow panel). Default: the shared popover width. */
  width?: "default" | "anchor";
  /**
   * Keep the popover inside this element (a panel body): it opens on the
   * `placement` side without flipping, and its content scrolls within the
   * space between the anchor and the element's edge.
   */
  boundaryElement?: Element | null;
  /** Upper bound for the content height in px (scrolls beyond it). */
  maxHeight?: number;
  /** Gap between anchor and popover in px (default 4). */
  offset?: number;
}

/** Space kept between a bounded popover and its boundary's edge (--space-2). */
const BOUNDARY_PADDING = 8;
const DEFAULT_OFFSET = 4;

/**
 * Non-modal dialog in a popover (filters, small pickers, sheets anchored to an
 * element). Place inside `DialogTrigger` after its Button trigger, or pass
 * `triggerRef` with `isOpen`/`onOpenChange`. Escape or clicking outside closes
 * it and returns focus to the trigger.
 */
export function PopoverDialog({
  title,
  hideTitle,
  children,
  placement = "bottom start",
  className,
  triggerRef,
  isOpen,
  onOpenChange,
  width = "default",
  boundaryElement,
  maxHeight,
  offset = DEFAULT_OFFSET,
}: PopoverDialogProps) {
  // Inside a DialogTrigger the anchor comes from the trigger context.
  const context = useSlottedContext(PopoverContext);
  const anchorRef = triggerRef ?? context?.triggerRef;
  return (
    <Popover
      placement={placement}
      offset={offset}
      {...(triggerRef ? { triggerRef } : {})}
      {...(isOpen !== undefined ? { isOpen, onOpenChange } : {})}
      {...(boundaryElement ? { boundaryElement, shouldFlip: false, containerPadding: BOUNDARY_PADDING } : {})}
      className={cx("ui-popover-dialog", className)}
      data-width={width}
    >
      <PopoverDialogContent
        title={title}
        hideTitle={hideTitle}
        anchorRef={anchorRef}
        boundaryElement={boundaryElement}
        maxHeight={maxHeight}
        side={placement.split(" ")[0]!}
        offset={offset}
      >
        {children}
      </PopoverDialogContent>
    </Popover>
  );
}

interface PopoverDialogContentProps {
  title: string;
  hideTitle?: boolean;
  children: ReactNode;
  anchorRef?: RefObject<Element | null>;
  boundaryElement?: Element | null;
  maxHeight?: number;
  side: string;
  offset: number;
}

/**
 * The dialog inside the popover. react-aria writes its own inline max-height
 * on the popover element, so the boundary cap goes on the dialog, measured
 * while open from the anchor and the boundary.
 */
function PopoverDialogContent({ title, hideTitle, children, anchorRef, boundaryElement, maxHeight, side, offset }: PopoverDialogContentProps) {
  const dialogRef = useRef<HTMLElement | null>(null);
  const [bounded, setBounded] = useState<number | undefined>(undefined);
  useLayoutEffect(() => {
    if (!boundaryElement) {
      setBounded(undefined);
      return;
    }
    const measure = () => {
      const dialog = dialogRef.current;
      const anchor = anchorRef?.current;
      if (!dialog || !anchor) return;
      const box = boundaryElement.getBoundingClientRect();
      const at = anchor.getBoundingClientRect();
      const space =
        side === "top" ? at.top - box.top : side === "bottom" ? box.bottom - at.bottom : box.height - BOUNDARY_PADDING;
      // The popover's own padding and border sit around the dialog.
      const popover = dialog.parentElement;
      let chrome = 0;
      if (popover) {
        const style = getComputedStyle(popover);
        chrome = ["paddingTop", "paddingBottom", "borderTopWidth", "borderBottomWidth"].reduce(
          (sum, key) => sum + (parseFloat(style[key as "paddingTop"]) || 0),
          0,
        );
      }
      const next = Math.max(0, Math.floor(space - offset - BOUNDARY_PADDING - chrome));
      setBounded((prev) => (prev === next ? prev : next));
    };
    measure();
    window.addEventListener("resize", measure);
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure);
    observer?.observe(boundaryElement);
    if (anchorRef?.current) observer?.observe(anchorRef.current);
    return () => {
      window.removeEventListener("resize", measure);
      observer?.disconnect();
    };
  }, [anchorRef, boundaryElement, side, offset]);
  const cap = bounded === undefined ? maxHeight : maxHeight === undefined ? bounded : Math.min(bounded, maxHeight);
  return (
    <AriaDialog
      ref={dialogRef}
      className="ui-popover-dialog__dialog"
      aria-label={hideTitle ? title : undefined}
      data-bounded={cap !== undefined || undefined}
      style={cap !== undefined ? { maxHeight: cap } : undefined}
    >
      {!hideTitle && (
        <Heading slot="title" className="ui-popover-dialog__title">
          {title}
        </Heading>
      )}
      {children}
    </AriaDialog>
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
  /** Dialog width (default `sm`); `md` for decisions that list several consequences. */
  size?: "sm" | "md" | "lg";
  /** `data-testid` of the alert dialog. */
  testId?: string;
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
      <Modal className="ui-modal" data-size={options.size ?? "sm"}>
        <AriaDialog className="ui-dialog" role="alertdialog" data-tone={options.tone} data-testid={options.testId}>
          <div className="ui-dialog__alert-header">
            {options.tone && <Icon className="ui-dialog__tone-icon" size={20} aria-hidden />}
            <Heading slot="title" className="ui-dialog__title">
              {options.title}
            </Heading>
          </div>
          <DialogBody>{options.message}</DialogBody>
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
