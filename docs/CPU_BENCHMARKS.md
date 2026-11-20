# CPU benchmarks

`frontend/src-tauri/src/perf_bench.rs` holds ignored macOS benchmarks that replay
saved recordings through production code as fast as it runs. Each reports process
CPU time (user + system over all threads, from `getrusage`) per minute of audio,
the same as a percentage of one core in real time, and wall time. CPU time stays
comparable on a loaded machine; wall time shows GPU/Neural Engine work.

| Test | What runs | Notes |
|---|---|---|
| `perf_bench::live_pipeline` | `AudioPipeline::run`: resampling, echo cancellation (with a system track), high-pass, loudness normalization, both VADs, mixing, level metering | Fed 10 ms blocks per source. Without `MEETILY_BENCH_SYSTEM` it is room-microphone mode. Writes the speech segments it emits for `live_parakeet`. Level updates are wall-clock throttled, so their count understates a real recording. |
| `perf_bench::live_parakeet` | Live Parakeet (`ParakeetModel::transcribe_samples`) on those segments | Uses CoreML when the model folder has the CoreML copy beside it. |
| `perf_bench::live_nemotron` | Live Nemotron on CoreML, fed 50 ms of 16 kHz audio at a time | Needs `MEETILY_BENCH_NEMOTRON_DIR` (the `nemotron3-coreml` folder). |
| `perf_bench::room_cleanup` | Room playback cleanup: WPE, DeepFilterNet, level match (FFmpeg excluded) | Two-thread pool like the production worker. |
| `perf_bench::post_call` | Decode + resample of the whole file, full-recording VAD, Whisper large-v3-turbo on packed windows | Whisper needs `MEETILY_BENCH_WHISPER_DIR` (the folder holding `ggml-large-v3-turbo-q5_0.bin`). |

Environment: `MEETILY_BENCH_MIC` (required), `MEETILY_BENCH_SYSTEM` (a file, or
`silence` for a silent computer-audio track),
`MEETILY_BENCH_SECONDS` (default 600), `MEETILY_BENCH_SEGMENTS`,
`MEETILY_BENCH_PARAKEET_DIR`, `MEETILY_BENCH_WHISPER_DIR`, `MEETILY_BENCH_NEMOTRON_DIR`.
Run one test per
invocation from `frontend/src-tauri`:

```text
MEETILY_BENCH_MIC=room-mic.mp4 MEETILY_BENCH_SEGMENTS=/tmp/seg.bin \
  cargo test --release -p meetily --lib perf_bench::live_pipeline -- --ignored --nocapture
```

Use recordings you are allowed to keep locally; never commit them or the segment
files. Lines starting with `BENCH` are the results.

`frontend/scripts/sample-meetily-cpu.sh <out.log>` samples the installed app's
native CPU, and the WebKit WebContent process that renders its windows, every
15 s with a recording flag from the app log, for before/after comparisons on
real meetings (`ps` CPU time deltas are exact; `%cpu` is a decaying average).
WebContent is matched by launch time (started within 10 s after the native
process). `sample <pid> 5` on either process shows where the time goes.

## Baseline (November 3, M4, main at fc5099f)

10 min of an English two-speaker room meeting (Jabra Speak 710); the "system" track
for the dual-source run is another meeting's microphone.

| Stage | CPU (% of one core in real time) | Wall |
|---|---|---|
| Live pipeline, room microphone | 0.7% | 4.4 s |
| Live pipeline, microphone + system (echo cancellation on) | 6.8% | 41.0 s |
| Live Parakeet, CoreML (105 segments, 516 s of speech) | 0.9% | 5.7 s |
| Post-call decode + resample (27 min file) | 0.4% | 1.7 s |
| Post-call VAD (10 min) | 0.3% | 1.6 s |
| Post-call Whisper, packed windows (10 min) | 0.5% | 44.0 s (GPU) |

Not covered yet: live Nemotron (CoreML, RTF 0.05 in its own qualification test), the
WebView, logging, transcript file writes, speaker embeddings, and room cleanup
(RTF 0.04 at normal priority in `room_cleanup` tests).

## After the CPU-reduction pass (November 3, same inputs)

| Stage | Before | After | Change |
|---|---|---|---|
| Live pipeline, room microphone (25 min) | 11.2 CPU-s | 9.1 CPU-s | Silero session reset instead of rebuild; loudness histogram mode, no unused true peak |
| Live pipeline, mic + system, other side talking throughout (10 min) | 6.8% | 5.5% | Vectorized/polyphase echo-canceller filters, reused model buffers |
| Live pipeline, mic + silent system (10 min) | 6.7% | 1.3% | Echo model skipped while the reference is silent |
| Post-call decode + resample (27 min) | 5.7 CPU-s (all cores) | 1.2 CPU-s | Streaming FFT resampler instead of parallel 512-tap sinc |
| Post-call Whisper (27 min) | 112 s wall | 58 s wall | CoreML encoder on the Neural Engine; one reused decoder state |
| Room cleanup | started 60 s after Stop, overlapping post-call | starts after 60 s of no recording or post-call work | scheduling only; FFmpeg children in the background class |

Also: level meters scale with `transform` (no layout per update) and update every
60 ms per source instead of 40 ms; the recording state poll runs once a second.
The WebView's CPU is not measured by these benchmarks.

Still on the CPU, measured: room cleanup DeepFilterNet 20 CPU-s and WPE 10.6
CPU-s per 10 min of audio (background priority, after post-call work). Live
Parakeet (0.9%) and live Nemotron (0.2%) are already small.

## A real recording (November 3, M4, 0.2.26 build with the changes above)

A 53-minute room-microphone meeting (Jabra Speak 710, live Parakeet and live
Nemotron with voice-profile naming), sampled with `ps` over 30 s windows and
profiled with `sample`:

| Process | CPU (% of one core) | Where it went |
|---|---|---|
| Native | 13-16% | ~15% live voice-profile naming: WeSpeaker on ONNX Runtime embedding every Nemotron turn |
| WebView (WebContent) | 10-14% | 57% of busy time in WebCore style and compositing for the level meters; 22% JavaScript |

The benchmarks above miss both: they do not run voice-profile naming, and they
do not render the window. Fixes (issue #21, #23):

- Level meters draw on one canvas from the event handler. The DOM version
  re-rendered 18 bars per meter per event through React, each with a 100 ms
  CSS transition that never finished at ~16 events a second.
- The recorder page no longer polls `is_recording` every second for its whole
  life (recording or not); the compact bar reads native state every 5 s
  instead of every 500 ms.
- Live naming embeds each speaker channel on its first four turns, then every
  30 s (unnamed) or 60 s (named).
- The macOS system-audio tap is read in chunks instead of one polled,
  timeout-wrapped sample at a time; `transcripts.json` is rewritten at most
  every 15 s instead of after every segment (580 full rewrites in that
  meeting).

Post-call on the same meeting: Whisper (CoreML encoder) took 117 s for 53.5 min,
the 0.036x real-time ratio measured in `perf_bench::post_call`.
