const upperCode = /\b[A-Z][A-Z0-9_]{2,80}\b/u;
const dottedCode = /\b[a-z][a-z0-9_]*(?:\.[a-z0-9_]+){1,5}\b/u;

/** Extract a bounded diagnostic code without ever returning a wire payload or raw exception. */
export function safeDiagnosticCode(error: unknown, fallback: string): string {
  const pending: unknown[] = [error];
  const visited = new Set<object>();
  while (pending.length > 0 && visited.size < 12) {
    const current = pending.shift();
    if (typeof current === 'string') {
      const value = current.slice(0, 4096);
      const match = value.match(upperCode)?.[0] ?? value.match(dottedCode)?.[0];
      if (match) return match;
      continue;
    }
    if (!current || typeof current !== 'object' || visited.has(current)) continue;
    visited.add(current);
    const value = current as Record<string, unknown>;
    pending.unshift(value.message_key, value.code);
    pending.push(value.error, value.failure, value.envelope);
  }
  return fallback;
}
