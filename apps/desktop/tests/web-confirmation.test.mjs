import assert from 'node:assert/strict';
import test from 'node:test';

import {
  enqueueWebConfirmation,
  isWebConfirmationRequest,
  listenForWebConfirmations,
  removeWebConfirmation,
} from '../src/features/web-confirmation.ts';

const request = (id = `confirmation/${'0a'.repeat(32)}`) => ({
  schema: 'hiroute.web-confirmation/v1',
  confirmation_id: id,
  title: 'HiRoute · Confirm change',
  message: 'Apply the verified change.',
  confirm_label: 'Apply change',
  cancel_label: 'Cancel',
});

test('only the bounded typed backend confirmation event enters the queue', () => {
  assert.equal(isWebConfirmationRequest(request()), true);
  assert.equal(isWebConfirmationRequest({ ...request(), schema: 'other' }), false);
  assert.equal(isWebConfirmationRequest({ ...request(), accepted: true }), false);
  assert.equal(isWebConfirmationRequest({ ...request(), confirmation_id: 'confirmation/1' }), false);
  assert.equal(isWebConfirmationRequest({ ...request(), message: '' }), false);
  assert.equal(isWebConfirmationRequest({ ...request(), message: 'x'.repeat(16_385) }), false);
});

test('confirmation delivery is ordered, de-duplicated and removed by exact id', () => {
  const first = request();
  const second = request(`confirmation/${'0b'.repeat(32)}`);
  const queued = enqueueWebConfirmation(enqueueWebConfirmation([], first), second);
  assert.deepEqual(enqueueWebConfirmation(queued, first), queued);
  assert.deepEqual(removeWebConfirmation(queued, first.confirmation_id), [second]);
  assert.deepEqual(removeWebConfirmation(queued, 'confirmation/unknown'), queued);
});

test('confirmation listener binds through the supplied WebView event source', async () => {
  const received = [];
  let listenedEvent = null;
  const unlisten = () => {};
  const source = {
    async listen(event, handler) {
      listenedEvent = event;
      handler({ payload: request() });
      handler({ payload: { ...request(), schema: 'other' } });
      return unlisten;
    },
  };

  assert.equal(await listenForWebConfirmations(source, value => received.push(value)), unlisten);
  assert.equal(listenedEvent, 'hiroute-web-confirmation');
  assert.deepEqual(received, [request()]);
});
