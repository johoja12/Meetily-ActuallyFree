# Labs: MacWhisper roadmap features 1, 3, 7, 8, 12

This note reviews the proposed architecture in
`H:/opencode/meetily/CHANGES_0.2.17_AND_MACWHISPER_ROADMAP.md` against this
branch. The attachment is a roadmap and historical inventory, not an
implementation specification or proof that a feature is present in a release.
The current branch already has per-app capture, live/post-call speaker editing,
VAD silence rejection, some Whisper repetition guards, a local-user voiceprint,
recording-relative transcript turn times, and process-based meeting prompts.

## Where each feature lives in the app

All switches are in Settings > Labs, and each feature also shows where it is
used:

- Meeting automation: a switch in Settings > Meeting detection as well. A
  notice says when a call starts a recording (with an option to keep it going
  after the call) and when the call's end stops it. The stop runs the
  recorder's normal stop and save from any page, without reloading.
- Audio and transcript seeking: the meeting player shows the waveform instead
  of a plain track and adds 0.5× and 0.75×. Seeking from a line's timestamp
  and following playback work without Labs.
- Named voice profiles: naming a speaker learns that contact's voice in the
  background, and matching unnamed speakers in other meetings get the name
  with an Undo notice (see "Automatic learning and matching" below). Settings >
  Labs shows "Learning voices…" and an "Apply voices to past meetings" button.
  A Voice panel on each contact's page (`ContactVoicePanel`) also learns a voice
  from all their recent meetings (up to twelve), updates it the same way so new
  meetings count, or forgets it. It shows whether a voice is saved even with the
  Labs switch off, and learning then turns the switch on (`learnVoiceForContact`,
  which turns it back off if learning fails). Before learning it reads
  `voice_learning_candidates(person_id)`: meetings where you named the contact
  (voice-matched ones never count), how many still have call audio, and whether
  the speaker models are downloaded; `voiceLearnBlocker` turns that into the
  reason Learn is disabled. The Contacts list marks contacts with a saved voice,
  filters by Voice saved / No voice, and offers Learn or Update voice per row; the speaker card on a meeting page adds that
  meeting's audio (Remember voice, or Update voice once one exists). A profile
  keeps each meeting's share, so learning from a meeting again replaces its
  share instead of counting it twice; profiles saved before shares were kept
  count as one earlier share. Renaming, merging or deleting a contact updates,
  combines or removes their voice.
- Whisper silence guard and Parakeet GPU: tagged on the engine they change in
  Settings > Transcription.
- Clean transcript: a Clean/Verbatim switch in the meeting player. With Labs
  off the transcript still hides simple fillers, as before.

## Feature 1: meeting automation

`meeting_detection.rs` now includes packaged Teams microphone/camera leases and
Windows browser meeting window titles alongside the existing process and
NonPackaged lease checks. Blocking scans run in `spawn_blocking`, and a monitor
generation prevents duplicate loops after settings changes. The detector emits
`meeting-detected` with `active_media` and `meeting-ended` after 45 seconds of
missing signals. Labs auto-start acts only on an active Windows media lease;
process-only detection still prompts. The frontend stores ownership only after
a successful start and runs the usual stop/save workflow only for an owned
recording. Enabling Labs meeting automation also enables the existing Detection
monitor; switching Labs off leaves ordinary detection prompts available.

The plan's proposal to bind the detected PID directly to per-app capture is not
used. Browser PIDs can contain unrelated tabs, and process capture settings are
independently selected by the user. The detector does not yet meter WASAPI
sessions or distinguish a muted live call from an idle app with an open media
lease. Linux has no active-media automation signal here.

On macOS (14 and later), detection no longer treats an open app as a meeting:
Webex, Teams and Slack stay open for chat all day. `macos_audio_clients` reads
CoreAudio's client process list (`kAudioHardwarePropertyProcessObjectList`, no
extra permission) and names each process by its outermost `.app`, so helpers
count for their app. A known meeting app counts only while it runs input and
output at once (`call_app_match`), on any device, a USB headset or a Desk Pro in
USB mode included. Input alone is not a call: Webex keeps the microphone open
for proximity pairing. A match is an active-media signal, so Labs automation
now auto-starts on macOS too. Open audio alone was not enough in practice: Webex opens
microphone and speakers for its join preview at the meeting's start, and
automation recorded an empty lobby. A new call is therefore reported only once
the recording microphone hears speech (`calendar::listen` sampling: 10 s, two
seconds at or above 0.01 RMS), re-checked each scan until it does; an already
reported call is not re-checked, and an unreadable microphone falls back to
reporting the call. When the query is unavailable the old
process-only check is used. Not detected: a call running on a paired Desk Pro
or room device, where no call audio reaches the Mac (calendar reminders still
offer Record), and browser meetings. Covered by unit tests on pure functions; a
live probe on macOS 27 confirmed the query works and showed idle Webex holding
input without output. A detection
event is not proof a meeting is underway; users should test the Labs action
against their conferencing apps before relying on it.

