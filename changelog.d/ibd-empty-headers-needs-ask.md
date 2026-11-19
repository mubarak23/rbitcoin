Fixed

- **An empty headers reply no longer stalls the header walk until restart.**
  Before the work floor, an empty reply counted toward abandoning the walk
  tip even when we had not asked that peer. Once every taller peer had sent
  one, the walk could mark its own starting point dead, drop every later
  batch built on it, and stall until the node restarted. A reply now counts
  only while the walk is still at the tip its ask named, and the starting
  point is never marked dead.
