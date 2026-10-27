/**
 * Whether the card shown after a recording identifies speakers on its own
 * (the same as clicking "Auto-detect & continue") instead of asking. A
 * per-computer convenience kept in this window's storage; the post-call card
 * that acts on it runs in the window too.
 */
const KEY = 'af-post-call-auto-identify';
export const AUTO_IDENTIFY_CHANGED_EVENT = 'af-post-call-auto-identify-changed';

export function readAutoIdentifySpeakers(): boolean {
  try {
    return localStorage.getItem(KEY) === '1';
  } catch {
    return false;
  }
}

export function writeAutoIdentifySpeakers(value: boolean) {
  try {
    if (value) localStorage.setItem(KEY, '1');
    else localStorage.removeItem(KEY);
  } catch {
    // Without storage the card keeps asking.
  }
  window.dispatchEvent(new Event(AUTO_IDENTIFY_CHANGED_EVENT));
}
