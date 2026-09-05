# Source-built map engine: first read-only boundary

The canonical contract is [mfg_ap_readonly_v1.h](include/mfg_ap_readonly_v1.h).
This replaces no stock API: the supplied 2.1.3 DLL exports neither function.

The explicit !mfgprobe command and the bounded F6 recording window obtain
a temporary loader reference, negotiate sizes/version/capabilities and copy one
hover snapshot per call. It never loads a DLL, calls legacy speculative marker setters,
writes game flags, infers completion or enables continuous tracking. An inert
engine with no published owner-thread cache must return unavailable / withhold
the hover capability, not fake a working no-hover response.

The pure bridge provides default-disabled lifecycle validation for subsequent
tracker wiring. Disconnect, seed replacement and module replacement must reset
the bridge; callers must discard selection on every transport failure, as the
one-shot probe naturally does. Generation increases on every engine row rebuild;
handles are scoped to generation and client session. Missing baked identity
stays unresolved. The 300 ms hover expiry is a conservative initial bound,
not a measured latency.

This stage prints identity only. Registry joining, AP selection, continuous tracking,
F6 opt-in persistence and native focus are follow-up work. No hover means no
selection; it does not mean a check was visited, completed, visible or reviewed.
No game layout is guessed here. API declarations have source-verified Windows
bindings; Linux unit tests do not prove Windows compilation or in-game safety.


## Recording while the client is closed

The F6 tracker now has a **Map pin test (optional)** section. Choose **Record
a map pin for 30 seconds**, close F6, and hide the main client window with F5
if it is open. Escape releases the client cursor if needed. After three seconds
with F6 closed and ImGui no longer requesting mouse or keyboard input, the client
samples at most ten times per second. Reopening F6 or capturing client input
restarts that grace period; the overall 30-second deadline does not extend.
Recording stops at the first fresh hovered marker or at that deadline.

Reopen F6 to read the result. A successful result is explicitly a **historical
observation**, with monotonic client receipt time, elapsed time since arming,
the source snapshot age, and copied marker identity. It is not a live hover,
a verified AP binding, a completion event, or player corroboration. The existing
300 ms freshness limit remains unchanged. An expired window reports its last
observation or refusal once; it does not log every poll.

Restarting, cancelling, connecting/disconnecting, changing seed, or leaving/entering a
loaded world clears the result. This is a session-only diagnostic: it does
not load an absent DLL, change settings, write game state, or submit reviews.
The original one-shot !mfgprobe remains available, but opening its console may
interrupt the hover, which is why the separate recording window exists.
