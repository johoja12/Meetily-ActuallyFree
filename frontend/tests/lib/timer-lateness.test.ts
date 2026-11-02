import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PROBE_INTERVAL_MS, reportableLateness } from '../../src/lib/timer-lateness';

test('on-time and slightly late ticks are not reported', () => {
  assert.equal(reportableLateness(0, PROBE_INTERVAL_MS), null);
  assert.equal(reportableLateness(0, PROBE_INTERVAL_MS + 5_000), null);
});

test('a throttled tick reports how late it was', () => {
  // Chromium's intensive throttling aligns timers to once a minute.
  assert.equal(reportableLateness(0, 60_000), 45_000);
});

test('a sleep/wake gap is not throttling', () => {
  assert.equal(reportableLateness(0, 2 * 60 * 60_000), null);
});
