# Source-built map engine: first read-only boundary

The canonical contract is [mfg_ap_readonly_v1.h](include/mfg_ap_readonly_v1.h).
This replaces no stock API: the supplied 2.1.3 DLL exports neither function.

The explicit existing !mfgprobe command is the only production caller. It obtains
a temporary loader reference, negotiates sizes/version/capabilities and copies one
hover snapshot. It never loads a DLL, calls legacy speculative marker setters,
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

This stage prints identity only. Registry joining, AP selection, frame polling,
F6 opt-in persistence and native focus are follow-up work. No hover means no
selection; it does not mean a check was visited, completed, visible or reviewed.
No game layout is guessed here. API declarations have source-verified Windows
bindings; Linux unit tests do not prove Windows compilation or in-game safety.
