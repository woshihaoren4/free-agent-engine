---
name: weather
description: Gets current weather and forecasts without an API key. Use when the user asks about weather for a location or date.
homepage: https://wttr.in/:help
metadata: {"clawdbot":{"emoji":"🌤️","requires":{"bins":["curl"]}}}
---

# Weather

Use `curl` through an available command tool. Report the location, local time and units returned by the provider. If the place name is ambiguous, confirm the intended location before interpreting the forecast.

## wttr.in (primary)

Quick one-liner:
```bash
curl -fsS --max-time 15 "https://wttr.in/London?format=3"
# Output: London: ⛅️ +8°C
```

Compact format:
```bash
curl -fsS --max-time 15 "https://wttr.in/London?format=%l:+%c+%t+%h+%w"
# Output: London: ⛅️ +8°C 71% ↙5km/h
```

Full forecast:
```bash
curl -fsS --max-time 15 "https://wttr.in/London?T"
```

Format codes: `%c` condition · `%t` temp · `%h` humidity · `%w` wind · `%l` location · `%m` moon

Tips:
- URL-encode spaces: `wttr.in/New+York`
- Airport codes: `wttr.in/JFK`
- Units: `?m` (metric) `?u` (USCS)
- Today only: `?1` · Current only: `?0`
- PNG: `curl -fsS --max-time 15 "https://wttr.in/Berlin.png" -o /tmp/weather.png`

## Open-Meteo (fallback, JSON)

If wttr.in fails or structured data is needed, resolve the city's coordinates with the [geocoding API](https://open-meteo.com/en/docs/geocoding-api), then request the needed fields. This example includes current conditions and three days of local forecasts for London:

```bash
curl -fsS --max-time 15 \
  "https://api.open-meteo.com/v1/forecast?latitude=51.5&longitude=-0.12&current=temperature_2m,wind_speed_10m,weather_code&daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=3"
```

Read conditions from `current` and the daily forecast from `daily`. Use `current_units` and `daily_units` to label the values.

Docs: https://open-meteo.com/en/docs
