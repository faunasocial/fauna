import { createConnectEdge } from './connectEdge.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${actual}, want ${expected}`);
  }
}

Deno.test('connectEdge — fires on the first connect and on each later reconnect', () => {
  const fire = createConnectEdge();
  eq(fire('aa', 'connecting'), false, 'not connected yet');
  eq(fire('aa', 'connected'), true, 'first connect');
  eq(fire('aa', 'connected'), false, 'effect re-run while still connected');
  eq(fire('aa', 'disconnected'), false, 'socket dropped');
  eq(fire('aa', 'connected'), true, 'reconnect fires again');
});

Deno.test('connectEdge — sign-out re-arms, identity change fires', () => {
  const fire = createConnectEdge();
  eq(fire('aa', 'connected'), true, 'first connect');
  eq(fire(null, 'connected'), false, 'signed out');
  eq(fire('aa', 'connected'), true, 'signed back in');
  eq(fire('bb', 'connected'), true, 'identity change while connected');
});
