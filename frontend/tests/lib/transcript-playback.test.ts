import { test } from 'node:test';
import assert from 'node:assert/strict';
import { activeIndex, needsMoreForPlayback } from '../../src/lib/transcript-playback';

const spans = [
  { timestamp: 0, endTime: 4 },
  { timestamp: 5, endTime: 9 },
  { timestamp: 30, endTime: 34 },
];

test('the span playing now is the last one started, kept lit until the next', () => {
  assert.equal(activeIndex(spans, null), -1);
  assert.equal(activeIndex(spans, 0), 0);
  assert.equal(activeIndex(spans, 4.5), 0);
  assert.equal(activeIndex(spans, 6), 1);
  // Between spans the last one stays lit until the next starts.
  assert.equal(activeIndex(spans, 20), 1);
  assert.equal(activeIndex(spans, 31), 2);
  // After the last span it stays lit.
  assert.equal(activeIndex(spans, 500), 2);
});

test('more transcript loads when playback passes the loaded part', () => {
  assert.equal(needsMoreForPlayback(spans, 10, true), false);
  assert.equal(needsMoreForPlayback(spans, 34, true), false);
  assert.equal(needsMoreForPlayback(spans, 35, true), true);
  assert.equal(needsMoreForPlayback(spans, 35, false), false);
  assert.equal(needsMoreForPlayback(spans, null, true), false);
  assert.equal(needsMoreForPlayback([], 5, true), true);
});
