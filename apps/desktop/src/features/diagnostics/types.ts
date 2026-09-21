export type DiagnosticLevel = 'error' | 'warn' | 'info' | 'debug';

export type DiagnosticsSettingsView = {
  revision: number;
  level: DiagnosticLevel;
  error?: string;
};

/**
 * What the native side reports: this process's own last settings read and whether the local
 * log directory can be opened. There is no process state and no other process's applied
 * revision; a running process adopts a saved revision on its own watch.
 */
export type DiagnosticsStatus = {
  schema: string;
  settings: DiagnosticsSettingsView;
  logs_directory_available: boolean;
};

export const DIAGNOSTIC_LEVELS: DiagnosticLevel[] = ['error', 'warn', 'info', 'debug'];
/** The page refreshes its view at this cadence while it is the active page. */
export const DIAGNOSTICS_POLL_MS = 2000;
