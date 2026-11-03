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
| `perf_bench::post_call` | Decode + resample of the whole file, full-recording VAD, Whisper large-v3-turbo on packed windows | Whisper needs `MEETILY_BENCH_WHISPER_DIR` (the folder holding `ggml-large-v3-turbo-q5_0.bin`). |

Environment: `MEETILY_BENCH_MIC` (required), `MEETILY_BENCH_SYSTEM`,
`MEETILY_BENCH_SECONDS` (default 600), `MEETILY_BENCH_SEGMENTS`,
`MEETILY_BENCH_PARAKEET_DIR`, `MEETILY_BENCH_WHISPER_DIR`. Run one test per
invocation from `frontend/src-tauri`:

```text
MEETILY_BENCH_MIC=room-mic.mp4 MEETILY_BENCH_SEGMENTS=/tmp/seg.bin \
  cargo test --release -p meetily --lib perf_bench::live_pipeline -- --ignored --nocapture
```

Use recordings you are allowed to keep locally; never commit them or the segment
files. Lines starting with `BENCH` are the results.

`frontend/scripts/sample-meetily-cpu.sh <out.log>` samples the installed app's
native CPU every 15 s with a recording flag from the app log, for before/after
comparisons on real meetings (`ps` CPU time deltas are exact; `%cpu` is a decaying
average). It does not see the WebView (WebKit WebContent) process.

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
