"""WER of `speech_windows::bench::bench` outputs against AMI manual transcripts.

Expects <root>/<meeting>[.anything]/<setting>/packed-<Decoding>.txt, as written
with MEETILY_BENCH_OUT; each line is "start-end text". Both sides go through
Whisper's English text normalizer (fillers, numbers, spelling variants).

    uvx --with jiwer --with whisper-normalizer python -I ami_wer.py ami_sdm_test.json <root>
"""
import glob
import json
import os
import re
import sys

import jiwer
from whisper_normalizer.english import EnglishTextNormalizer

normalize = EnglishTextNormalizer()
refs = json.load(open(sys.argv[1]))
totals = {}
for meeting_dir in sorted(glob.glob(f"{sys.argv[2]}/*")):
    meeting = os.path.basename(meeting_dir).split(".")[0]
    ref = normalize(" ".join(r[4] for r in sorted((r for r in refs if r[0] == meeting), key=lambda r: r[1])))
    for path in sorted(glob.glob(f"{meeting_dir}/*/packed-*.txt")):
        setting = path.split("/")[-2] + "/" + os.path.basename(path)[len("packed-"):-len(".txt")]
        hyp = normalize(" ".join(re.sub(r"^\S+\s", "", line.strip(), count=1) for line in open(path)))
        result = jiwer.process_words(ref, hyp)
        n = len(ref.split())
        print(f"{meeting:8} {setting:14} WER {100 * result.wer:5.1f}%  sub {result.substitutions:4} del {result.deletions:4} ins {result.insertions:4}  ref {n}")
        total = totals.setdefault(setting, [0, 0])
        total[0] += result.substitutions + result.deletions + result.insertions
        total[1] += n
for setting, (errors, n) in sorted(totals.items()):
    print(f"ALL      {setting:14} WER {100 * errors / n:5.1f}%")
