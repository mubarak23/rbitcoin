Added

- **`ibd: perf` meters the IBD heap that `accounted` leaves out.** Each line
  has an `owned.heap` object: the in-flight pin estimate the retain
  governor charges (`pin_est`) and the heap its maps hold at their
  high-water capacity past that estimate (`pin_maps`), the decoded bodies
  alive in the confirm pipeline (`decoded_blocks`, `decoded`), the spend
  delta's notes and page directory (`spend_notes`, `spend_dir`), and the
  confirm header plans (`conf_plans`). `work.map_bytes` is the work-path
  maps' heap from their capacity. These are meters only; `accounted` and the
  retain governor do not change. `docs/ibd-memory.md` has a `jq` line for
  the anonymous heap no meter names.