### Noticing when a meeting ends

`components/recording/MeetingEndWatcher.tsx` (main window only) watches any
running recording and asks "Stop & save / Keep recording". The decision is pure
and tested in `lib/meeting-end.ts`:
- the meeting app hung up (`meeting-ended`, emitted 45 s after a known app stops
  using microphone and speakers), unless meeting automation owns that recording;
- both sources quiet (`recording-audio-levels` RMS below 0.01, about -40 dBFS)
  for 3 minutes, or 1 minute once the linked calendar event's end has passed
  (`readLiveTitle().calendarEvent.end_ms`);
- two goodbye phrases in the final live transcript within 90 s, then 15 s of
  quiet, within 5 minutes of the calendar end or at any time without an event.
  This needs live transcription; post-call-only recordings rely on the others.
A pause, or a gap in level events, restarts the quiet clock; stale levels never
count as silence. Keep recording (or closing the prompt) snoozes it for 10
minutes and forgets the goodbyes. Settings > Meeting detection has the switch
(on by default) and an opt-in "Stop and save automatically if I don't answer
within 1 minute". Stopping uses the normal stop-and-save path. Prompts live in
the WebView; a closed window loses only the prompt, never capture. Thresholds
are not yet qualified against real calls; a muted listener in a long silent
stretch will be asked.

## Feature 3: audio and transcript seeking

`get_meeting_playback_audio` resolves the saved mixed audio for a meeting.
The asset protocol streams it to an HTML audio element, while a worker streams
8 kHz mono PCM through FFmpeg to compute one approximate peak per second.
One worker owns waveform extraction at a time; additional requests report busy.
The UI tracks recording-relative seconds, seeks from a transcript turn
timestamp or the waveform, highlights the active turn, and supports 0.5–2× playback.
A turn merges a speaker's consecutive segments but keeps each one as a part
(`Turn.parts`): clicking a part's text seeks to that segment (a text selection
never seeks), the part playing now is highlighted, and Follow scrolls part by
part so a long turn keeps the spoken line in view. Saved transcripts load in
pages of 100, so playback or a seek past the loaded part loads the next page,
repeating until it is covered (`needsMoreForPlayback` in
`lib/transcript-playback.ts`); before this, highlight and follow stopped after
the first page. `tests/lib/transcript-playback.test.ts` covers the active-span
search and the load trigger; the scrolling itself is not covered by tests.

The roadmap's `TokenData` claim does not apply to the current persisted data:
transcript turns have start/end seconds, but no verified per-word timings.
The UI therefore seeks/highlights turns, never invents word offsets. Waveform
extraction is on demand and may take time for a long recording; its peak history
is bounded to 24 hours. No word-level karaoke claim is made.

Windows issue #43: `audio/waveform.rs::extract_peaks` launches FFmpeg with
`CREATE_NO_WINDOW`, matching the other audio decoding paths. Waveform requests
from the meeting player can spawn this process; a title update itself does not.
The launch flag preserves the piped PCM output and existing worker ownership.
Verification uses the native waveform unit test and Windows compilation; the
unit test checks peak binning, not visible console behavior. Installed-GUI
verification still requires opening an uncached waveform on Windows.

## Feature 7: named voice profiles

The existing `voiceprint.rs` belongs to the microphone user only. New
`voice_profiles.rs` stores opt-in WeSpeaker post-LDA embeddings for explicitly
named people, keyed by the existing `people` and `person_speakers` identity.
Enrollment uses the separate system track and, from each meeting, up to twenty
non-overlapping 2–15 second turns bearing that person's saved speaker label,
spread across the meeting. A voice keeps a share per meeting (up to twelve) and
is the mean of all their turns; it requires at least two successful embeddings. The model runs on a blocking worker; capture
does no inference. Profiles are saved in local app data, can be listed and
deleted in Labs, and matching is disabled by default.

On a future Pyannote live session, the remote centroid must clear cosine 0.80
and exceed the next profile by 0.08 before a name is assigned. For Nemotron
live sessions, a separate WeSpeaker matcher compares 2–15 second system-audio
speech turns on the transcription worker and caches a verified name for that
meeting-local channel. Nemotron still owns diarization; its channel number is
never treated as identity. Post-call diarization with either engine compares
at least two clean system-track turns per remote channel against the profiles.
Post-call transcription alone does not run this matching pass (the background
pass described below does, from transcript spans). The microphone
still labels `You`. The person link is persisted with the transcript when a
profile name is used. Profiles are a consent-based convenience, not verified
identity. Enrollment and matching need clean turns, a separate system track,
and the WeSpeaker model. Short, overlapping, or uncertain turns stay unnamed.
Profile changes take effect next call for live matching and the next post-call
diarization pass for an existing recording.

