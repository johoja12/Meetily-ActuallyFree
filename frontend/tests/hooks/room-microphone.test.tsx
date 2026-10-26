import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';

let preferences: Record<string, unknown>;
let saveFails = false;
let loadFails = false;
let finishSave: (() => void) | undefined;
const errors: string[] = [];
const events = new EventTarget();
Object.assign(globalThis, { React, window: events });
mock.module('sonner', () => ({ toast: { error: (message: string) => errors.push(message) } }));
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args?: any) => {
  if (command === 'get_recording_preferences') {
    if (loadFails) throw new Error('unavailable');
    return { ...preferences };
  }
  if (command === 'set_recording_preferences') {
    if (saveFails) throw new Error('disk full');
    if (finishSave) await new Promise<void>((resolve) => { finishSave = resolve; });
    preferences = args.preferences;
    return;
  }
  throw new Error(`Unexpected command ${command}`);
} }));
const { useRoomMicrophone } = await import('../../src/hooks/useRoomMicrophone');
const { RoomMicrophoneSetting } = await import('../../src/components/recording/RoomMicrophoneSetting');
const { isLinkableSpeakerName } = await import('../../src/lib/speakerLabels');
const { isUnnamedSpeaker } = await import('../../src/utils/speakerUtils');
let queue: { current: Promise<void> } | undefined;
let current: ReturnType<typeof useRoomMicrophone>;
const roots: ReactTestRenderer[] = [];
function Probe() { current = useRoomMicrophone(queue); return null; }
async function mount() { await act(async () => { roots.push(create(<Probe />)); }); }
beforeEach(() => { preferences = { auto_save: true, mic_gain: 1.7, per_app_recording_enabled: true }; saveFails = false; loadFails = false; finishSave = undefined; queue = undefined; errors.length = 0; });
afterEach(async () => { await act(async () => roots.splice(0).forEach(root => root.unmount())); });

test('legacy preferences default to personal microphone, then persist room mode without losing other choices', async () => {
  await mount();
  expect(current.enabled).toBe(false);
  await act(async () => current.update(true));
  expect(current.enabled).toBe(true);
  expect(preferences).toEqual({ auto_save: true, mic_gain: 1.7, per_app_recording_enabled: true, room_microphone: true });
  await mount();
  expect(current.enabled).toBe(true);
  await act(async () => current.update(false));
  expect(preferences.room_microphone).toBe(false);
});

test('mode stays unchanged until native save completes and reports a failed save', async () => {
  await mount();
  finishSave = () => {};
  let saving!: Promise<void>;
  await act(async () => { saving = current.update(true); });
  expect(current.saving).toBe(true);
  expect(current.enabled).toBe(false);
  await act(async () => { finishSave!(); await saving; });
  expect(current.enabled).toBe(true);
  saveFails = true;
  await act(async () => current.update(false));
  expect(current.enabled).toBe(true);
  expect(errors).toEqual(['Could not save microphone mode']);
});

test('unreadable preferences are exposed instead of silently assuming a mode', async () => {
  loadFails = true;
  await mount();
  expect(current.loadError).toBe(true);
  expect(errors).toEqual(['Could not load microphone mode']);
  expect(current.loading).toBe(false);
});

test('room source label remains anonymous and does not become a contact', () => {
  expect(isLinkableSpeakerName('Room')).toBe(false);
  expect(isUnnamedSpeaker('Room')).toBe(true);
  expect(isLinkableSpeakerName('Robin')).toBe(true);
});

test('room control is accessible and disabled during a recording', async () => {
  let root!: ReactTestRenderer;
  await act(async () => { root = create(<RoomMicrophoneSetting enabled disabled onChange={() => {}} />); roots.push(root); });
  const control = root.root.findByProps({ 'aria-label': 'Room microphone' });
  expect(control.props.disabled).toBe(true);
  expect(control.props.checked).toBe(true);
});


test('mode save waits for pending device preferences and reads their completed values', async () => {
  let finishDevice!: () => void;
  queue = { current: new Promise<void>(resolve => { finishDevice = resolve; }) };
  await mount();
  let saving!: Promise<void>;
  await act(async () => { saving = current.update(true); });
  expect(current.saving).toBe(true);
  expect(preferences.room_microphone).toBeUndefined();
  preferences.preferred_mic_device = 'External room microphone';
  preferences.mic_gain = 2.1;
  await act(async () => { finishDevice(); await saving; });
  expect(preferences.room_microphone).toBe(true);
  expect(preferences.preferred_mic_device).toBe('External room microphone');
  expect(preferences.mic_gain).toBe(2.1);
});
