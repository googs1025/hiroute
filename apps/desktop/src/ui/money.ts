const currencySymbols: Record<string, string> = {
  CNY: '¥',
  RMB: '¥',
  USD: '$',
  EUR: '€',
  GBP: '£',
  JPY: '¥',
};

export function formatMoney(currency: string, amount: string): string {
  const normalized = currency.trim().toUpperCase();
  return `${currencySymbols[normalized] ?? `${currency} `}${amount}`;
}