### Automatic learning and matching

Design: [specs/2026-10-27-auto-learn-voices-design.md](superpowers/specs/2026-10-27-auto-learn-voices-design.md).
`diarization/voice_learning.rs` owns one native worker (started in app setup)
for all automatic work, so it completes even if the WebView reloads. Callers
only enqueue; requests coalesce by meeting/label, person/meeting and meeting.

- Learn: `rename_meeting_speaker` and `reassign_transcript_speaker` enqueue
  the new label when it is a person name; `api_save_transcript` enqueues the
  named labels the save linked, except names live matching gave out during
  that recording (`voice_profiles::take_live_names`). Learning reuses
  `meeting_share` + `store_shares`; failures are logged only.
- Forget: when an edit leaves a contact unlinked in a meeting, that meeting's
  share is removed (`drop_meeting_share`); a voice under two turns is forgotten.
- Match: after any change to a voice, after "Apply voices to past meetings",
  after a save and after `diarize_meeting`, meetings are matched from their
  own transcript spans without re-diarization. Generic single labels
  (`Speaker N`, including components of combined labels) need two clean 2–15 s
  turns from `voice_profiles::call_track` (`mic.mp4` in room mode, else
  `system.mp4`); the mean is scored with `best_match_scored` (same 0.80/0.08
  rule). Contacts already in the meeting and rejected pairs are skipped, one
  label per contact (strongest) is kept, and the rename goes through
  `PeopleRepository::rename_meeting_speaker` under the diarization operation
  guard. This is how room-microphone meetings get post-call names.
- `voice_speaker_matches` (migration `20261027000000`) records each automatic
  name as `applied`. Those meetings never train the contact's voice (auto-learn
  and Learn voice skip them); Remember voice on such a speaker confirms it and
  deletes the row. Undo (`undo_voice_matches`) restores the original label, or
  the next free `Speaker N`, and marks the row `rejected`; a manual rename that
  removes an automatic name also rejects it. Merge moves rows to the kept
  contact. `person_speakers` is unchanged because the people-migration
  checksum repair pins its schema.
- UI: `VoiceMatchNotifier` (mounted in the root layout) turns
  `voice-matches-applied` into a toast with Undo and a
  `meetily-voice-matches-changed` window event; an open meeting page refetches
  its transcript when its id is included. `voice-profiles-changed` refreshes
  voice lists.

Tests: `cargo test -p meetily --lib -- voice_learning voice_profiles person::`
covers label selection, choice rules, coalescing, share removal, clean turns,
and an in-memory apply → undo → no-reapply round trip and manual rejection;
`tests/lib/voice-match-summary.test.ts` covers the notice wording. No
model/audio test was added: real WeSpeaker matching on transcript spans is not
exercised by an automated test.

Limits: crosstalk-heavy meetings may lack two clean turns per label; voices
learned from room-microphone audio may score lower against call audio (and the
reverse); a wrong match at the existing threshold is possible, and Undo is the
remedy.

### Your own voice

You have no contact row, so your voice profile is kept in `voice_profiles.json`
under the reserved id `voice_profiles::SELF_ID` (`"self"`, shown as `You`;
`SELF_VOICE_ID` in `lib/voice-profiles.ts`). It is built like a contact's
voice, but from your `You` lines on `mic.mp4`, in every recording mode.

- Learn: Learn voice in the "Your voice" panel (Settings > Labs > Voice
  profiles) rebuilds it from up to twelve recent meetings with `You` lines
  (`enroll_person_voice` with `personId: "self"`); Remember voice on your own
  speaker card adds one meeting (`enroll_voice_profile` with speaker `You`).
  "This is me" (a rename to `You`) enqueues learning like naming a contact.
  Saving a meeting does not learn your voice by itself, because the mic label is
  assigned by audio source, not by you.
- Match: only the background worker uses it, and only in room-microphone
  meetings, where you are on the matched track. There it can rename one
  `Speaker N` to `You`. `load_for_matching` leaves your voice out, so live
  matching, post-call diarization matching, and call-track meetings never name
  a call speaker `You`; `load_all_for_matching` includes it for the worker.
- `self_voice_matches` (migration `20261028010000`, synced, removed with its
  meeting) is the counterpart of `voice_speaker_matches` for you, since that
  table's `person_id` must reference `people`. Undo, rejection on renaming
  `You` away, and "a matched meeting never trains the voice" work the same way.
