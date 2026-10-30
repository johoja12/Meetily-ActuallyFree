/**
 * Is the recorded meeting over? Signals, strongest first: the meeting app hung
 * up (native `meeting-ended`), both audio sources silent, goodbye phrases in the
 * live transcript, each weighted by the calendar end time. The answer only
 * prompts (or, opted in, stops after a countdown); it never trims audio.
 * Times are epoch milliseconds.
 */

/** About -40 dBFS on either source counts as sound (speech, or a call). */
export const SOUND_RMS = 0.01;
const SILENCE_BEFORE_END_MS = 3 * 60_000;
const SILENCE_AFTER_END_MS = 60_000;
const QUIET_AFTER_GOODBYE_MS = 15_000;
/** Goodbyes count only near the calendar end (or always, without an event). */
const GOODBYE_NEAR_END_MS = 5 * 60_000;
const GOODBYE_WINDOW_MS = 90_000;
/** Level events normally arrive many times a second; a gap means paused or stalled. */
const LEVELS_STALE_MS = 5_000;
export const SNOOZE_MS = 10 * 60_000;

export interface EndState {
  startedAt: number;
  calendarEndMs: number | null;
  lastSoundAt: number;
  lastLevelAt: number | null;
  goodbyes: number[];
  appEnded: { at: number; app: string } | null;
  snoozedUntil: number | null;
}

export type EndReason = { reason: 'app'; app: string } | { reason: 'silence'; quietMs: number } | { reason: 'goodbye' };

export function freshEndState(startedAt: number, calendarEndMs: number | null): EndState {
  return { startedAt, calendarEndMs, lastSoundAt: startedAt, lastLevelAt: null, goodbyes: [], appEnded: null, snoozedUntil: null };
}

export function noteLevel(state: EndState, rms: number, at: number): EndState {
  // Levels resuming after a gap (a pause) restart the quiet clock.
  const resumed = state.lastLevelAt != null && at - state.lastLevelAt > LEVELS_STALE_MS;
  return { ...state, lastLevelAt: at, lastSoundAt: rms >= SOUND_RMS || resumed ? at : state.lastSoundAt };
}

export function noteGoodbye(state: EndState, at: number): EndState {
  return { ...state, goodbyes: [...state.goodbyes.filter((time) => at - time <= GOODBYE_WINDOW_MS), at] };
}

const GOODBYE =
  /\b(?:bye(?:[- ]bye)?|good ?bye|see (?:you|ya)(?: (?:all|soon|later|next \w+|tomorrow))?|talk (?:to you )?(?:soon|later|tomorrow)|take care|catch you later|thanks?(?: you)?,? (?:everyone|everybody|all|guys|team|folks)|have a (?:good|great|nice|lovely) (?:one|day|weekend|evening|night|rest of (?:the|your) \w+))\b/i;

export function isGoodbye(text: string): boolean {
  return GOODBYE.test(text);
}

export function endReason(state: EndState, now: number): EndReason | null {
  if (state.snoozedUntil != null && now < state.snoozedUntil) return null;
  if (state.appEnded) return { reason: 'app', app: state.appEnded.app };
  // Without fresh level events (paused, or a stalled stream) silence is unknown.
  if (state.lastLevelAt == null || now - state.lastLevelAt > LEVELS_STALE_MS) return null;
  const quietMs = now - state.lastSoundAt;
  const pastEnd = state.calendarEndMs != null && now >= state.calendarEndMs;
  if (quietMs >= (pastEnd ? SILENCE_AFTER_END_MS : SILENCE_BEFORE_END_MS)) return { reason: 'silence', quietMs };
  const nearEnd = state.calendarEndMs == null || now >= state.calendarEndMs - GOODBYE_NEAR_END_MS;
  const recent = state.goodbyes.filter((time) => now - time <= GOODBYE_WINDOW_MS);
  if (nearEnd && recent.length >= 2 && quietMs >= QUIET_AFTER_GOODBYE_MS) return { reason: 'goodbye' };
  return null;
}

/** After Keep recording: no prompt for a while, and old goodbyes no longer count. */
export function snooze(state: EndState, now: number): EndState {
  return { ...state, snoozedUntil: now + SNOOZE_MS, goodbyes: [], appEnded: null };
}

/**
 * What to do once the meeting seems over. A recording automation started from
 * the calendar has no app to hang up, so it always counts down and stops,
 * even with the prompt turned off; Keep recording still wins.
 */
export function endAction(prefs: { prompt: boolean; autoStop: boolean }, calendarAutomation: boolean): { show: boolean; autoStop: boolean } {
  if (calendarAutomation) return { show: true, autoStop: true };
  return { show: prefs.prompt, autoStop: prefs.prompt && prefs.autoStop };
}

const PROMPT_KEY = 'af-meeting-end-prompt-off';
const AUTO_STOP_KEY = 'af-meeting-end-auto-stop';
export const MEETING_END_PREFS_EVENT = 'af-meeting-end-prefs-changed';

/** Prompting is on unless turned off; stopping without an answer is opt-in. */
export function readMeetingEndPrefs(): { prompt: boolean; autoStop: boolean } {
  try {
    return { prompt: localStorage.getItem(PROMPT_KEY) !== '1', autoStop: localStorage.getItem(AUTO_STOP_KEY) === '1' };
  } catch {
    return { prompt: true, autoStop: false };
  }
}

export function writeMeetingEndPrefs(next: { prompt: boolean; autoStop: boolean }) {
  try {
    if (next.prompt) localStorage.removeItem(PROMPT_KEY);
    else localStorage.setItem(PROMPT_KEY, '1');
    if (next.autoStop) localStorage.setItem(AUTO_STOP_KEY, '1');
    else localStorage.removeItem(AUTO_STOP_KEY);
  } catch {
    // Without storage the defaults apply.
  }
  window.dispatchEvent(new Event(MEETING_END_PREFS_EVENT));
}
