import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';

const calls: Array<[string, any]> = [];
let candidates = { named: 1, with_audio: 1, automatic: 0, models_ready: true };
let labsOn = false;
let voices: any[] = [];
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args: any) => {
  calls.push([command, args]);
  if (command === 'voice_learning_candidates') return candidates;
  if (command === 'enroll_person_voice') return { person_id: 'p1', name: 'Jane', samples: 5, meetings: 1 };
  return null;
} }));
mock.module('@/hooks/useLabs', () => ({ useLabs: () => ({ labs: { voiceProfiles: labsOn } }) }));
mock.module('@/hooks/useVoiceProfiles', () => ({ useVoiceProfiles: () => voices }));
const toasts: string[] = [];
mock.module('sonner', () => ({ toast: { success: (title: string, options: any) => toasts.push(`${title} | ${options?.description ?? ''}`), error: (title: string) => toasts.push(`error: ${title}`) } }));
const wrap = ({ children }: any) => <div>{children}</div>;
mock.module('@/components/ui/surface', () => ({ Panel: ({ children }: any) => <section>{children}</section>, Skeleton: () => <i /> }));
mock.module('@/components/ui/badge', () => ({ Badge: wrap }));
mock.module('@/components/ui/button', () => ({ Button: ({ loading: _loading, ...props }: any) => <button {...props} /> }));
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { React, localStorage: storage, window: { localStorage: storage, dispatchEvent: () => true } });
const { ContactVoicePanel } = await import('../../src/components/people/ContactVoicePanel');

let root: ReactTestRenderer;
afterEach(async () => { await act(async () => root?.unmount()); calls.length = 0; toasts.length = 0; });
const text = () => JSON.stringify(root.toJSON());
const label = (children: any): string => (Array.isArray(children) ? children : [children]).filter(part => typeof part === 'string').join('');
const learnButton = () => root.root.findAllByType('button').find(button => label(button.props.children).includes('Learn voice'))!;

test('with no voice and nothing to learn from, Learn is disabled with the reason', async () => {
  candidates = { named: 0, with_audio: 0, automatic: 0, models_ready: true };
  voices = [];
  await act(async () => { root = create(<ContactVoicePanel personId="p1" first="Jane" />); });
  expect(text()).toContain('No voice saved');
  expect(text()).toContain('Name Jane as a speaker');
  expect(learnButton().props.disabled).toBe(true);
});

test('Learn with Labs off turns voice profiles on, learns, and says so', async () => {
  candidates = { named: 2, with_audio: 2, automatic: 0, models_ready: true };
  voices = [];
  labsOn = false;
  await act(async () => { root = create(<ContactVoicePanel personId="p1" first="Jane" />); });
  expect(text()).toContain('Can learn from 2 meetings');
  expect(text()).toContain('turns on Voice profiles');
  await act(async () => learnButton().props.onClick());
  expect(calls.map(([command]) => command)).toEqual(['voice_learning_candidates', 'set_voice_profiles_enabled', 'enroll_person_voice']);
  expect(toasts[0]).toContain('Voice profiles are now on');
});

test('a saved voice shows as saved', async () => {
  voices = [{ person_id: 'p1', name: 'Jane', samples: 9, meetings: 2 }];
  labsOn = true;
  await act(async () => { root = create(<ContactVoicePanel personId="p1" first="Jane" />); });
  expect(text()).toContain('Voice saved');
  expect(text()).toContain('9 clear turns in 2 meetings');
});
