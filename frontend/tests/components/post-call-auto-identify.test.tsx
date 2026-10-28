import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';

const calls: string[] = [];
const notifications: unknown[] = [];
const handlers = new Map<string, (event: { payload: unknown }) => void>();
// By default every native step fails, so a run stops at its first step.
let succeed = false;
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args?: unknown) => {
  calls.push(command);
  if (command === 'show_simple_notification') { notifications.push(args); return; }
  if (!succeed) throw new Error('stop here');
  if (command === 'parakeet_get_available_models') return [{ name: 'parakeet-tdt-0.6b-v3-int8', status: 'Available' }];
  if (command === 'whisper_get_available_models') return [];
  if (command === 'api_get_post_call_transcript_config') return { provider: 'live', model: '' };
  if (command === 'start_retranscription_command') {
    queueMicrotask(() => handlers.get('retranscription-complete')?.({ payload: { meeting_id: 'm1' } }));
  }
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => { if (handlers.get(name) === handler) handlers.delete(name); };
} }));
let engineState: { engine: string | null; isNemotron: boolean; error: string | null } = { engine: 'nemotron', isNemotron: true, error: null };
mock.module('@/hooks/useDiarizationEngine', () => ({ useDiarizationEngine: () => engineState }));
mock.module('@/contexts/ConfigContext', () => ({ useConfig: () => ({ selectedLanguage: 'auto', transcriptModelConfig: { provider: 'parakeet', model: 'm' } }) }));
mock.module('sonner', () => ({ toast: Object.assign(() => {}, { success: () => {}, error: () => {}, info: () => {} }) }));
mock.module('@/components/PostCallHandoffCard', () => ({ PostCallHandoffCard: ({ title, children }: any) => <div data-title={title}>{children}</div> }));
mock.module('@/components/ui/button', () => ({ Button: (props: any) => <button {...props} /> }));
mock.module('@/lib/workspace-api', () => ({ announceChange: () => {} }));
mock.module('next/link', () => ({ default: ({ children }: any) => <a>{children}</a> }));
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { React, localStorage: storage, sessionStorage: storage, window: { localStorage: storage, sessionStorage: storage, dispatchEvent: () => true, addEventListener() {}, removeEventListener() {} }, Event: class { constructor(public type: string) {} } });
const { PostCallProcessingWorker } = await import('../../src/components/MeetingDetails/PostCallProcessingWorker');

let root: ReactTestRenderer;
afterEach(async () => {
  await act(async () => root?.unmount());
  calls.length = 0; notifications.length = 0; values.clear(); succeed = false;
  engineState = { engine: 'nemotron', isNemotron: true, error: null };
});
const settle = () => act(async () => { for (let i = 0; i < 20; i++) await new Promise(resolve => setTimeout(resolve, 0)); });
const render = async () => {
  await act(async () => { root = create(<PostCallProcessingWorker enabled meetingId="m1" meetingFolderPath="/rec" onComplete={() => {}} />); });
};

test('with the setting off, the card waits for a choice', async () => {
  await render();
  expect(calls).toEqual([]);
  expect(root.root.findAll(node => node.props['data-title'] === 'Identify speakers with Nemotron').length).toBe(1);
});

test('with the setting on, speakers are identified without asking, once', async () => {
  values.set('af-post-call-auto-identify', '1');
  await render();
  expect(calls[0]).toBe('api_get_post_call_transcript_config');
  const started = calls.length;
  await act(async () => { root.update(<PostCallProcessingWorker enabled meetingId="m1" meetingFolderPath="/rec" onComplete={() => {}} />); });
  expect(calls.length).toBe(started);
});

test('ticking the box on the card remembers the choice', async () => {
  await render();
  const box = root.root.findByProps({ 'aria-label': 'Transcribe and identify speakers after every recording' });
  await act(async () => box.props.onChange({ target: { checked: true } }));
  expect(values.get('af-post-call-auto-identify')).toBe('1');
});

test('with the setting on, a failed engine status load does not stop the run', async () => {
  values.set('af-post-call-auto-identify', '1');
  engineState = { engine: null, isNemotron: false, error: 'Could not load the selected diarization engine.' };
  await render();
  expect(calls[0]).toBe('api_get_post_call_transcript_config');
});

test('an automatic run that finishes sends a system notification', async () => {
  values.set('af-post-call-auto-identify', '1');
  succeed = true;
  let completed = false;
  await act(async () => { root = create(<PostCallProcessingWorker enabled meetingId="m1" meetingFolderPath="/rec" onComplete={() => { completed = true; }} />); });
  await settle();
  expect(calls).toContain('diarize_meeting');
  expect(completed).toBe(true);
  expect(notifications).toEqual([{ title: 'Transcript and speakers ready', body: 'Transcript enhanced and speaker labels refreshed.' }]);
});

test('an automatic run that fails sends a system notification with the reason', async () => {
  values.set('af-post-call-auto-identify', '1');
  await render();
  await settle();
  expect(notifications).toEqual([{ title: 'Post-call processing failed', body: 'No downloaded parakeet model is available for enhancement.' }]);
});

test('a run started from the card does not send a notification', async () => {
  await render();
  const button = root.root.findAll(node => node.type === 'button' && node.props.children === 'Auto-detect & continue')[0];
  await act(async () => { button.props.onClick(); });
  await settle();
  expect(calls).toContain('api_get_post_call_transcript_config');
  expect(notifications).toEqual([]);
});
