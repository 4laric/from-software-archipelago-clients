# Free respec prototype

Choose **Respec (experimental)** in the AP overlay, or enter `!respec` in its
Console. The native rebirth screen owns input while open; the overlay returns
after it closes. No new seed, option field, or world regeneration is required.
Use a disposable character or a backed-up save for the first live acceptance run.

This is a prototype, not a claim of in-game verification. Its target is free
rebirth anywhere, before Rennala, without spending either kind of Larval Tear.
The implementation invokes native talk event 113 directly. It never changes
attributes itself, supplies/refunds consumables, or changes progression flags.
Whether this direct path needs any further cost bypass remains a live-test gate.

## Implementation and provenance

- Client base: `b984f625e4bc89a035ef164067dedbb501a47214`.
- Pinned fromsoftware-rs: `af8f38c656bb90aa40d7b81df79483ac94088dfd`.
  `crates/eldenring/src/cs/talk_script.rs` supplies checked invocation and owns
  `NpcMenuState`; `menu_type.rs` identifies `ReallocateAttributes = 19`.
  `examples/invoke-esd` runs a player-associated synthetic talk script at
  FrameBegin and monitors its menus with env 59, arguments `[menu_type, 0]`.
- [TarnishedTool](https://github.com/borgCode/TarnishedTool/blob/dc499cfc4af713e8a91975944d637e17d7e3ab6b/TarnishedTool/GameIds/EzState.cs)
  defines Rebirth as event 113 with no arguments; its UtilityViewModel calls it directly.
- [TGA](https://github.com/The-Grand-Archives/Elden-Ring-CT-TGA/tree/7926205c5a2ed236dd31278c4f5579c964ceec35)
  `Rebirth.cea` likewise calls `executeEzStateEvent(ReallocateAttributes, {})`.
- [ESDLang Talk metadata](https://github.com/thefifthmatt/ESDLang/blob/master/dist/ESDScriptingDocumentation_Talk.json)
  identifies env 25 (global menu query) and env 0 (nearby enemies). The latter's
  argument is documented only as `unk`: **our argument 0 is a candidate, not a
  verified combat gate**. Test normal enemy aggro explicitly before promoting it.

Only the FrameBegin task accesses the synthetic script. It runs even while AP
is disconnected. A single retained allocation keeps menu callbacks alive for
the process lifetime. Normal completion requires the menu query to close, the
owner's menu state to clear, and its finalize callback pointer to retire; a
two-second settle window follows. A timeout or active-owner change disables
further respecs until restart. It never frees or retargets uncertain callbacks.

Inventory reconciliation, received/start grants, auto-equip, queued traps,
DeathLink kills, region kicks and client warps wait while the transaction is
active. AP network processing continues. Input capture is relinquished for the
native screen, including stale imgui capture from the initiating console field.
The original state and the observed post-menu level, class and eight attributes
are logged under `respec:`. A matching total is not proof of save persistence or
consumable behaviour.

## Required live acceptance

Run on each supported executable being claimed, recording its version and DLL SHA.

1. Before defeating Rennala, with **zero** Larval Tears, open from a quiet
   overworld location. Reallocate and confirm. Check the level, class minimums,
   attribute total, and immediate HP/FP/stamina/equipment requirements.
2. Repeat while holding ordinary and DLC tears. Their counts must not fall.
   Cancel a changed allocation; all stats and both tear counts must be unchanged.
3. Quit and reload. Confirm committed stats persist, cancelled changes do not,
   and a second normal respec works without restarting the process.
4. Try menu and console invocation using keyboard/mouse and controller. Input
   must reach rebirth without requiring F5; closing must restore the overlay.
5. Refusal cases: main menu, loading, dying, mounted, boss fight, ordinary enemy
   aggro, vanilla multiplayer, and existing game menus (including inventory,
   status, map, merchant, grace, and generic dialogs). Global menu IDs and the
   enemy query are source-derived and still need this behavioural coverage.
   Seamless Co-op is **not verified** by the vanilla session-manager check.
6. While open, receive items, a trap and DeathLink; disconnect/reconnect AP.
   Effects must resume after closure, exactly once. Exercise an already-running
   spawn burst too. No receipts or watermarks should advance past held work.
7. Interrupt via death or a game-driven world transition. Confirm no crash or
   dangling callbacks; retry must explain that a restart is required. A callback
   timeout must produce a visible message instead of silently freeing its owner.

If the finalize pointer does not clear on a normal close, record its value and
the native menu query/state at that point. Do not remove the retirement check
just to make repeated respec appear to work.
