import React from 'react';
import { act, create } from 'react-test-renderer';
import { expect, mock, test } from 'bun:test';

const listeners = new Map<string, Array<(event: any) => void>>();
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { React, window: {
  sessionStorage: storage, localStorage: storage,
  dispatchEvent: (event: any) => { listeners.get(event.type)?.forEach(handler => handler(event)); return true; },
  addEventListener: (type: string, handler: any) => listeners.set(type, [...(listeners.get(type) ?? []), handler]),
  removeEventListener: (type: string, handler: any) => listeners.set(type, (listeners.get(type) ?? []).filter(item => item !== handler)),
}, CustomEvent: class { constructor(public type: string, init?: { detail?: unknown }) { (this as any).detail = init?.detail; } } });
let linked: unknown = null;
const calls: string[] = [];
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args: any) => { calls.push(`${command}:${args.meetingId}`); return linked; } }));
const { useInviteAttendees } = await import('../../src/hooks/useInviteAttendees');
const { beginLiveSession } = await import('../../src/lib/live-session');

let seen: unknown[] = [];
function Probe({ meetingId, open }: { meetingId?: string; open: boolean }) { seen = useInviteAttendees(meetingId, open); return null; }

test('a saved meeting loads its link when opened and reloads when that link changes', async () => {
  linked = { attendees: [{ name: 'Jane', email: null, organizer: false }] };
  let root: any;
  await act(async () => { root = create(<Probe meetingId="m1" open={false} />); });
  expect(calls).toEqual([]);
  await act(async () => root.update(<Probe meetingId="m1" open />));
  expect(seen).toEqual([{ name: 'Jane', email: null, organizer: false }]);
  linked = null;
  await act(async () => { (globalThis as any).window.dispatchEvent(new (globalThis as any).CustomEvent('af-meeting-calendar-link', { detail: { meetingId: 'other' } })); });
  expect(calls).toEqual(['get_meeting_calendar_event:m1']);
  await act(async () => { (globalThis as any).window.dispatchEvent(new (globalThis as any).CustomEvent('af-meeting-calendar-link', { detail: { meetingId: 'm1' } })); });
  expect(seen).toEqual([]);
  await act(async () => root.unmount());
});

test('a live recording reads the attendees of its live calendar event', async () => {
  beginLiveSession('Weekly sync', 1, true, { id: 'e', title: 'Weekly sync', calendar_id: 'c', calendar_name: 'C', start_ms: 0, end_ms: 1, is_meeting: true, attendees: [{ name: 'Bob', email: 'bob@a.com', organizer: true }] });
  let root: any;
  await act(async () => { root = create(<Probe open />); });
  expect(seen).toEqual([{ name: 'Bob', email: 'bob@a.com', organizer: true }]);
  await act(async () => root.unmount());
});
