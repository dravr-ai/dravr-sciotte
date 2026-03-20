# Page Analysis — Identify Page Type and Available Actions

You are analyzing a screenshot of a web page during a login or navigation flow.

## Task

Identify the page type and return the available actions.

## Output Format

Return a JSON object:

```json
{
  "page_type": "one of the types below",
  "actions": [
    {"type": "click", "label": "button or link text", "x": 123, "y": 456},
    {"type": "fill", "label": "field description", "x": 123, "y": 456},
    {"type": "none", "label": "wait for redirect"}
  ],
  "error_message": "text of any visible error, or null",
  "two_factor_options": [
    {"id": "otp", "label": "Google Authenticator", "x": 100, "y": 200},
    {"id": "app", "label": "Tap Yes on your phone", "x": 100, "y": 300}
  ]
}
```

## Page Types

| Type | Description |
|------|-------------|
| `provider_login` | Provider's login page (Strava, Garmin) with email/password fields |
| `oauth_email` | Google/Apple email entry page |
| `oauth_password` | Google/Apple password entry page |
| `cookie_consent` | Cookie consent dialog blocking the page |
| `passkey_challenge` | Passkey / security key prompt |
| `two_factor_selection` | 2FA method chooser (Authenticator, SMS, phone tap) |
| `otp_entry` | OTP / verification code input field |
| `phone_approval` | "Check your phone" / "Tap Yes" waiting page |
| `success` | Dashboard or post-login page (login succeeded) |
| `error` | Error message displayed (wrong password, account locked) |
| `unknown` | Page doesn't match any known type |

## Rules

- The `actions` array should list clickable buttons and fillable fields with their center coordinates
- For `two_factor_selection`, populate `two_factor_options` with each visible 2FA method
- Exclude passkey/security key from `two_factor_options`
- For `error`, extract the error text into `error_message`
- Coordinates are in CSS pixels relative to the viewport
- Return valid JSON only — no markdown fences, no commentary
