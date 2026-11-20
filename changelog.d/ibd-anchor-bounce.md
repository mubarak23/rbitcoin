Changed

- **An anchor ask samples the ordered queue once.** `unproved_anchor` read
  every ordered hash once per checkpoint, so a long queue with several
  look-ahead checkpoints rescanned it on each `send_getheaders`. It now
  takes one highest-height sample and compares the checkpoints to it.
- **Merged body peeks reuse their bounce buffers.** A merged page-span
  read allocated and zeroed a fresh buffer on every wave. Each thread now
  keeps a small capped pool and reuses a buffer that already covers the
  span.
