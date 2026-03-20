# Strava Activity Detail Page — Metrics Extraction

You are analyzing a screenshot of a Strava activity detail page (`strava.com/activities/{id}`).

## Task

Extract all visible metrics and metadata from this activity page.

## Output Format

Return a single JSON object with the following fields. Include only fields that are visible on the page — omit fields that are not shown.

### Core Fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Activity name / title |
| `type` | string | Sport type (Run, Ride, Swim, Hike, etc.) |

### Distance & Pace

| Field | Type | Description |
|-------|------|-------------|
| `distance` | string | Distance with unit (e.g., "8.2 km") |
| `moving_time` | string | Moving time (e.g., "45:30", "1:23:45") |
| `elapsed_time` | string | Elapsed time including stops |
| `pace` | string | Pace (e.g., "5:32 /km") |
| `avg_speed` | string | Average speed (e.g., "25.3 km/h") |
| `max_speed` | string | Maximum speed |
| `elevation` | string | Elevation gain (e.g., "120 m") |
| `gap` | string | Grade Adjusted Pace / VAP |

### Heart Rate & Power

| Field | Type | Description |
|-------|------|-------------|
| `avg_hr` | string | Average heart rate (e.g., "145 bpm") |
| `max_hr` | string | Maximum heart rate |
| `avg_power` | string | Average power (e.g., "210 W") |
| `cadence` | string | Average cadence (e.g., "170 spm") |
| `calories` | string | Calories burned |
| `relative_effort` | string | Relative Effort score |

### Weather

| Field | Type | Description |
|-------|------|-------------|
| `weather` | string | Conditions (e.g., "Clear", "Cloudy") |
| `temperature` | string | Temperature (e.g., "18°C") |
| `feels_like` | string | Feels-like temperature |
| `humidity` | string | Humidity percentage |
| `wind_speed` | string | Wind speed |
| `wind_direction` | string | Wind direction |

### Equipment & Location

| Field | Type | Description |
|-------|------|-------------|
| `device` | string | Recording device (e.g., "Garmin Forerunner 265") |
| `gear` | string | Shoes or bike name |
| `location` | string | City / area where the activity took place |
| `perceived_exertion` | string | Perceived exertion rating |

## Rules

- Extract values exactly as displayed on the page
- Don't convert units or reformat values
- Omit fields that aren't visible — don't guess
- If the page shows data in multiple languages, extract as-is
- Return valid JSON only — no markdown fences, no commentary
- The screenshot may require scrolling — extract everything visible

## Example Output

```json
{
  "name": "Morning Trail Run",
  "type": "Trail Run",
  "distance": "12.4 km",
  "moving_time": "1:15:30",
  "pace": "6:05 /km",
  "elevation": "385 m",
  "avg_hr": "152 bpm",
  "max_hr": "178 bpm",
  "cadence": "168 spm",
  "calories": "845",
  "weather": "Clear",
  "temperature": "14°C",
  "device": "Garmin Forerunner 265",
  "gear": "Nike Pegasus Trail 4"
}
```
