/**
 * What the app (not Rust) owns about the recording in progress: the title
 * shown in the header, and notes typed during the call. Rust keeps the name
 * the recording started with, so changes made mid-call live here until the
 * meeting is saved, then land on the saved meeting.
 */
import { defaultMeetingTitle, finalizeDefaultTitle, groupMeetingTitle } from '@/lib/meeting-titles';
import type { CalendarMeeting } from '@/lib/calendar-meetings';

const TITLE_KEY = 'af-live-title';
const NOTES_KEY = 'af-live-notes';
const CHANGE_EVENT = 'af-live-session';

export interface LiveTitle {
  title: string;
  /** The user typed it, so picking a group no longer retitles the meeting. */
  manual: boolean;
  /** When the recording started (ms), for the default title's time slot. */
  startedAt: number;
  /**
   * The calendar event the title came from. Linked to the meeting when it is
   * saved, so the speaker picker can offer the invite's attendees. Kept when
   * the title is edited; cleared by Undo.
   */
  calendarEvent?: CalendarMeeting | null;
}

export interface LiveNotes {
  markdown: string;
  json: unknown[] | null;
  updatedAt: number;
}

function read<T>(storage: Storage | undefined, key: string): T | null {
  try {
    const raw = storage?.getItem(key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}

function write(storage: Storage | undefined, key: string, value: unknown) {
  try {
    if (value === null) storage?.removeItem(key);
    else storage?.setItem(key, JSON.stringify(value));
  } catch {
    // Storage is best effort; the header still shows the title in memory.
  }
}

const session = () => (typeof window === 'undefined' ? undefined : window.sessionStorage);
// Notes survive a crash or reload of the window.
const local = () => (typeof window === 'undefined' ? undefined : window.localStorage);

export function readLiveTitle(): LiveTitle | null {
  const value = read<LiveTitle>(session(), TITLE_KEY);
  return value && typeof value.title === 'string' && value.title.trim() ? value : null;
}

export function writeLiveTitle(value: LiveTitle | null) {
  write(session(), TITLE_KEY, value);
  if (typeof window !== 'undefined') window.dispatchEvent(new CustomEvent(CHANGE_EVENT));
}

/** "Meeting · Mon, Sep 28 · 2:30 PM", or the group's "Weekly Standup — Sep 28". */
export function automaticTitle(startedAt: Date, groupName?: string | null, existingTitles: string[] = []): string {
  return groupName?.trim() ? groupMeetingTitle(groupName, startedAt, existingTitles) : defaultMeetingTitle(startedAt);
}

/**
 * The title to save when the recording ends: the user's own if they typed
 * one, otherwise the automatic title with its time slot completed.
 */
export function finalLiveTitle(fallback: string, endedAt: Date = new Date()): string {
  const live = readLiveTitle();
  if (!live) return fallback;
  if (live.manual) return live.title.trim();
  return finalizeDefaultTitle(live.title, new Date(live.startedAt), endedAt);
}

export function readLiveNotes(): LiveNotes | null {
  const value = read<LiveNotes>(local(), NOTES_KEY);
  return value && typeof value.markdown === 'string' ? value : null;
}

export function writeLiveNotes(notes: { markdown: string; json: unknown[] | null } | null) {
  write(local(), NOTES_KEY, notes ? { ...notes, updatedAt: Date.now() } : null);
}

/** A new recording starts with its automatic title and an empty page. */
export function beginLiveSession(title: string, startedAt: number, manual = false, calendarEvent: CalendarMeeting | null = null) {
  writeLiveNotes(null);
  writeLiveTitle({ title, manual, startedAt, ...(calendarEvent ? { calendarEvent } : {}) });
}

/** The user typed a title. Any calendar event stays linked. */
export function renameLiveTitle(title: string) {
  const live = readLiveTitle();
  writeLiveTitle({ ...live, title, manual: true, startedAt: live?.startedAt ?? Date.now() });
}

/** The user picked a calendar event for the recording in progress. */
export function chooseLiveCalendarEvent(event: CalendarMeeting) {
  const live = readLiveTitle();
  writeLiveTitle({ ...live, title: event.title, manual: true, startedAt: live?.startedAt ?? Date.now(), calendarEvent: event });
}

/**
 * Undo the automatic calendar name for event `eventId`: back to the automatic
 * title, no event. Refused once the user typed a title or chose another event
 * ('renamed'), or after the recording ended ('ended').
 */
export function undoCalendarTitle(automatic: string, eventId: string): 'undone' | 'renamed' | 'ended' {
  const live = readLiveTitle();
  if (!live) return 'ended';
  if (live.calendarEvent?.id !== eventId || live.title !== live.calendarEvent.title) return 'renamed';
  writeLiveTitle({ ...live, title: automatic, manual: false, calendarEvent: null });
  return 'undone';
}

/** "Not this invite": keep the title, stop offering the event's attendees. */
export function clearLiveCalendarEvent() {
  const live = readLiveTitle();
  if (live) writeLiveTitle({ ...live, calendarEvent: null });
}

/**
 * Links the live title's calendar event to the meeting just saved. Call before
 * the live session is cleared. Returns false when there is no event.
 */
export async function linkLiveCalendarEvent(meetingId: string, call: (command: string, args: Record<string, unknown>) => Promise<unknown>): Promise<boolean> {
  const event = readLiveTitle()?.calendarEvent;
  if (!event) return false;
  await call('link_meeting_calendar_event', { meetingId, event });
  return true;
}

export function endLiveSession() {
  writeLiveNotes(null);
  writeLiveTitle(null);
}

/** Subscribe to title changes made anywhere in this window. */
export function onLiveSessionChange(handler: () => void): () => void {
  window.addEventListener(CHANGE_EVENT, handler);
  return () => window.removeEventListener(CHANGE_EVENT, handler);
}
