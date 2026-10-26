# Room microphone capture

The Room microphone switch is available under Settings > Recording and in the
record card's microphone settings. It is off for legacy preferences and applies
to the next recording. Use it when a microphone hears both the local person and
nearby conference-device speakers (for example a native Cisco Desk Pro call).
The Desk Pro stays in its native call; Meetily does not connect to its APIs.

## Data flow and ownership

`RecordingPreferences.room_microphone` is persisted by the existing native
preferences command. The frontend waits for save completion before showing the
new setting and disables the record-card start button while that save is pending.
The record card shares its device/gain save queue with mode saves so a delayed
read/modify/write cannot restore an earlier mode; settings disables competing
preference writes while the mode is saving.
Both native start entrypoints snapshot it into the new `RecordingState` before
capture. Changing preferences does not change an existing worker's mode.

`RecordingManager` requires a microphone, omits system capture and per-app audio
for the session, and initializes `RecordingSaver` with the mode. The saver writes
`metadata.json.room_microphone` in the initial atomic metadata write, before
capture starts, including when Save audio is off. It still retains actual
microphone provenance (`mic.mp4`) and existing audio/transcript timestamp units.
Stored system/per-app preferences are preserved for personal microphone mode.

The continuous VAD observer feeds the selected Nemotron worker from microphone
rather than system samples in room mode. `DeviceType.feeds_diarization` selects
exactly one source, also on final drain. The existing bounded worker queue,
recording-relative clock, final flush, and retained timeline are unchanged.
Pyannote uses its existing per-turn embedding path with `mic_dominant=false`:
room audio does not enroll a whole room as the local user's voice. Personal mode
keeps its microphone-to-You contract.

The transcription worker diarizes room speech with the selected engine. When
models are missing or labeling fails, the visible error remains and the fallback
is Room, never You. No other engine is silently substituted. Room is a source
label, excluded from contact creation, named-speaker transfer, and attendee links.

Post-call diarization reads the saved mode and bypasses the separate-track
single-local-user assumption for room meetings, diarizing the mixed recording.
Retranscription uses the room microphone track when present (otherwise the saved
playback file), with Room as its source hint. Named voice enrollment explicitly
uses mic.mp4 for room meetings, with the existing opt-in and clean-turn checks.
Older meetings without the metadata flag retain existing behavior.
The home permission banner omits computer-audio advice in room mode, and its
permission recheck requests only microphone access.

## Qualification and limits

Frontend regression coverage exercises legacy defaults, persistence/reload,
preservation of other preferences, pending/failed saves, unreadable preferences,
the accessible disabled control, and anonymous Room labels. Native regressions
cover source selection, state initialization, initial metadata persistence,
legacy metadata, preference round trips, and room retranscription source choice.
Run frontend files in separate processes as described in DEVELOPMENT_MAP.md.
Native filters: `room_microphone`, plus affected `audio::pipeline`,
`audio::retranscription`, `audio::recording_saver`, and diarization label tests.

Verification on macOS: all 35 isolated frontend test files passed; the permission
group passed again with its added room-mode case (nine tests), and the room-mode
group passed with six tests, including a pending device-save race. TypeScript and
the production frontend build passed. The native library suite passed 378 tests
with eight opt-in tests ignored, including all four new room-mode regressions.
For library tests only, `TAURI_CONFIG='{"bundle":{"externalBin":[]}}'` omitted
external-binary bundling because this checkout had no built llama-helper. This
does not qualify an installer or summary-sidecar packaging. Browser preview
confirmed the microphone popover switch and room indicator, with output controls
hidden when enabled. All fixtures were synthetic/unit fixtures; no physical
recording or optional model/audio tests ran.

This is acoustic capture, not separated audio received from the conference
service. Room echo, placement, low speaker volume, device noise cancellation,
and overlapping voices affect transcription and speaker matching. Each live ASR
turn still receives one greatest-overlap speaker label; it is not split into
word-timed speakers. No physical Desk Pro or model/audio accuracy qualification
has been performed for this feature. A test call must verify alternating local
and remote speech, overlap, saved playback, Stop, and post-call speaker labels.

## Local installation and initial hardware observation (October 26)

Built using `frontend/scripts/build-macos-apple-silicon.sh` with the OpenSSL 3
include/library environment from the macOS CI workflow. This local v0.2.22 build
is installed in `/Applications/Meetily - Actually Free.app`; no release was
published. The installed meetily/llama-helper/ffmpeg hashes matched the DMG
payload. Deep signature verification, arm64/dependency checks, the packaged
permission helper, llama-helper ping/shutdown, and a generated-silence FFmpeg
encode/decode check passed. The final 35 isolated frontend files passed.

The installed Room microphone indicator and disabled in-session mode switch were
observed during the user's recording. Initial capture from Desk Pro Web Camera
was near-silent (two 30-second microphone checkpoints measured RMS approximately
0.000162, with microphone gain already at 3x). This establishes that the selected
input did not supply useful room speech during that observation; it does not
establish a Desk Pro firmware limitation or a diarization/ASR accuracy result.
Mac mini has no built-in microphone. Acoustic capture requires a suitable external
input (for example a nearby iPhone microphone or a USB room microphone); the
Desk Pro USB webcam input must not be assumed to export a native call's mix.
No private recording fixtures or logs are included in the repository.

Follow-up: with the user's nearby iPhone selected as the microphone, the installed
room-mode recording produced live transcript text labeled Speaker 1. This is a
live acoustic capture/ASR/labeling observation, not multi-speaker accuracy or native
Desk Pro USB mix qualification. The recording and transcript remain private and
were not copied into this repository.
