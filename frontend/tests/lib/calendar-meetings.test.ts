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
  assert.deepEqual(autoCalendarChoice([one, meeting('two', 10, 40)], now), { kind: 'many', count: 2 });
  assert.equal(autoCalendarChoice([focus], now), null);
  assert.equal(autoCalendarChoice([], now), null);
});
