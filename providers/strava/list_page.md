# Strava Training Page — Activity List Extraction

You are analyzing a screenshot of the Strava training page (`strava.com/athlete/training`).

## Task

Extract all visible activities from the training table.

## Output Format

Return a JSON array. Each element must have these fields:

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Numeric activity ID (from the activity URL, e.g., "12345678") |
| `name` | string | Activity name (e.g., "Morning Run") |
| `type` | string | Sport type (e.g., "Run", "Ride", "Swim", "Hike") |
| `date` | string | Date as shown on the page |
| `time` | string | Duration as shown (e.g., "1:23:45", "45:30") |
| `distance` | string | Distance with unit (e.g., "5.2 km", "3.1 mi") |
| `elevation` | string | Elevation gain with unit (e.g., "120 m", "394 ft") |
| `suffer_score` | string | Suffer score / relative effort (empty string if not shown) |

## Rules

- Extract every activity row visible in the screenshot
- Preserve the exact text as displayed (don't convert units)
- If a field is not visible for an activity, use an empty string
- The `id` must be the numeric ID, not the full URL
- Return valid JSON only — no markdown fences, no commentary

## Example Output

```json
[
  {"id": "12345678", "name": "Morning Run", "type": "Run", "date": "Mar 15, 2026", "time": "45:30", "distance": "8.2 km", "elevation": "120 m", "suffer_score": "42"},
  {"id": "12345679", "name": "Afternoon Ride", "type": "Ride", "date": "Mar 14, 2026", "time": "1:30:00", "distance": "35.5 km", "elevation": "450 m", "suffer_score": ""}
]
```
