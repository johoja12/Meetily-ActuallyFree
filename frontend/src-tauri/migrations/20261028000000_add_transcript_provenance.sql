-- How a meeting's saved transcript was produced, written by the native step
-- that produced it when that step commits. NULL means unknown: meetings saved
-- before this migration (or by an import) are not back-filled with guesses.
--   transcript_pass: 'live' (streamed while recording) or 'enhanced' (full
--     retranscription of the saved recording).
--   transcript_provider / transcript_model: the engine of that pass, as the
--     transcript settings name it (e.g. 'parakeet', 'parakeet-tdt-0.6b-v3-int8').
--   speakers_identified_by: diarization engine ('pyannote' | 'nemotron') whose
--     labels are on the current rows; cleared when a retranscription replaces them.
ALTER TABLE meetings ADD COLUMN transcript_pass TEXT;
ALTER TABLE meetings ADD COLUMN transcript_provider TEXT;
ALTER TABLE meetings ADD COLUMN transcript_model TEXT;
ALTER TABLE meetings ADD COLUMN speakers_identified_by TEXT;
