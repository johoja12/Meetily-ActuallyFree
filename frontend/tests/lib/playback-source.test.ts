import { beforeEach, expect, test } from 'bun:test';

const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { window: { localStorage: storage } });
const { PLAYBACK_SOURCE_KEY, isSameFolder, playbackFile, readPlaybackSource, savePlaybackSource } = await import('../../src/lib/playback-source');
beforeEach(() => values.clear());

test('the cleaned room copy plays by default, and the choice is remembered', () => {
  expect(readPlaybackSource()).toBe('cleaned');
  savePlaybackSource('original');
  expect(values.get(PLAYBACK_SOURCE_KEY)).toBe('original');
  expect(readPlaybackSource()).toBe('original');
  values.set(PLAYBACK_SOURCE_KEY, 'garbage');
  expect(readPlaybackSource()).toBe('cleaned');
});

test('the original plays until a cleaned copy exists or when it is chosen', () => {
  const original = '/r/Meeting/audio.mp4';
  const cleaned = '/r/Meeting/room-clean.m4a';
  expect(playbackFile({ path: original, cleanedPath: null }, 'cleaned')).toBe(original);
  expect(playbackFile({ path: original }, 'cleaned')).toBe(original);
  expect(playbackFile({ path: original, cleanedPath: cleaned }, 'cleaned')).toBe(cleaned);
  expect(playbackFile({ path: original, cleanedPath: cleaned }, 'original')).toBe(original);
});

test('a cleaned-copy event matches only the recording in its folder', () => {
  expect(isSameFolder('/r/Meeting', '/r/Meeting/audio.mp4')).toBe(true);
  expect(isSameFolder('/r/Meeting/', '/r/Meeting/audio.mp4')).toBe(true);
  expect(isSameFolder('C:\\r\\Meeting', 'C:\\r\\Meeting\\audio.mp4')).toBe(true);
  expect(isSameFolder('/r/Meeting', '/r/Meeting 2/audio.mp4')).toBe(false);
  expect(isSameFolder('/r/Meeting', null)).toBe(false);
});
