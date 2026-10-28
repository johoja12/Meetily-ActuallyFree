import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';

const calls: string[] = [];
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string) => {
  calls.push(command);
  // Stop the workflow at its first native step; the test only checks that it began.
  throw new Error('stop here');
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
mock.module('@/hooks/useDiarizationEngine', () => ({ useDiarizationEngine: () => ({ engine: 'nemotron', isNemotron: true, error: null }) }));
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
afterEach(async () => { await act(async () => root?.unmount()); calls.length = 0; values.clear(); });
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
