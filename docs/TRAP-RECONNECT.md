# Trap reconnect behavior (0.6.0.8)

Received AP traps now have a persisted consumption frontier scoped to the room and
AP slot. Reconnecting, rebinding an Elden Ring character, or recovering an item
cursor must not enqueue old traps again or rebroadcast them through TrapLink.
New traps, including those received while offline, still enter the existing queue.
Switching characters on the same AP slot does not repeat previously consumed traps.

Existing saves migrate automatically from the highest recorded receive cursor.
There is no seed-contract or save-version change. Ordinary item recovery and
inbound TrapLink delivery retain their existing behavior.

The frontier records acceptance into the queue, as the ordinary cursor did before:
a process exit before a queued effect fires can still lose that pending effect.
Deleting the client save file also deletes its deduplication history.

Regression coverage: `cargo test -p er-logic --test trap_reconnect` exercises the
receive decision, trap claim, and old/new save JSON through reconnect and cursor
rewind timelines. An in-game smoke test should consume traps, quit/reload and
reconnect, then receive another trap: only the new trap should fire or be linked.
