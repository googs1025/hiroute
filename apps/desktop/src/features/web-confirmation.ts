export const WEB_CONFIRMATION_EVENT = 'hiroute-web-confirmation';
export const WEB_CONFIRMATION_SCHEMA = 'hiroute.web-confirmation/v1';

export type WebConfirmationRequest = {
  schema: typeof WEB_CONFIRMATION_SCHEMA;
  confirmation_id: string;
  title: string;
  message: string;
  confirm_label: string;
  cancel_label: string;
};

export type WebConfirmationEventSource = {
  listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void>;
};

function boundedString(value: unknown, maximum: number): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

function confirmationId(value: unknown): value is string {
  return typeof value === 'string' && /^confirmation\/[0-9a-f]{64}$/.test(value);
}

export function isWebConfirmationRequest(value: unknown): value is WebConfirmationRequest {
  if (!value || typeof value !== 'object') return false;
  const request = value as Record<string, unknown>;
  const fields = ['schema', 'confirmation_id', 'title', 'message', 'confirm_label', 'cancel_label'];
  return Object.keys(request).length === fields.length
    && fields.every(field => Object.hasOwn(request, field))
    && request.schema === WEB_CONFIRMATION_SCHEMA
    && confirmationId(request.confirmation_id)
    && boundedString(request.title, 256)
    && boundedString(request.message, 16_384)
    && boundedString(request.confirm_label, 256)
    && boundedString(request.cancel_label, 256);
}

export function listenForWebConfirmations(
  source: WebConfirmationEventSource,
  onRequest: (request: WebConfirmationRequest) => void,
): Promise<() => void> {
  return source.listen<unknown>(WEB_CONFIRMATION_EVENT, event => {
    if (isWebConfirmationRequest(event.payload)) onRequest(event.payload);
  });
}

export function enqueueWebConfirmation(
  current: WebConfirmationRequest[],
  request: WebConfirmationRequest,
): WebConfirmationRequest[] {
  return current.some(item => item.confirmation_id === request.confirmation_id)
    ? current
    : [...current, request];
}

export function removeWebConfirmation(
  current: WebConfirmationRequest[],
  confirmationId: string,
): WebConfirmationRequest[] {
  return current.filter(item => item.confirmation_id !== confirmationId);
}
