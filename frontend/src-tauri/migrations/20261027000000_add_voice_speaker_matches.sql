-- Names that saved voices gave to a meeting's unnamed speakers. An applied
-- row marks the contact's appearance in that meeting as automatic, so it never
-- trains their voice; a rejected row (undone, or renamed away by the user)
-- keeps later passes from giving that label the same contact again.
-- person_speakers is left unchanged: its schema is pinned by the people
-- migration checksum repair.
CREATE TABLE voice_speaker_matches (
    meeting_id TEXT NOT NULL,
    original_label TEXT NOT NULL,
    person_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('applied', 'rejected')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (meeting_id, original_label, person_id),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE,
    FOREIGN KEY (person_id) REFERENCES people(id) ON DELETE CASCADE
);

CREATE INDEX idx_voice_speaker_matches_person ON voice_speaker_matches(meeting_id, person_id);
