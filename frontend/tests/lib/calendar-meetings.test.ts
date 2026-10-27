import { beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { calendarTitleSuggestions, selectCalendarRecording, takeCalendarRecording, type CalendarMeeting } from '../../src/lib/calendar-meetings';
const values = new Map<string, string>();
Object.assign(globalThis, { sessionStorage: {
  getItem: (key: string) => values.get(key) ?? null,
  setItem: (key: string, value: string) => values.set(key, value),
  removeItem: (key: string) => values.delete(key),
} });
const now = Date.parse('2026-10-26T18:00:00Z');
const meeting = (id: string, start: number, end: number): CalendarMeeting => ({
  id, title: `Meeting ${id}`, calendar_id: 'work', calendar_name: 'Work',
  start_ms: now + start * 60_000, end_ms: now + end * 60_000, is_meeting: true,
});
beforeEach(() => values.clear());

test('current and upcoming titles are suggestions; overlapping meetings remain choices', () => {
  const events = [meeting('ended', -30, 0), meeting('future', 16, 30), meeting('soon', 5, 35), meeting('ongoing', -15, 15), meeting('just-started', -1, 29)];
  assert.deepEqual(calendarTitleSuggestions(events, now).map(event => event.id), ['just-started', 'ongoing', 'soon']);
});

test('occurrences are deduplicated by identity and start time, never by title', () => {
  const event = meeting('series', -10, 10);
  assert.equal(calendarTitleSuggestions([event, event, { ...meeting('another', -5, 25), title: event.title }], now).length, 2);
  assert.equal(calendarTitleSuggestions([event, { ...event, start_ms: now, end_ms: now + 60_000 }], now).length, 2);
});

test('only a recent confirmed calendar selection names the next recording, once', () => {
  const event = meeting('selected', -5, 25);
  selectCalendarRecording(event, now);
  assert.deepEqual(takeCalendarRecording(now + 1_000), event);
  assert.equal(takeCalendarRecording(now + 2_000), null);
  selectCalendarRecording(event, now);
  assert.equal(takeCalendarRecording(now + 11 * 60_000), null);
});

test('malformed dates, empty titles, corrupt storage and ended selections are rejected', () => {
  const event = meeting('invalid', 0, 30);
  assert.deepEqual(calendarTitleSuggestions([{ ...event, start_ms: NaN }, { ...event, title: ' ' }, { ...event, end_ms: event.start_ms }], now), []);
  values.set('af-calendar-recording-title', '{');
  assert.equal(takeCalendarRecording(now), null);
  selectCalendarRecording(meeting('ended', -10, 1), now);
  assert.equal(takeCalendarRecording(now + 2 * 60_000), null);
});

test('only one current meeting is applied automatically; several ask; plain events never apply', async () => {
  const { autoCalendarChoice } = await import('../../src/lib/calendar-meetings');
  const one = meeting('one', -5, 25);
  const focus = { ...meeting('focus', -10, 50), is_meeting: false };
  assert.deepEqual(autoCalendarChoice([one, focus, meeting('ended', -30, 0)], now), { kind: 'one', event: one });
  assert.deepEqual(autoCalendarChoice([one, meeting('two', 3, 40)], now), { kind: 'many', count: 2 });
  assert.equal(autoCalendarChoice([focus], now), null);
  assert.equal(autoCalendarChoice([], now), null);
});

test('a meeting names a recording automatically only once started or within five minutes', async () => {
  const { autoCalendarChoice } = await import('../../src/lib/calendar-meetings');
  assert.equal(autoCalendarChoice([meeting('later', 14, 44)], now), null);
  const soon = meeting('soon', 4, 34);
  assert.deepEqual(autoCalendarChoice([soon, meeting('later', 14, 44)], now), { kind: 'one', event: soon });
});

test('a Choose request waits for the calendar menu to appear', async () => {
  const { requestCalendarTitleMenu, takeCalendarTitleMenuRequest } = await import('../../src/lib/calendar-meetings');
  const navigated: string[] = [];
  const dispatched: string[] = [];
  Object.assign(globalThis, { window: { location: { pathname: '/meetings/x' }, dispatchEvent: (event: { type: string }) => { dispatched.push(event.type); return true; } }, CustomEvent: class { constructor(public type: string) {} } });
  requestCalendarTitleMenu(href => navigated.push(href), now);
  assert.deepEqual(navigated, ['/']);
  assert.equal(takeCalendarTitleMenuRequest(now), true);
  assert.equal(takeCalendarTitleMenuRequest(now), false);
  (globalThis as any).window.location.pathname = '/';
  requestCalendarTitleMenu(href => navigated.push(href));
  assert.deepEqual(dispatched, ['af-open-calendar-title']);
  assert.equal(navigated.length, 1);
});
