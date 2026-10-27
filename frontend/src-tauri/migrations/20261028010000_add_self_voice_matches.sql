-- Speakers your own saved voice named "You", the counterpart of
-- voice_speaker_matches for the user, who has no contact row. An applied row
-- keeps that meeting from training your voice; a rejected row (undone, or
-- renamed away) keeps later passes from naming that label "You" again.
CREATE TABLE self_voice_matches (
    meeting_id TEXT NOT NULL,
    original_label TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('applied', 'rejected')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (meeting_id, original_label),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
