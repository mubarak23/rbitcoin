Added

- **`getblockfrompeer "blockhash" peer_id` asks one peer for a block body.**
  The header must be known and the body missing. The call returns `{}` once
  the ask is queued, and that session's heartbeat sends the `getdata`. The
  body takes the normal receive path, so the block still goes through the
  usual checks and is not marked valid because it was requested. The errors
  are Core's: `Block header missing`, `Block already downloaded`, `Peer does
  not exist`, `Pre-SegWit peer`, and `Peer not fully connected`. At most 16
  asks wait on one session. As in Core, a body already in flight from
  another peer moves to the named peer: the first peer stops counting it
  toward its stall timeout, and a body it still sends is treated as
  unrequested. The call does not restore `--prune-seqsigwit` data.
