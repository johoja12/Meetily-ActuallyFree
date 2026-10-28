import { test } from 'node:test';
import assert from 'node:assert/strict';
import { endReason, freshEndState, isGoodbye, noteGoodbye, noteLevel, type EndState } from '../../src/lib/meeting-end';

const MIN = 60_000;
const T0 = 1_000_000_000;

/** Levels arrive every second; `loud` until `soundUntil`, then quiet. */
function withLevels(state: EndState, from: number, to: number, soundUntil: number): EndState {
  let next = state;
  for (let at = from; at <= to; at += 1000) next = noteLevel(next, at < soundUntil ? 0.05 : 0.001, at);
  return next;
}

test('goodbye phrases are recognized without catching ordinary thanks', () => {
  for (const text of ['Okay, bye!', 'Bye-bye everyone', 'Thanks everyone, talk soon.', 'See you next week', 'Have a great weekend', 'Take care, goodbye'])
    assert.ok(isGoodbye(text), text);
  for (const text of ['Thanks for the update', 'Thank you so much for that', 'Buy the licenses', 'Byers is on the call'])
    assert.equal(isGoodbye(text), false, text);
});

test('the meeting app hanging up is reported right away', () => {
  const state = { ...freshEndState(T0, null), appEnded: { at: T0 + 10 * MIN, app: 'Webex' } };
  assert.deepEqual(endReason(state, T0 + 10 * MIN), { reason: 'app', app: 'Webex' });
});

test('silence on both sides needs 3 minutes before the calendar end and 1 minute after', () => {
  const end = T0 + 30 * MIN;
  let state = withLevels(freshEndState(T0, end), T0, T0 + 20 * MIN, T0 + 18 * MIN);
  // 2 minutes quiet, before the end: not yet.
  assert.equal(endReason(state, T0 + 20 * MIN), null);
  state = withLevels(state, T0 + 20 * MIN, T0 + 21 * MIN, 0);
  assert.equal(endReason(state, T0 + 21 * MIN)?.reason, 'silence');
  // Past the end, one minute is enough.
  let late = withLevels(freshEndState(T0, end), T0, end + 2 * MIN, end + 50_000);
  assert.equal(endReason(late, end + 50_000 + 58_000), null);
  late = withLevels(late, end + 2 * MIN, end + 2 * MIN, 0);
  assert.equal(endReason(late, end + 50_000 + 61_000)?.reason, 'silence');
});

test('no level events (paused, or a stalled stream) is not silence', () => {
  const state = withLevels(freshEndState(T0, null), T0, T0 + MIN, T0 + 10_000);
  assert.equal(endReason(state, T0 + 10 * MIN), null);
});

test('goodbyes end the meeting after 15 seconds of quiet near the calendar end', () => {
  const end = T0 + 30 * MIN;
  let state = withLevels(freshEndState(T0, end), T0, end, end - 20_000);
  state = noteGoodbye(state, end - 30_000);
  state = noteGoodbye(state, end - 25_000);
  assert.equal(endReason(state, end - 10_000), null, 'still talking');
  state = withLevels(state, end, end + 2_000, 0);
  assert.equal(endReason(state, end - 20_000 + 16_000)?.reason, 'goodbye');
  // One goodbye is someone leaving early, not the meeting ending.
  let single = withLevels(freshEndState(T0, end), T0, end, end - 20_000);
  single = noteGoodbye(single, end - 25_000);
  assert.equal(endReason(single, end), null);
  // Well before the end, goodbyes alone are not enough.
  let early = withLevels(freshEndState(T0, end), T0, T0 + 10 * MIN, T0 + 9 * MIN);
  early = noteGoodbye(noteGoodbye(early, T0 + 9 * MIN - 5_000), T0 + 9 * MIN - 2_000);
  assert.equal(endReason(early, T0 + 10 * MIN), null);
  // Without a calendar event, two goodbyes and quiet are enough.
  let free = withLevels(freshEndState(T0, null), T0, T0 + 10 * MIN, T0 + 10 * MIN - 20_000);
  free = noteGoodbye(noteGoodbye(free, T0 + 10 * MIN - 30_000), T0 + 10 * MIN - 25_000);
  assert.equal(endReason(free, T0 + 10 * MIN)?.reason, 'goodbye');
});

test('Keep recording snoozes prompts for 10 minutes and forgets old goodbyes', () => {
  let state = withLevels(freshEndState(T0, null), T0, T0 + 5 * MIN, T0 + MIN);
  assert.equal(endReason(state, T0 + 5 * MIN)?.reason, 'silence');
  state = { ...state, snoozedUntil: T0 + 15 * MIN, goodbyes: [] };
  state = withLevels(state, T0 + 5 * MIN, T0 + 14 * MIN, 0);
  assert.equal(endReason(state, T0 + 14 * MIN), null);
  state = withLevels(state, T0 + 14 * MIN, T0 + 16 * MIN, 0);
  assert.equal(endReason(state, T0 + 16 * MIN)?.reason, 'silence');
});

test('a gap in level events (pause) restarts the quiet clock', () => {
  let state = withLevels(freshEndState(T0, null), T0, T0 + MIN, T0 + 10_000);
  // Paused for 10 minutes, then quiet levels resume.
  state = noteLevel(state, 0.001, T0 + 11 * MIN);
  assert.equal(endReason(state, T0 + 11 * MIN), null);
  state = withLevels(state, T0 + 11 * MIN, T0 + 14 * MIN, 0);
  assert.equal(endReason(state, T0 + 14 * MIN)?.reason, 'silence');
});
