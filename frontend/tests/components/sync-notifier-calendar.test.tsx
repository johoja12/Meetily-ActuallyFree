import React from 'react';
import { act, create } from 'react-test-renderer';
import { expect, mock, test } from 'bun:test';

const dispatched: Array<{ type: string; detail?: any }> = [];
Object.assign(globalThis, { React, window: { dispatchEvent: (event: any) => { dispatched.push(event); return true; } },
  CustomEvent: class { constructor(public type: string, init?: { detail?: unknown }) { (this as any).detail = init?.detail; } },
  Event: class { constructor(public type: string) {} } });
let handler: ((event: { payload: any }) => void) | null = null;
mock.module('@tauri-apps/api/event', () => ({ listen: async (_name: string, callback: any) => { handler = callback; return () => {}; } }));
mock.module('@/lib/workspace-api', () => ({ announceChange: () => {} }));
mock.module('@/lib/voice-matches', () => ({ announceVoiceMatchChanges: () => {} }));
const { SyncNotifier } = await import('../../src/components/SyncNotifier');

test('meetings imported by sync refresh their calendar invite', async () => {
  await act(async () => { create(<SyncNotifier />); });
  await act(async () => { handler!({ payload: { meeting_ids: ['m1', 'm2'], library: false, voices: false } }); });
  expect(dispatched.filter(event => event.type === 'af-meeting-calendar-link').map(event => event.detail.meetingId)).toEqual(['m1', 'm2']);
});
