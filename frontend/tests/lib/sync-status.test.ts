// Run with: node --test tests/lib/sync-status.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { describeDevices, describeLastSync, type SyncDevice } from '../../src/lib/sync-status.ts';

const now = new Date('2026-10-27T15:00:00Z');
const ago = (seconds: number) => new Date(now.getTime() - seconds * 1000).toISOString();

test('last sync is described relative to now', () => {
  assert.equal(describeLastSync(null, now), 'Not synced yet');
  assert.equal(describeLastSync(ago(20), now), 'Synced just now');
  assert.equal(describeLastSync(ago(60 * 5), now), 'Synced 5 min ago');
  assert.equal(describeLastSync(ago(60 * 60 * 3), now), 'Synced 3 h ago');
  assert.match(describeLastSync(ago(60 * 60 * 30), now), /^Synced on /);
  assert.equal(describeLastSync('not a date', now), 'Not synced yet');
});

test('other computers are listed by name with their last sync', () => {
  const device = (name: string, seconds: number): SyncDevice => ({
    id: name, name, platform: 'macos', app_version: '0.2.22', updated: ago(seconds),
  });
  assert.equal(describeDevices([], now), 'No other computer has joined this library yet.');
  assert.equal(describeDevices([device('MacBook Air', 120)], now), 'Also used on MacBook Air (synced 2 min ago).');
  assert.equal(
    describeDevices([device('Air', 30), device('Studio', 60 * 60 * 2)], now),
    'Also used on Air (synced just now) and Studio (synced 2 h ago).',
  );
});
