import type { CalendarAttendee } from '@/lib/calendar-attendees';

/** Calendar data stays out of transcripts until the user chooses a title. */
export interface CalendarMeeting {
  id: string;
  title: string;
  calendar_id: string;
  calendar_name: string;
  start_ms: number;
  end_ms: number;
  is_meeting: boolean;
  /** Everyone invited except the user, rooms and people who declined. */
  attendees?: CalendarAttendee[];
}

const PENDING_KEY = 'af-calendar-recording-title';
const MAX_SELECTION_AGE_MS = 10 * 60_000;

/** Keep overlapping events separate; timing alone cannot identify the attended call. */
export function calendarTitleSuggestions(events: CalendarMeeting[], referenceMs: number): CalendarMeeting[] {
  if (!Number.isFinite(referenceMs)) return [];
  const unique = new Map<string, CalendarMeeting>();
  for (const event of events) {
    if (!event.id || !event.title.trim() || !Number.isFinite(event.start_ms) ||
        !Number.isFinite(event.end_ms) || event.end_ms <= event.start_ms) continue;
    if (event.start_ms > referenceMs + 15 * 60_000 || event.end_ms <= referenceMs) continue;
    unique.set(`${event.id}:${event.start_ms}`, event);
  }
  return [...unique.values()].sort((a, b) => {
    const aActive = a.start_ms <= referenceMs;
    const bActive = b.start_ms <= referenceMs;
    return Number(bActive) - Number(aActive) ||
      Math.abs(a.start_ms - referenceMs) - Math.abs(b.start_ms - referenceMs) ||
      a.title.localeCompare(b.title);
  });
}

/** A confirmed calendar prompt hands its title to the existing recording-start path. */
export function selectCalendarRecording(event: CalendarMeeting, now = Date.now()): void {
  sessionStorage.setItem(PENDING_KEY, JSON.stringify({ event, selectedAt: now }));
}

export function takeCalendarRecording(now = Date.now()): CalendarMeeting | null {
  let raw: string | null;
  try {
    raw = sessionStorage.getItem(PENDING_KEY);
    sessionStorage.removeItem(PENDING_KEY);
  } catch { return null; }
  if (!raw) return null;
  try {
    const { event, selectedAt } = JSON.parse(raw) as { event: CalendarMeeting; selectedAt: number };
    if (!Number.isFinite(selectedAt) || now < selectedAt || now - selectedAt > MAX_SELECTION_AGE_MS) return null;
    return calendarTitleSuggestions([event], now).length === 1 ? event : null;
  } catch { return null; }
}

const AUTO_LEAD_MS = 5 * 60_000;

export type AutoCalendarChoice = { kind: 'one'; event: CalendarMeeting } | { kind: 'many'; count: number } | null;

/**
 * What a new recording does with the calendar: one current meeting names it,
 * several are offered as a choice. Plain events (no attendees or meeting link,
 * such as focus time) never name a recording on their own.
 */
export function autoCalendarChoice(events: CalendarMeeting[], now: number): AutoCalendarChoice {
  // The menu offers meetings up to 15 minutes ahead; naming without asking
  // needs one that started or starts within 5.
  const meetings = calendarTitleSuggestions(events.filter(event => event.is_meeting && event.start_ms <= now + AUTO_LEAD_MS), now);
  if (meetings.length === 1) return { kind: 'one', event: meetings[0] };
  return meetings.length > 1 ? { kind: 'many', count: meetings.length } : null;
}

/** Asks the recording header to open its Calendar title menu. */
export const OPEN_CALENDAR_TITLE_EVENT = 'af-open-calendar-title';
const OPEN_MENU_KEY = 'af-open-calendar-title';
const OPEN_MENU_MAX_AGE_MS = 2 * 60_000;

/**
 * Opens the recording header's Calendar title menu: right away on the recorder,
 * otherwise after navigating there (the menu takes the request when it mounts).
 */
export function requestCalendarTitleMenu(navigate: (href: string) => void, now = Date.now()) {
  if (window.location.pathname === '/') {
    window.dispatchEvent(new CustomEvent(OPEN_CALENDAR_TITLE_EVENT));
    return;
  }
  try { sessionStorage.setItem(OPEN_MENU_KEY, String(now)); } catch { /* The menu stays one click away. */ }
  navigate('/');
}

/** A pending request from another page, taken once and only while recent. */
export function takeCalendarTitleMenuRequest(now = Date.now()): boolean {
  let raw: string | null = null;
  try { raw = sessionStorage.getItem(OPEN_MENU_KEY); sessionStorage.removeItem(OPEN_MENU_KEY); } catch { return false; }
  const at = Number(raw);
  return !!raw && Number.isFinite(at) && Math.abs(now - at) <= OPEN_MENU_MAX_AGE_MS;
}
