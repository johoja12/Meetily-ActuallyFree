-- The calendar event a meeting was named from, with the invite's attendees
-- as a JSON array of {name, email, organizer}. One per meeting; choosing
-- another event replaces it. Attendees are a snapshot taken when linked.
CREATE TABLE meeting_calendar_events (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    event_id TEXT NOT NULL,
    title TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    attendees TEXT NOT NULL DEFAULT '[]',
    linked_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
