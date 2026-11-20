Fixed

- **The leftover tweak-file notice names the real flag.** Opening a store
  that still had pre-schema-17 `sp_tweaks.idx` / `sp_tweaks.body` files
  printed "--sptweaks backfill regenerates", a flag the node refuses. It now
  says `--sp-tweaks`, and the three leftover-file notices on open are WARN
  log lines instead of bare stderr.
