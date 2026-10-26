import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';
Object.assign(globalThis, { React });
let configured = true;
const requests: Array<{ args: any; resolve: (value: unknown) => void; reject: (error: unknown) => void }> = [];
mock.module('next/link', () => ({ default: ({ children, ...props }: any) => <a {...props}>{children}</a> }));
mock.module('@tauri-apps/api/core', () => ({ invoke: (command: string, args: any) => {
  expect(command).toBe('calendar_recording_suggestions');
  return new Promise((resolve, reject) => requests.push({ args, resolve, reject }));
} }));
mock.module('@/hooks/useCalendar', () => ({ useCalendar: () => ({ snapshot: { settings: { enabled: configured, suggest_titles: true, selected_calendar_ids: ['work'] } } }) }));
mock.module('@/components/ui/popover', () => ({ Popover: ({ children, open, onOpenChange }: any) => <div><button onClick={() => onOpenChange(!open)}>Toggle</button>{open && children}</div>, PopoverTrigger: () => null, PopoverContent: ({ children }: any) => <div>{children}</div> }));
mock.module('@/components/calendar/CalendarMeetings', () => ({ calendarTime: () => '10:00–11:00' }));
const { SavedCalendarTitleSuggestions } = await import('../../src/components/calendar/SavedCalendarTitleSuggestions');
let root: ReactTestRenderer;
const rename = mock(async (_title: string) => true);
const result = { start_ms: 1_000_000, end_ms: 3_000_000, matches: ['Review', 'Planning'].map(title => ({ event: { id: title, title, start_ms: 1_000_000, end_ms: 3_000_000, calendar_name: 'Work' }, overlap_ms: 2_000_000, gap_ms: 0 })) };
async function mount(id = 'saved') { await act(async () => { root = create(<SavedCalendarTitleSuggestions meetingId={id} onRename={rename} />); }); }
async function open() { await act(async () => { root.root.findAllByType('button').find(b => b.children.includes('Toggle'))!.props.onClick(); }); }
afterEach(async () => { await act(async () => root?.unmount()); requests.length = 0; rename.mockClear(); configured = true; });

test('saved lookup uses meeting identity and applies only the chosen overlapping event', async () => {
  await mount(); expect(requests).toHaveLength(0);
  await open(); expect(requests[0].args).toEqual({ meetingId: 'saved' });
  await act(async () => requests[0].resolve(result));
  expect(rename).not.toHaveBeenCalled();
  const choices = root.root.findAllByType('button').filter(b => b.props.disabled === false);
  expect(choices).toHaveLength(2);
  await act(async () => choices[1].props.onClick());
  expect(rename).toHaveBeenCalledWith('Planning');
});
test('navigation discards an earlier recording lookup', async () => {
  await mount('old'); await open();
  await act(async () => root.update(<SavedCalendarTitleSuggestions meetingId="new" onRename={rename} />));
  await act(async () => requests[0].resolve(result));
  expect(JSON.stringify(root.toJSON())).not.toContain('Review');
  await open(); expect(requests[1].args.meetingId).toBe('new');
});
test('lookup errors and empty matches keep the existing title', async () => {
  await mount(); await open();
  await act(async () => requests[0].reject('Calendar unavailable'));
  expect(JSON.stringify(root.toJSON())).toContain('Calendar unavailable');
  await act(async () => root.root.findAllByType('button').find(b => b.children.includes('Try again'))!.props.onClick());
  await act(async () => requests[1].resolve({ ...result, matches: [] }));
  expect(JSON.stringify(root.toJSON())).toContain('No events overlap');
  expect(rename).not.toHaveBeenCalled();
});
test('disconnected calendar offers setup without making a provider query', async () => {
  configured = false; await mount(); await open();
  expect(requests).toHaveLength(0);
  expect(JSON.stringify(root.toJSON())).toContain('/settings?section=calendar');
});
