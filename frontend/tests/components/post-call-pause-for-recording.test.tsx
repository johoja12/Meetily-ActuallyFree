import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';

const calls: string[] = [];
const handlers = new Map<string, Array<(event: { payload: any }) => void>>();
const fire = (name: string, payload: any) => handlers.get(name)?.slice().forEach(handler => handler({ payload }));
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string) => {
  calls.push(command);
  if (command === 'api_get_post_call_transcript_config') return { provider: 'parakeet', model: 'p' };
  if (command === 'parakeet_get_available_models') return [{ name: 'p', status: 'Available' }];
  if (command === 'whisper_get_available_models') return [];
  return null;
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: any) => {
  handlers.set(name, [...(handlers.get(name) ?? []), handler]);
  return () => handlers.set(name, (handlers.get(name) ?? []).filter(item => item !== handler));
} }));
mock.module('@/hooks/useDiarizationEngine', () => ({ useDiarizationEngine: () => ({ engine: 'nemotron', isNemotron: true, error: null }) }));
mock.module('@/contexts/ConfigContext', () => ({ useConfig: () => ({ selectedLanguage: 'auto', transcriptModelConfig: null }) }));
const toasts: string[] = [];
mock.module('sonner', () => ({ toast: Object.assign(() => {}, { success: () => {}, error: (title: string) => toasts.push(title), info: () => {} }) }));
mock.module('@/components/PostCallHandoffCard', () => ({ PostCallHandoffCard: ({ title, children }: any) => <div data-title={title}>{children}</div> }));
mock.module('@/components/ui/button', () => ({ Button: (props: any) => <button {...props} /> }));
mock.module('@/lib/workspace-api', () => ({ announceChange: () => {} }));
mock.module('next/link', () => ({ default: ({ children }: any) => <a>{children}</a> }));
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
const timers: Array<() => void> = [];
Object.assign(globalThis, { React, localStorage: storage, sessionStorage: storage, window: { localStorage: storage, sessionStorage: storage, dispatchEvent: () => true, addEventListener() {}, removeEventListener() {} }, Event: class { constructor(public type: string) {} } });
const realSetTimeout = globalThis.setTimeout;
(globalThis as any).setTimeout = (callback: () => void, ms: number) => (ms === 5000 ? (timers.push(callback), 0) : realSetTimeout(callback, ms));
const { PostCallProcessingWorker } = await import('../../src/components/MeetingDetails/PostCallProcessingWorker');

let root: ReactTestRenderer;
afterEach(async () => { await act(async () => root?.unmount()); });
const title = () => root.root.findAll(node => typeof node.props['data-title'] === 'string')[0]?.props['data-title'];
const settle = () => act(async () => { await new Promise(resolve => realSetTimeout(resolve, 0)); });

test('a recording pauses the re-run, which starts over when the recording stops', async () => {
  values.set('af-post-call-auto-identify', '1');
  await act(async () => { root = create(<PostCallProcessingWorker enabled meetingId="m1" meetingFolderPath="/rec" onComplete={() => {}} />); });
  await settle();
  expect(calls.filter(command => command === 'start_retranscription_command').length).toBe(1);
  // Recording starts: native stops the re-run and says so, then reports it cancelled.
  await act(async () => {
    fire('retranscription-paused-for-recording', { meeting_id: 'm1' });
    fire('retranscription-error', { meeting_id: 'm1', error: 'Retranscription cancelled' });
  });
  await settle();
  expect(title()).toBe('Paused while you record');
  expect(toasts).toEqual([]);
  await act(async () => { fire('recording-stopped', {}); });
  await act(async () => { timers.splice(0).forEach(run => run()); });
  await settle();
  expect(calls.filter(command => command === 'start_retranscription_command').length).toBe(2);
});
