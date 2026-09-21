const ENVIRONMENT_OPEN = '<environment_context>';
const ENVIRONMENT_CLOSE = '</environment_context>';

export function isInjectedEnvironmentContext(value: string): boolean {
  const text = value.trim();
  if (!text.startsWith(ENVIRONMENT_OPEN)) return false;
  const close = text.indexOf(ENVIRONMENT_CLOSE, ENVIRONMENT_OPEN.length);
  return close >= 0 && close + ENVIRONMENT_CLOSE.length === text.length;
}

export function firstRealUserText(values: readonly string[]): string | null {
  for (const value of values) {
    if (!value.trim() || isInjectedEnvironmentContext(value)) continue;
    return value;
  }
  return null;
}
