import { UiIcon } from './UiIcon';
import { useEffect, useId, useRef, useState } from 'react';

const focusableSelector = [
  'button:not([disabled])',
  '[href]',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  'summary',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

let openDialogCount = 0;
let savedBodyOverflow = '';

export function Dialog({
  open,
  title,
  description,
  closeLabel,
  closeDisabled = false,
  onClose,
  children,
  footer,
  variant = 'modal',
}: {
  open: boolean;
  title: string;
  description?: string;
  closeLabel: string;
  closeDisabled?: boolean;
  onClose: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  variant?: 'modal' | 'drawer';
}) {
  const panel = useRef<HTMLElement>(null);
  const returnTarget = useRef<HTMLElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    if (openDialogCount === 0) {
      savedBodyOverflow = document.body.style.overflow;
      document.body.style.overflow = 'hidden';
    }
    openDialogCount += 1;
    returnTarget.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const backgrounds: { element: HTMLElement; inert: boolean }[] = [];
    let branch = panel.current?.parentElement;
    while (branch?.parentElement && branch !== document.body) {
      for (const sibling of Array.from(branch.parentElement.children)) {
        if (sibling !== branch && sibling instanceof HTMLElement) {
          backgrounds.push({ element: sibling, inert: sibling.inert });
          sibling.inert = true;
        }
      }
      branch = branch.parentElement;
    }
    const frame = requestAnimationFrame(() => {
      const first = panel.current?.querySelector<HTMLElement>('[data-autofocus]')
        ?? Array.from(panel.current?.querySelector<HTMLElement>('.modal-body')?.querySelectorAll<HTMLElement>(focusableSelector) ?? []).find(element => element.getClientRects().length > 0)
        ?? Array.from(panel.current?.querySelector<HTMLElement>('.modal-foot')?.querySelectorAll<HTMLElement>(focusableSelector) ?? []).find(element => element.getClientRects().length > 0)
        ?? Array.from(panel.current?.querySelectorAll<HTMLElement>(focusableSelector) ?? []).find(element => element.getClientRects().length > 0);
      (first ?? panel.current)?.focus();
    });
    return () => {
      cancelAnimationFrame(frame);
      backgrounds.forEach(({ element, inert }) => { element.inert = inert; });
      openDialogCount = Math.max(0, openDialogCount - 1);
      if (openDialogCount === 0) document.body.style.overflow = savedBodyOverflow;
      const target = returnTarget.current;
      requestAnimationFrame(() => {
        if (target?.isConnected && !target.matches(':disabled')) target.focus();
      });
    };
  }, [open]);

  if (!open) return null;
  return (
    <div
      className={`modal-backdrop${variant === 'drawer' ? ' oc-dialog-drawer' : ''}`}
      onMouseDown={event => {
        if (!closeDisabled && event.target === event.currentTarget) onClose();
      }}
    >
      <section
        ref={panel}
        className={`modal${variant === 'drawer' ? ' oc-drawer-panel' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? `${titleId}-description` : undefined}
        tabIndex={-1}
        onKeyDown={event => {
          if (event.key === 'Escape') {
            event.preventDefault();
            if (!closeDisabled) onClose();
            return;
          }
          if (event.key !== 'Tab') return;
          const focusable = Array.from(panel.current?.querySelectorAll<HTMLElement>(focusableSelector) ?? []).filter(element => element.getClientRects().length > 0);
          if (!focusable.length) {
            event.preventDefault();
            panel.current?.focus();
            return;
          }
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <header className="modal-head">
          <div>
            <h2 id={titleId}>{title}</h2>
            {description && <p id={`${titleId}-description`}>{description}</p>}
          </div>
          <button className="icon-btn" type="button" aria-label={closeLabel} title={closeLabel} disabled={closeDisabled} onClick={onClose}><UiIcon name="close" /></button>
        </header>
        <div className="modal-body">{children}</div>
        {footer && <footer className="modal-foot">{footer}</footer>}
      </section>
    </div>
  );
}

export function useDiscardGuard(dirty: boolean, close: () => void) {
  const [confirmationOpen, setConfirmationOpen] = useState(false);
  return {
    confirmationOpen,
    requestClose: () => {
      if (!dirty) close();
      else setConfirmationOpen(true);
    },
    keepEditing: () => setConfirmationOpen(false),
    discard: () => {
      setConfirmationOpen(false);
      close();
    },
  };
}
