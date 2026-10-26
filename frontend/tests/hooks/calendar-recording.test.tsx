import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import type { CalendarSnapshot } from '../../src/hooks/useCalendar';
import type { CalendarMeeting } from '../../src/lib/calendar-meetings';

let snapshot: CalendarSnapshot;
let recording = false;
let prompts: CalendarMeeting[] = [];
let connectFails = false;
const calls: Array<[string, any]> = [];
const messages: Array<{ title: string; options: any }> = [];
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
const intervals = new Map<number, () => void>();
const reload = mock(async () => {});
const start = mock(() => {});
Object.assign(globalThis, { React, sessionStorage: storage, window: {
  sessionStorage: storage, localStorage: storage, location: { pathname: '/' }, dispatchEvent: () => true,
  setInterval: (callback: () => void) => { const id = intervals.size + 1; intervals.set(id, callback); return id; },
  clearInterval: (id: number) => intervals.delete(id),
} });
const toast = Object.assign((title: string, options: any) => messages.push({ title, options }), { error: mock(() => {}), info: mock(() => {}) });
mock.module('sonner', () => ({ toast }));
mock.module('next/navigation', () => ({ useRouter: () => ({ push: () => {} }) }));
mock.module('@/hooks/useCalendar', () => ({ useCalendar: () => ({ snapshot, error: null, reload }) }));
mock.module('@/components/ui/popover', () => ({ Popover: ({ children }: any) => children, PopoverTrigger: ({ children }: any) => children, PopoverContent: ({ children }: any) => <div>{children}</div> }));
mock.module('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args: any) => {
  calls.push([command, args]);
  if (command === 'calendar_take_prompts') { const result = prompts; prompts = []; return result; }
  if (command === 'get_recording_state') return { is_recording: recording };
  if (command === 'calendar_connect') { if (connectFails) throw new Error('Permission denied'); return snapshot; }
  if (command === 'api_save_transcript') return { meeting_id: 'saved' };
  return snapshot;
} }));
const { CalendarRecordingPrompts } = await import('../../src/components/calendar/CalendarRecordingPrompts');
const { CalendarSettings } = await import('../../src/components/calendar/CalendarSettings');
const { CalendarTitleSuggestions } = await import('../../src/components/calendar/CalendarMeetings');
const { beginLiveSession, readLiveTitle, finalLiveTitle } = await import('../../src/lib/live-session');
const { takeCalendarRecording } = await import('../../src/lib/calendar-meetings');
const { storageService } = await import('../../src/services/storageService');
const roots: ReactTestRenderer[] = [];
async function mount(node: React.ReactElement) { let root!: ReactTestRenderer; await act(async () => { root = create(node); roots.push(root); }); return root; }
beforeEach(() => {
  const now = Date.now();
  snapshot = { supported: true, authorization: 'authorized', busy: false, error: null, last_updated_ms: now,
    settings: { enabled: true, suggest_titles: true, prompt_recording: true, reminder_minutes: 5, selected_calendar_ids: ['work'] },
    calendars: [{ id: 'work', name: 'Work' }], events: [{ id: 'review', title: 'Design review', calendar_id: 'work', calendar_name: 'Work', start_ms: now - 60_000, end_ms: now + 30 * 60_000, is_meeting: true }],
  };
  recording = false; prompts = []; connectFails = false; calls.length = 0; messages.length = 0; values.clear(); intervals.clear(); start.mockClear(); reload.mockClear(); toast.error.mockClear(); toast.info.mockClear();
});
afterEach(async () => { await act(async () => roots.splice(0).forEach(root => root.unmount())); expect(intervals.size).toBe(0); });

test('a calendar reminder prompts but never automatically starts capture', async () => {
  prompts = [snapshot.events[0]];
  await mount(<CalendarRecordingPrompts enabled onStart={start} />);
  expect(messages).toHaveLength(1);
  expect(start).not.toHaveBeenCalled();
  expect(takeCalendarRecording()).toBeNull();
  await act(async () => { messages[0].options.action.onClick(); });
  expect(start).toHaveBeenCalledTimes(1);
  expect(takeCalendarRecording()?.title).toBe('Design review');
});

test('a stale reminder checks native state and cannot start over a running recording', async () => {
  prompts = [snapshot.events[0]];
  await mount(<CalendarRecordingPrompts enabled onStart={start} />);
  recording = true;
  await act(async () => { messages[0].options.action.onClick(); });
  expect(start).not.toHaveBeenCalled(); expect(takeCalendarRecording()).toBeNull();
});

test('reminders do not drain before onboarding and recording disclosure are ready', async () => {
  await mount(<CalendarRecordingPrompts enabled={false} onStart={start} />);
  expect(calls).toHaveLength(0); expect(messages).toHaveLength(0);
});

test('calendar title choices preserve overlaps and only rename on explicit selection', async () => {
  snapshot.events.push({ ...snapshot.events[0], id: 'overlap', title: 'Another meeting' });
  const selected = mock(() => {});
  const root = await mount(<CalendarTitleSuggestions startedAt={Date.now()} onSelect={selected} />);
  expect(selected).not.toHaveBeenCalled();
  const choices = root.root.findAllByType('button').filter(button => button.props.onClick);
  expect(choices).toHaveLength(2);
  await act(async () => { choices[0].props.onClick(); });
  expect(selected).toHaveBeenCalledTimes(1);
});

test('permission is requested only on Connect, and failure does not enable the feature', async () => {
  snapshot.settings.enabled = false;
  connectFails = true;
  const root = await mount(<CalendarSettings />);
  expect(calls).toHaveLength(0);
  const connect = root.root.findByProps({ 'aria-label': 'Connect macOS Calendar' });
  await act(async () => { await connect.props.onClick(); });
  expect(calls[0][0]).toBe('calendar_connect');
  expect(snapshot.settings.enabled).toBe(false);
  expect(toast.error).toHaveBeenCalled();
});

test('a chosen calendar title keeps explicit ownership through live and native save interfaces', async () => {
  beginLiveSession('New Meeting', Date.now(), true);
  expect(readLiveTitle()?.manual).toBe(true);
  expect(finalLiveTitle('Fallback', new Date(Date.now() + 60_000))).toBe('New Meeting');
  await storageService.saveMeeting('New Meeting', [], null, null, true);
  expect(calls.at(-1)).toEqual(['api_save_transcript', { meetingTitle: 'New Meeting', transcripts: [], folderPath: null, recordingStartedAt: null, titleIsManual: true }]);
});
