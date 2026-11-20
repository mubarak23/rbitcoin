Changed

- **An anchor ask reads the ordered queue only when it can matter.**
  `unproved_anchor` read every ordered hash once per checkpoint, so a long
  queue with several look-ahead checkpoints rescanned it on each
  `send_getheaders`. It now reads the queue only when a checkpoint sits at
  or above the queue front, from the back, and stops at the first hash that
  reaches the lowest such checkpoint.
- **Merged body peeks reuse one bounce buffer.** A merged page-span read
  allocated and zeroed a fresh buffer on every wave. Each wave now carves
  its merged spans from one per-thread buffer kept up to 1 MiB; a wave that
  needs more gets a buffer of its own and the kept one stays.
