"""Rebuild one AMI test meeting's single-distant-mic audio from its segments.

The dataset stores audio per annotated segment, so gaps between segments
become digital silence; the full Array1-01 WAV from the AMI site is better
when it can be downloaded. Writes 16 kHz mono 16-bit WAV.

    uvx --with duckdb --with soundfile --with numpy python -I ami_meeting_audio.py IS1009a IS1009a.wav
"""
import io
import sys

import duckdb
import numpy as np
import soundfile as sf

meeting, out = sys.argv[1], sys.argv[2]
con = duckdb.connect()
con.execute("INSTALL httpfs; LOAD httpfs;")
rows = []
for shard in range(4):
    url = f"https://huggingface.co/datasets/edinburghcstr/ami/resolve/main/sdm/test-0000{shard}-of-00004.parquet"
    rows += con.execute(f"select begin_time, audio from read_parquet('{url}') where meeting_id = ?", [meeting]).fetchall()
if not rows:
    sys.exit(f"no segments for {meeting}")
buf = np.zeros(int((max(b for b, _ in rows) + 120) * 16000), dtype=np.float32)
last = 0
for begin, audio in rows:
    x, sr = sf.read(io.BytesIO(audio["bytes"]), dtype="float32")
    assert sr == 16000, sr
    start = int(round(begin * 16000))
    buf[start:start + len(x)] = x
    last = max(last, start + len(x))
sf.write(out, buf[:last + 16000], 16000, subtype="PCM_16")
print(meeting, len(rows), "segments,", round(last / 16000 / 60, 1), "min")
