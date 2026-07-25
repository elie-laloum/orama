# 03 — Persist round-trips to SQLite via async writer

**What to build:** Every completed non-streaming round-trip is persisted as one SQLite row — one record per request→response exchange. A background async writer task owns the SQLite connection so persistence never sits on the request path. Auth header values are redacted on write; everything else is stored verbatim. After a call completes, the corresponding row exists and is readable.

**Blocked by:** 02 — Transparent catch-all relay (non-streaming).

**Status:** ready-for-agent

Schema — one record = one exchange (fields carried from the POC plan; streaming columns populated in a later ticket):

```
id                       INTEGER PK
timestamp_start          TEXT/epoch
timestamp_first_chunk    nullable   (→ derive TTFT)
timestamp_end            nullable   (→ derive latency)
method                   TEXT
url                      TEXT       (full path + query)
request_headers          JSON       (auth values redacted)
request_body             JSON
response_status          INTEGER
response_headers         JSON
response_raw_sse         TEXT        (verbatim chunk sequence; null if non-stream)
response_reconstructed   JSON        (assembled response for display)
error                    TEXT nullable
```

- [ ] A background writer task owns the SQLite connection; the request path hands off the finished record and does not block on the write.
- [ ] Each non-streaming round-trip produces exactly one row with method, url, request headers/body, response status/headers/body, and start/end timestamps.
- [ ] Auth header values are redacted before write; all other data is stored verbatim.
- [ ] A completed non-streaming call is retrievable as a row from the database.
- [ ] The catch-all relay behaviour from ticket 02 is unchanged.
