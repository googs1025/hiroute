/** Keep existing order (including temporarily unavailable identities), append newly chosen IDs. */
export function reconcileSelection(previous: readonly string[], selected: readonly string[]): string[] {
  const retained = new Set(selected);
  return [...new Set([...previous.filter(id => retained.has(id)), ...selected])];
}
