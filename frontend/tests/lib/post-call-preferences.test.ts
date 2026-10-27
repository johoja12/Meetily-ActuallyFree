import { beforeEach, expect, test } from 'bun:test';

const values = new Map<string, string>();
const events: string[] = [];
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { localStorage: storage, window: { localStorage: storage, dispatchEvent: (event: { type: string }) => { events.push(event.type); return true; } }, Event: class { constructor(public type: string) {} } });
const { readAutoIdentifySpeakers, writeAutoIdentifySpeakers, AUTO_IDENTIFY_CHANGED_EVENT } = await import('../../src/lib/post-call-preferences');
beforeEach(() => { values.clear(); events.length = 0; });

test('identifying speakers without asking is off until turned on, and announces changes', () => {
  expect(readAutoIdentifySpeakers()).toBe(false);
  writeAutoIdentifySpeakers(true);
  expect(readAutoIdentifySpeakers()).toBe(true);
  expect(events).toEqual([AUTO_IDENTIFY_CHANGED_EVENT]);
  writeAutoIdentifySpeakers(false);
  expect(readAutoIdentifySpeakers()).toBe(false);
  values.set('af-post-call-auto-identify', 'garbage');
  expect(readAutoIdentifySpeakers()).toBe(false);
});