- Contact reconciliation and sync's hold-back of voices whose contact has not
  arrived both keep `self`.

Separate from this, `voiceprint.rs` still keeps an automatic, hidden user
voiceprint, updated from the mic track after two-track diarization and used to
pick the `You` cluster when only one file is diarized. The two are not merged.

Tests: `voice_learning::tests::your_voice_*` and `renaming_you_away_*`
(in-memory apply, undo, rejection, "This is me" learning),
`voice_profiles::candidate_tests::your_candidates_*` (mic audio counts, call
audio does not), reconcile and sync hold-back cases, and the frontend
`contact-voices` and `contact-voice-panel` tests. The ignored
`voice_profiles::tests::real_audio_your_voice_names_you_and_not_someone_else`
learns your voice from one local recording (`MEETILY_SELF_VOICE_LEARN`) and
checks the thresholds name you in another (`MEETILY_SELF_VOICE_PROBE`) and not
someone else (`MEETILY_SELF_VOICE_OTHER`), using the installed WeSpeaker models
and fixed 6-second turns. It does not exercise transcript turn selection, and
its fixtures are private recordings that are never committed.

Live manual names now create person links when the meeting is saved. For older
meetings whose transcript has a name but lacks that link, enrollment repairs the
link using the saved label. Only generic labels prompt the user to name the
speaker; a named label missing from the saved transcript reports that condition.

## Feature 8: Whisper silence guard

The branch already uses VAD/energy checks, repetition cleanup, Whisper
`set_logprob_thold(-1.0)`, entropy threshold 2.4, and no-speech threshold 0.55.
Labs persists a stricter no-speech threshold of 0.45, read atomically by both
Whisper decode entry points. This may reject quiet speech; it does not imply
confidence calibration. The plan's `compression_ratio_threshold` call is not
available in the pinned whisper-rs API, and token suppression/temperature
fallback claims are not added without verified support.

## Feature 12: clean transcript view

The virtualized view previously stripped some filler words unconditionally.
Labs now provides a reversible clean/verbatim display switch. It removes a
small set of English hesitations and immediate repetitions, then repairs basic
spacing/capitalization. The saved transcript text, speaker, and start/end
times stay verbatim. New summaries use the derived clean text when this Labs
setting is enabled; export paths continue to use the saved raw text. The clean
version is derived at use time, so it does not suppress acoustic tokens,
alter words in other languages, or claim a second timestamp track. More
aggressive phrase removal and LLM rewriting would risk changing meaning.

## Verification and release status

Verification performed on this branch: Next production build, native CPU
`cargo check`, the voice-profile matcher unit test, CUDA native release compile,
and NSIS packaging passed. The CUDA executable imports bundled CUDA 13 cuBLAS,
and the installer SHA-256 matches `dist-labs-cuda-test/SHA256SUMS.txt`.
Earlier targeted meeting detection and waveform tests and frontend Labs tests
also passed before this follow-up. The installed model directory contains the
v3 INT8 ONNX encoder/decoder. An explicitly run ignored DirectML test with
that model and synthetic silence recorded 2,020 encoder node events on
DirectML and 479 on CPU. This establishes mixed-provider execution, not a
real-speech speedup or accuracy result. Real-call qualification remains open.
The installer built with
`frontend/scripts/build-labs-test-windows.ps1` produces an unsigned CPU installer
under `dist-labs-test/`; `-Cuda` produces an unsigned RTX 50-series-capable
Whisper CUDA installer under `dist-labs-cuda-test/`. Both use the same Labs app
identity so an upgrade can retain install-local profiles, models, and meetings.
Nemotron continues using DirectML independently of Whisper's CUDA backend.
The additional Labs Parakeet GPU switch uses DirectML for Parakeet's encoder
while keeping its decoder and preprocessor on CPU. Changing it reloads the
current Parakeet model and reports initialization failure in Settings; real
speech inference on the RTX 5080 still needs installed-app qualification. The
Local stack view now reports the native DirectML setting rather than a fixed
CPU label; it does not claim all encoder operations run on the GPU. The app's
default is `parakeet-tdt-0.6b-v3-int8` from the verified ONNX export. NVIDIA's
upstream NeMo checkpoint and the linked FastAPI project's FP32 CUDA benchmark
are different runtime and precision configurations.
The universal release build is owned by
`frontend/scripts/build-universal-windows.ps1` and must be checked with
`frontend/scripts/verify-windows-release.mjs` when release packaging is
requested. These checks are separate from real-call qualification. No private
recording or biometric profile belongs in the repository. A locally built
installer is not an installed or published release. The attachment's existing
installer path identifies an older build and must not be presented as
containing these Labs changes.
