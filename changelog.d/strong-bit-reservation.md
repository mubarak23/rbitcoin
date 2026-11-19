Fixed

- **The strong-bit image no longer stops the node at 2^31 creates.** `strong_tx`
  is one bit per create and was sized by the 256 MiB Class C load cap, which
  holds exactly 2^31 bits. The create past that index asked for a page beyond
  the reservation and failed with `strong bit image exceeds the reservation` on
  every retry, so the node could not confirm the block that carried it. The
  image now reserves address space for 2^33 creates from a constant of its own;
  the Class C load cap keeps its original job for `confirmed` and
  `header_txs_*`. Only address space is reserved, so resident memory stays one
  bit per create and operators see no change at today's chain height.
