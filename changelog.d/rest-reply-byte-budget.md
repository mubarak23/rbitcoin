Security

- **Built REST replies share a byte budget until they are written.** With
  `--rest`, slow readers could each hold a full verbose
  `/rest/mempool/contents.json` reply in memory, up to the 256-connection
  cap. A built reply now holds its bytes of a 64 MiB budget until it is
  written or its connection closes. A reply larger than the budget takes all
  of it, so one still goes out at a time. While the budget is spent, a new
  REST request is HTTP 503 `Work queue depth exceeded` before anything is
  built. Core has no such budget.
