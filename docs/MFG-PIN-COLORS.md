# Optional hint and progression-surface rings

In F6, open **Map pin test (optional)** and enable **Color map pins (this session)**.
The source-built map engine draws rings around its visible mapped pins:

- Yellow: a known hint for a check in this world.
- Orange: the check is eligible to hold progression under this seed's surface.
- No ring: neither condition, already AP-checked, no reliable mapping, or conflicting
  candidate classifications.

Yellow takes priority over orange. Orange does not identify the actual randomized
item. This feature neither scouts nor reads item placements. Eligibility comes from
the same progression-surface list already used by F6.

The original map icons stay intact. These are overlay rings, not icon texture tints.
The engine's existing projection and visibility determine which rings can appear;
this does not reveal hidden categories, unavailable layers or undiscovered map pins.
Shared source identities are colored only when every surviving seed candidate agrees.
Exact catalog names must agree as well as IDs.

## Integration

Both the updated client and updated source-built map engine are required. The old
hover-only source build still supports pin recording but cannot display rings.
No DLL is loaded automatically.

The client publishes a bounded, copied table/lot/style snapshot once a second, with
a three-second lease. The engine renders it on its own overlay thread. Turning the
option off, disconnecting, changing seeds or leaving the world withdraws the snapshot.
If the client stops updating, the lease expires. No game flags or parameter rows are
written by the coloring API.

## Live acceptance

1. Enable colors with a seed containing progression-surface checks. Compare an
   orange pin against the corresponding starred check in F6.
2. Obtain a normal hint for a mapped local-world check: its ring should become
   yellow, including when it was previously orange.
3. Verify a non-surface, unhinted check remains neutral. Check ambiguous identities
   without assuming the first candidate owns the pin.
4. Pan, zoom and switch overworld/underground/DLC layers. Rings must track visible
   pins and never bleed onto other layers.
5. Collect a pickup, hide its category and test a story/discovery-gated pin. No
   orphan rings should remain.
6. Disable colors and disconnect/reconnect. Rings should clear; restart requires
   opting in again.
7. Check dense map areas for additional frame cost with colors on and off.

Compilation and automated tests do not establish projection accuracy or in-game
performance. These cases require a live run on the intended game version.
