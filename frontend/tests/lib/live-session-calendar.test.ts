import { beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';

const values = new Map<string, string>();
const storage = {
  getItem: (key: string) => values.get(key) ?? null,
  setItem: (key: string, value: string) => values.set(key, value),
  removeItem: (key: string) => values.delete(key),
};
Object.assign(globalThis, { window: { sessionStorage: storage, localStorage: storage, dispatchEvent: () => true, addEventListener() {}, removeEventListener() {} }, CustomEvent: class { constructor(public type: string) {} } });

const { beginLiveSession, readLiveTitle, renameLiveTitle, undoCalendarTitle } = await import('../../src/lib/live-session');
const event = { id: 'e', title: 'Weekly sync', calendar_id: 'c', calendar_name: 'Work', start_ms: 0, end_ms: 1, is_meeting: true, attendees: [] };
beforeEach(() => values.clear());

test('a calendar-named recording keeps its event through renames', () => {
  beginLiveSession('Weekly sync', 1_000, true, event);
  renameLiveTitle('Weekly sync with Jane');
  assert.deepEqual(readLiveTitle(), { title: 'Weekly sync with Jane', manual: true, startedAt: 1_000, calendarEvent: event });
});

test('undo restores the automatic title and drops the event, only while the calendar title stands', () => {
  beginLiveSession('Weekly sync', 1_000, true, event);
  assert.equal(undoCalendarTitle('Meeting · 9:00 AM'), true);
  assert.deepEqual(readLiveTitle(), { title: 'Meeting · 9:00 AM', manual: false, startedAt: 1_000, calendarEvent: null });
  beginLiveSession('Weekly sync', 1_000, true, event);
  renameLiveTitle('Typed by me');
  assert.equal(undoCalendarTitle('Meeting · 9:00 AM'), false);
  assert.equal(readLiveTitle()?.title, 'Typed by me');
});
