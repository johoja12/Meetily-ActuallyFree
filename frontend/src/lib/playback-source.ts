/**
 * Which copy of a room-microphone recording to play: the dereverberated copy
 * written after the call (`room-clean.m4a`, see ROOM_MICROPHONE.md) or the
 * original. One choice per computer; the cleaned copy is the default.
 */
export type PlaybackSource = 'cleaned' | 'original';

export const PLAYBACK_SOURCE_KEY = 'roomPlaybackSource';

export function readPlaybackSource(): PlaybackSource {
  if (typeof window === 'undefined') return 'cleaned';
  try {
    return window.localStorage.getItem(PLAYBACK_SOURCE_KEY) === 'original' ? 'original' : 'cleaned';
  } catch {
    return 'cleaned';
  }
}

export function savePlaybackSource(source: PlaybackSource): void {
  try {
    window.localStorage.setItem(PLAYBACK_SOURCE_KEY, source);
  } catch {
    // Private storage unavailable: the choice lasts for this page only.
  }
}

/** The file to play: the cleaned copy when it exists and is preferred. */
export function playbackFile(
  audio: { path: string | null; cleanedPath?: string | null },
  source: PlaybackSource,
): string | null {
  return source === 'cleaned' && audio.cleanedPath ? audio.cleanedPath : audio.path;
}

/** Whether a `room-playback-cleaned` event's folder holds this recording. */
export function isSameFolder(folder: string, file: string | null): boolean {
  if (!file) return false;
  const trim = (value: string) => value.replace(/[\\/]+$/, '');
  const dir = file.replace(/[\\/][^\\/]*$/, '');
  return trim(dir) === trim(folder);
}
