import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';

let listener: (() => void) | undefined;
let resolveListen: ((stop: () => void) => void) | undefined;
let delayListen = false;
const requests: Array<(value: unknown) => void> = [];
const commands: string[] = [];
let stopCount = 0;
const intervals = new Map<number, () => void>();
Object.assign(globalThis, { React, window: {
  setInterval: (callback: () => void) => { intervals.set(1, callback); return 1; },
  clearInterval: (id: number) => intervals.delete(id),
} });
mock.module('@tauri-apps/api/core', () => ({ invoke: (command: string) => {
  commands.push(command);
  return new Promise(resolve => requests.push(resolve));
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async (_event: string, callback: () => void) => {
  listener = callback;
  if (delayListen) return new Promise<() => void>(resolve => { resolveListen = resolve; });
  return () => { stopCount++; };
} }));
const { useCalendar } = await import('../../src/hooks/useCalendar');
let current: ReturnType<typeof useCalendar>;
let root: ReactTestRenderer | undefined;
function Probe() { current = useCalendar(); return null; }
async function mount() { await act(async () => { root = create(<Probe />); }); }
beforeEach(() => { listener = undefined; resolveListen = undefined; delayListen = false; requests.length = 0; commands.length = 0; stopCount = 0; intervals.clear(); });
afterEach(async () => { await act(async () => root?.unmount()); root = undefined; expect(intervals.size).toBe(0); });

test('mount and updates read only the native cache, never grant access or query a provider', async () => {
  await mount();
  await act(async () => requests[0]({ events: [] }));
  expect(commands).toEqual(['calendar_snapshot']);
  await act(async () => listener!());
  expect(commands).toEqual(['calendar_snapshot', 'calendar_snapshot']);
});

test('a slow earlier snapshot cannot overwrite newer calendar data', async () => {
  await mount();
  await act(async () => listener!());
  await act(async () => requests[1]({ events: [{ title: 'Updated event' }] }));
  await act(async () => requests[0]({ events: [{ title: 'Old event' }] }));
  expect(current.snapshot?.events[0].title).toBe('Updated event');
});

test('a listener that arrives after navigation is released', async () => {
  delayListen = true;
  await mount();
  await act(async () => root?.unmount()); root = undefined;
  await act(async () => resolveListen!(() => { stopCount++; }));
  expect(stopCount).toBe(1);
});
