"""Fetch AMI test-set manual transcripts (single distant mic config) as JSON.

Reads only the text columns of the Hugging Face `edinburghcstr/ami` parquet
files (HTTP range requests; the audio column is not downloaded). The output is
a list of [meeting_id, begin_s, end_s, speaker_id, text] rows.

    uvx --with duckdb python -I ami_reference.py ami_sdm_test.json
"""
import json
import sys

import duckdb

out = sys.argv[1]
con = duckdb.connect()
con.execute("INSTALL httpfs; LOAD httpfs;")
rows = []
for shard in range(4):
    url = f"https://huggingface.co/datasets/edinburghcstr/ami/resolve/main/sdm/test-0000{shard}-of-00004.parquet"
    rows += con.execute(f"select meeting_id, begin_time, end_time, speaker_id, text from read_parquet('{url}')").fetchall()
json.dump(rows, open(out, "w"))
print(len(rows), "segments:", sorted({r[0] for r in rows}))
