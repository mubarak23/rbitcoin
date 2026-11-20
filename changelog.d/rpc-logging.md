Added

- **`logging` RPC.** Read or change debug categories at runtime, as in
  Core: `logging` lists each category, `logging '["mempool"]'` turns one
  on, `logging '[]' '["mempool"]'` turns it off. `"all"` covers every
  category, and an unknown name is refused without changing anything.
  The categories are rbitcoin's own log prefixes (`api`, `electrum`,
  `i2p`, `ibd`, `mempool`, `node`, `p2p`, `query`, `store`, `sv2`, `tip`,
  `tor`, `txrelay`). `--log-level debug` and `trace` start with all of
  them on, so startup output is unchanged.
