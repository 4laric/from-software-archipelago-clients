#ifndef MFG_AP_READONLY_V1_H
#define MFG_AP_READONLY_V1_H
#include <stdint.h>
#if !defined(_WIN32) && !defined(__cdecl)
#define __cdecl
#endif
/* Windows x64 C ABI; natural 8-byte alignment, no packed structures.
 * Caller owns buffers. Callees never retain pointers or touch game state.
 * Query/copy read a synchronized cache published by the map owner thread.
 */
#define MFG_AP_ABI_V1 1u
#define MFG_AP_CAP_HOVER_V1 1u
#define MFG_AP_CAP_LOT_STYLE_OVERLAY_V1 2u
#define MFG_AP_OK 0u
#define MFG_AP_UNAVAILABLE 1u
#define MFG_AP_BAD_ARGUMENT 2u
#define MFG_AP_UNSUPPORTED_ABI 3u
#define MFG_AP_NO_HOVER 0u
#define MFG_AP_HOVER 1u
#define MFG_AP_LOT_UNKNOWN 0u
#define MFG_AP_LOT_MAP 1u
#define MFG_AP_LOT_ENEMY 2u
#define MFG_AP_STYLE_NORMAL 0u
#define MFG_AP_STYLE_ORANGE 1u
#define MFG_AP_STYLE_YELLOW 2u
#define MFG_AP_LOT_STYLE_MAX_ENTRIES 8192u
#define MFG_AP_LOT_STYLE_MIN_LEASE_MS 250u
#define MFG_AP_LOT_STYLE_MAX_LEASE_MS 10000u
typedef struct MFG_AP_InfoV1 {
    uint32_t abi_version, struct_size, capabilities, hover_size;
} MFG_AP_InfoV1;
typedef struct MFG_AP_HoverV1 {
    uint32_t struct_size, status;
    uint64_t generation, handle;
    uint32_t original_flag, lot_table, lot_row, age_ms;
} MFG_AP_HoverV1;
/* A presentation style for one baked ItemLotParam identity. The entry contains
 * neither an item id nor a location, so it cannot request a randomized-item
 * reveal. lot_table is MAP or ENEMY; lot_row is nonzero. NORMAL is not a
 * valid snapshot entry: omit it instead. */
typedef struct MFG_AP_LotStyleV1 {
    uint32_t lot_table, lot_row, style;
} MFG_AP_LotStyleV1;
/* Generation starts at 1 and increases at every row rebuild. Reset on DLL
 * replacement requires host session invalidation. A handle is never a pointer.
 * original_flag is baked acquisition identity, not a rewritten live flag.
 * lot_table 0 means unknown; absent identity stays unresolved.
 * age_ms is elapsed monotonic time since owner-thread hover observation.
 * NO_HOVER is valid only for an initialized current generation.
 * UNAVAILABLE means no initialized cache; never represent it as NO_HOVER.
 * On non-OK return callers discard the output.
 */
typedef uint32_t (__cdecl *MFG_AP_QueryV1)(uint32_t requested_abi,
    MFG_AP_InfoV1 *out, uint32_t capacity);
typedef uint32_t (__cdecl *MFG_AP_CopyHoverV1)(MFG_AP_HoverV1 *out,
    uint32_t capacity);
typedef uint32_t (__cdecl *MFG_AP_SetLotStylesV1)(uint32_t requested_abi,
    const MFG_AP_LotStyleV1 *entries, uint32_t count, uint32_t lease_ms);
/* Exact exports: MFG_AP_QUERY_V1 and MFG_AP_COPY_HOVER_V1.
 * Query returns UNSUPPORTED_ABI for versions other than 1; BAD_ARGUMENT for
 * null/undersized output. Copy returns UNAVAILABLE until a cache exists.
 * Engines may not advertise CAP_HOVER until owner-thread publication is wired.
 *
 * MFG_AP_SET_LOT_STYLES_V1 replaces the complete style snapshot synchronously:
 * it copies no caller-owned data after returning and never reads or writes game
 * memory on the caller thread. The caller supplies ABI 1, 0..8192 entries,
 * and a 250..10000 ms lease. Every nonempty entry must have a unique
 * (lot_table, lot_row), table MAP or ENEMY, nonzero row, and style ORANGE or
 * YELLOW. A null pointer with count=0 and lease_ms=0 clears immediately.
 * The implementation stops rendering all styles when the lease expires, when
 * map rows are retired, or when map injection is disabled. A style only
 * decorates an already-visible unambiguous native marker; it never requests a
 * marker reveal or item scouting.
 */
#endif
