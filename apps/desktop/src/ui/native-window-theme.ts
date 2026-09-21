import { getCurrentWindow } from '@tauri-apps/api/window';
import type { ResolvedTheme, ThemePreference } from './preferences';

function isTauriRuntime(): boolean {
  return typeof window !== 'undefined'
    && '__TAURI_INTERNALS__' in (window as Window & { __TAURI_INTERNALS__?: unknown });
}

export async function syncNativeWindowTheme(
  preference: ThemePreference,
  resolvedTheme: ResolvedTheme,
): Promise<'skipped' | 'synced'> {
  if (!isTauriRuntime()) return 'skipped';
  await getCurrentWindow().setTheme(preference === 'system' ? null : resolvedTheme);
  return 'synced';
}
