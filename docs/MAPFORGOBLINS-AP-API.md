# Proposed MapForGoblins marker API for Archipelago

Status: proposal only. No released MapForGoblins build exports this interface, and the
Archipelago client must remain observation-only until both projects agree on the ABI.

## Evidence and scope

The proposal follows the public MapForGoblins v2.0.6 source at commit `a254433`:

- injected markers retain their original row id and their live completion flags;
- the Progress focus path already filters live rows and draws highlight rings;
- focus changes are reapplied through MapForGoblins' own refresh path; and
- completion can be represented by several fields, including live lot rewrites.

Archipelago already knows each check's acquisition flag. A completion flag is therefore the
narrow shared identity. It avoids offsets, generated row ids, coordinates, category ordinals,
and ownership of MapForGoblins' rendering internals.

## Minimal version-one behavior

The three names currently probed by the client are a capability proposal, not an ABI claim:

- `MFG_AP_API_VERSION`
- `MFG_AP_SET_MARKER_STATE_V1`
- `MFG_AP_CLEAR_MARKER_STATES_V1`

Before either side calls them, upstream and the client must agree on a C-compatible header that
defines the calling convention, integer widths, return values, ownership, and threading. The
version query must return a numeric ABI version; symbol presence alone is not a handshake.

`SET_MARKER_STATE_V1` should accept a copied list of Elden Ring completion flags plus a semantic
state such as `available` or `hinted`. MapForGoblins should own the state-to-colour/icon mapping and
apply it through its existing live focus/highlight machinery. All markers matching any supplied
completion flag should receive the state; unknown flags should be harmless no-ops.

`CLEAR_MARKER_STATES_V1` should remove only external overrides and restore ordinary
MapForGoblins visibility, collection, manual-hide, map-fragment, and Progress behavior.

## Safety requirements

- The client never reads or writes MapForGoblins memory by offset.
- The client never assumes generated row ids, category numbers, coordinates, or structure layout.
- MapForGoblins copies caller-owned arrays before returning and does not retain their pointers.
- MapForGoblins serializes or queues updates onto the thread that owns its marker state.
- A partial export set, an unsupported version, or a failed call leaves the integration disabled.
- Unloading or replacing either DLL clears the integration state without leaving callbacks behind.
- Repeated set and clear calls are idempotent.

## Acceptance probes

1. With a released MapForGoblins build that has no API, `!mfgprobe` reports observe-only and makes
   no changes.
2. Every partial combination of the three proposed exports is refused.
3. A complete export set is reported only as a candidate until the numeric version handshake is
   implemented and validated.
4. One supplied check flag highlights every MFG marker that completes on that flag, including a
   marker whose completion flag was rewritten from the live item lot.
5. A foreign or absent flag changes nothing and returns success with zero matches.
6. Clearing restores the exact visibility and highlight state present before the AP override.
7. Map open/close, warp, reconnect, and seed disconnect preserve or clear state as specified,
   without a crash or stale highlight.

Arbitrary coordinates and a client-owned colour palette are deliberately outside version one.
They can be added in a later ABI only if completion-flag matching cannot express a demonstrated
use case.
