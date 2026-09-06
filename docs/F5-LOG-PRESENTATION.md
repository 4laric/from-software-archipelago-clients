# F5 log presentation

Local Elden Ring check names hide the terminal numeric flag ID and map tile ID.
For example, `Limgrave :: Item, may be sweep-granted by Tree Sentinel (m60_42_42) [f123]`
becomes `Limgrave :: Item, sweep: Tree Sentinel` when that sweep is enabled for the seed.
Disabled sweep notes are omitted; unknown seed rules retain the original uncertainty.
Boss qualifiers such as `(Glaive)` remain visible.

- `[P]` marks a location in the seed's progression surface, regardless of the item placed there.
- `[S]` marks a check this client submitted through a sweep during this session, after server acknowledgement.
  Direct pickups detected during the same poll are excluded. It does not reconstruct sweep history
  from previous sessions or claim that a simultaneous co-op pickup was impossible.
- `sweep: Boss` describes eligibility, independently of the grant marker.

Historical rows from an earlier seed never receive the current seed's markers.

The original protocol names and rich-text metadata remain intact. Remote players' location
labels are unchanged. Sweep banner trigger IDs remain in diagnostic logs.

Item text uses Universal Tracker's progression purple `#AF99EF`, useful blue `#6D8BE8`,
trap salmon `#FA8072`, and filler cyan `#00EEEE`, in that precedence for combined flags.
Location text is green; player and entrance colors follow the same parser.
Source: [Universal Tracker NetUtils.py](https://github.com/FarisTheAncient/Archipelago/blob/4c1d65412fa0964ce22184577832fc2518dc7b3f/NetUtils.py#L226).
The overlay retains readable gray for explicitly black text on its black background.
