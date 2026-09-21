import { useCallback, useEffect, useRef, useState } from 'react';
import { UiIcon } from './UiIcon';

export type ToastNotice = {
  key: number;
  message: string;
  tone: 'success' | 'error';
};

export function useTransientToast(durationMs = 2400) {
  const [toast, setToast] = useState<ToastNotice | null>(null);
  const timer = useRef<number | null>(null);
  const sequence = useRef(0);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const showToast = useCallback((message: string, tone: ToastNotice['tone'] = 'success') => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    sequence.current += 1;
    setToast({ key: sequence.current, message, tone });
    timer.current = window.setTimeout(() => {
      setToast(null);
      timer.current = null;
    }, durationMs);
  }, [durationMs]);

  return { toast, showToast };
}

export function Toast({ notice }: { notice: ToastNotice | null }) {
  if (!notice) return null;
  return <div className="toast-stack" aria-live={notice.tone === 'error' ? 'assertive' : 'polite'}>
    <div key={notice.key} className={`toast${notice.tone === 'error' ? ' error' : ''}`} data-tone={notice.tone} role={notice.tone === 'error' ? 'alert' : 'status'}>
      <UiIcon name={notice.tone === 'error' ? 'warning' : 'check'} />
      <span>{notice.message}</span>
    </div>
  </div>;
}

export async function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const input = document.createElement('textarea');
  input.value = value;
  input.setAttribute('readonly', '');
  input.style.position = 'fixed';
  input.style.opacity = '0';
  document.body.append(input);
  input.select();
  const copied = document.execCommand('copy');
  input.remove();
  if (!copied) throw new Error('CLIPBOARD_UNAVAILABLE');
}
