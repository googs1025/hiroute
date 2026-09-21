export type GatewayListenerScope = 'local' | 'external';

// The native listener store validates IPv4 addresses. The Desktop only presents
// loopback versus network access; the CLI retains its explicit-address controls.
export function listenerScopeFromAddress(address: string): GatewayListenerScope {
  return address.startsWith('127.') ? 'local' : 'external';
}

export function listenerApplyRequest(scope: GatewayListenerScope) {
  return { scope, accept_remote_risk: scope === 'external' };
}
