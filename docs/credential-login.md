# Credential Login and Two-Factor Authentication

This document describes how dravr-sciotte performs programmatic login — filling email and password
forms in a real Chrome browser via CDP — and how it handles two-factor authentication (2FA) flows
that follow.

## Overview

Credential login lets the server authenticate against a provider on behalf of the user without
requiring any manual browser interaction. The server launches a Chrome window, navigates to the
provider's login page, fills in the credentials, and polls for the outcome. If the provider
requires 2FA, the browser session is held open so the caller can complete the additional step
through follow-up API calls.

The flow is stateful: the browser and page are kept alive between the initial login call and any
subsequent 2FA calls. Callers must complete or abandon a pending 2FA session before starting a new
one.

---

## Login Methods

The `method` field on the `credential_login` call selects which login flow to execute.

### Email/Password (`method = "email"`)

The default flow. The scraper navigates to the provider's `login_url`, fills the email field
identified by `login_email_selector`, then checks whether a password field is already visible.

**Two-step form detection.** Some providers (including Strava) show the email field first, then
reveal the password field on the next screen after the user submits the email. The scraper handles
this automatically:

1. Fill the email field.
2. Check whether the password field (`login_password_selector`) is already in the DOM and visible.
3. If not visible, click the submit button and wait for the password field to appear (up to
   `email_step_timeout_secs`, default 10 s).
4. Fill the password field.
5. Click the submit button.
6. Poll for a success URL pattern, an OTP/challenge URL, or an error message.

**Error detection.** After the password submit, the scraper checks the element identified by
`login_error_selector` on every poll cycle. When that element contains visible text, the login
returns `Failed` with the error message.

All form input uses CDP `Input.insertText` rather than JavaScript property manipulation. This
ensures compatibility with React, Angular, and other frameworks that intercept DOM value setters.

### Google OAuth (`method = "google"`)

Navigates through Google's sign-in pages using a fixed set of selectors that target Google's
published form elements.

Flow:

1. Navigate to the provider's `login_url`.
2. Click the button identified by `login_oauth_buttons.google` (e.g., `text:Sign In With Google`).
3. Wait for Google's account chooser or email page.
4. Fill `input[type="email"]`, click `#identifierNext`.
5. Wait for the password page to load.
6. Fill `input[type="password"]` or `input[name="Passwd"]`, click `#passwordNext`.
7. Poll for the final result.

**Passkey bypass.** Google sometimes redirects to `/challenge/pk` (a passkey prompt) instead of
the password page. The scraper detects this URL pattern and automatically:

1. Clicks "Try another way" (`text:Try another way, text:Essayer autrement`).
2. CDP-clicks the "Enter your password" option by evaluating `getBoundingClientRect()` on each
   `[data-challengetype]` element and clicking at the center coordinates.
3. Repeats the CDP click once (Google sometimes requires two clicks on this page).

The passkey bypass runs transparently inside `poll_for_next_step` and does not affect the return
value seen by callers.

### Apple OAuth (`method = "apple"`)

Follows the same pattern as Google OAuth but targets Apple's sign-in page elements:

- Email: `#account_name_text_field, input[type="text"]`
- Email next: `#sign-in, button[type="submit"]`
- Password: `#password_text_field, input[type="password"]`
- Password next: `#sign-in, button[type="submit"]`

---

## Two-Factor Authentication

### When 2FA Is Triggered

After the password step, the scraper monitors the current URL on every poll cycle:

| URL pattern contains | Action |
|---|---|
| `challenge/totp`, `challenge/sms`, `challenge/ipp`, `verify`, `2fa`, `mfa`, `otp` | Returns `OtpRequired` |
| `/challenge/` and not `challenge/pk` or `challenge/pwd` | Parses the page for selectable 2FA options and returns `TwoFactorChoice` |
| `challenge/pk` | Passkey page — handled automatically (see Google OAuth above) |
| `challenge/pwd` | Password re-entry page — skipped, polling continues |
| A `login_success_patterns` URL | Returns `Success` |

### `TwoFactorChoice` — Selecting a Method

When the provider shows a page where the user chooses a 2FA method, the scraper reads all
`[data-challengetype]` elements, filters by visibility (`getBoundingClientRect` width and height
both non-zero), and maps each element's text to a canonical `id`:

| Text contains | Canonical `id` |
|---|---|
| "authenticator", "verification code", "code de validation" | `otp` |
| "tap", "yes on your", "appuyez" | `app` |
| "text message", "sms", "texto" | `sms` |
| "backup", "secours" | `backup` |

The result is a `Vec<TwoFactorOption>` where each entry has an `id` and a `label` (the raw button
text, truncated to 120 characters). The browser and page remain open until `select_two_factor` is
called.

### `select_two_factor(option_id)`

Finds the previously parsed 2FA option by `id`, CDP-clicks at its stored screen coordinates, then
polls for the next result.

Timeout behavior differs by option type:

- `app` (phone tap) — waits up to `phone_tap_timeout_secs` (default 60 s) because the user must
  physically approve on their device.
- All other options — wait up to `password_step_timeout_secs` (default 10 s).

If the option `id` is not found on the page, the browser session is put back into the pending slot
and `ScraperError::Auth` is returned so the caller can retry with a different `id`.

Returns `OtpRequired`, `Success`, or `Failed`.

### `submit_otp(code)`

Called after `credential_login` or `select_two_factor` returns `OtpRequired`.

Steps:

1. Fill the OTP input field identified by the provider's `login_otp_selector`.
2. Click the submit button. The selector combines the provider's `login_button_selector` with
   Google's `#totpNext button, #totpNext, text:Next` as a fallback — whichever matches first is
   clicked.
3. Wait one page-load cycle, then poll for the final result.

If the code is accepted, returns `Success` with a new `AuthSession`. If the provider asks for
another step, returns `OtpRequired` or `TwoFactorChoice` again and keeps the browser alive.

---

## `LoginResult` Enum

`credential_login`, `submit_otp`, and `select_two_factor` all return `ScraperResult<LoginResult>`.

```rust
pub enum LoginResult {
    /// Login succeeded. Contains the captured session cookies.
    Success(AuthSession),

    /// Provider requires a one-time password or 2FA code.
    /// Call submit_otp() with the code.
    OtpRequired,

    /// Provider shows a method-selection page.
    /// Call select_two_factor() with one of the returned option ids.
    TwoFactorChoice(Vec<TwoFactorOption>),

    /// Login was rejected (wrong password, account locked, etc.).
    /// The inner String is the error text from login_error_selector.
    Failed(String),
}

pub struct TwoFactorOption {
    /// Machine-readable identifier: "otp", "app", "sms", or "backup"
    pub id: String,
    /// Human-readable label from the provider's challenge page (up to 120 chars)
    pub label: String,
}
```

---

## Sequence Diagrams

### Google OAuth with Authenticator App 2FA

```
Client                    Server (scraper)               Chrome             Google
  |                            |                            |                  |
  |-- POST /auth/login-with-credentials ----------------->  |                  |
  |   { email, password,       |                            |                  |
  |     method: "google" }     |                            |                  |
  |                            |-- launch Chrome ---------->|                  |
  |                            |-- navigate login_url ----->|                  |
  |                            |-- click Sign In With Google|                  |
  |                            |                            |-- redirect ------>|
  |                            |-- fill email ------------->|                  |
  |                            |-- click #identifierNext -->|                  |
  |                            |                            |-- /challenge/pk ->|
  |                            |   (passkey bypass)         |                  |
  |                            |-- click Try another way -->|                  |
  |                            |-- CDP-click Enter password>|                  |
  |                            |-- fill password ---------->|                  |
  |                            |-- click #passwordNext ---->|                  |
  |                            |                            |-- /challenge/ --->|
  |                            |   (parse 2FA options)      |                  |
  |<-- 200 { status: "two_factor_choice",                   |                  |
  |          options: [{id:"otp",label:"..."},              |                  |
  |                    {id:"app",label:"..."}] } -----------|                  |
  |                            |                            |                  |
  |-- POST /auth/select-2fa -->|                            |                  |
  |   { option_id: "otp" }     |                            |                  |
  |                            |-- CDP-click otp option --->|                  |
  |                            |                            |-- /challenge/totp>|
  |<-- 200 { status: "otp_required" } ---------------------|                  |
  |                            |                            |                  |
  |-- POST /auth/submit-otp -->|                            |                  |
  |   { code: "123456" }       |                            |                  |
  |                            |-- fill OTP field ---------->|                 |
  |                            |-- click #totpNext --------->|                 |
  |                            |                             |-- dashboard ---->|
  |                            |-- capture cookies --------->|                 |
  |<-- 200 { status: "authenticated",                        |                 |
  |          session_id: "...",                              |                 |
  |          cookie_count: 12 } ----------------------------|                 |
```

### Email/Password (No 2FA)

```
Client                    Server (scraper)               Chrome            Provider
  |                            |                            |                  |
  |-- POST /auth/login-with-credentials ----------------->  |                  |
  |   { email, password,       |                            |                  |
  |     method: "email" }      |                            |                  |
  |                            |-- launch Chrome ---------->|                  |
  |                            |-- navigate login_url ----->|-- load page ----->|
  |                            |-- fill email field ------->|                  |
  |                            |-- (password not visible)   |                  |
  |                            |-- click submit ----------->|-- show password -->|
  |                            |-- fill password ---------->|                  |
  |                            |-- click submit ----------->|-- redirect ------>|
  |                            |-- (URL matches success)    |                  |
  |                            |-- capture cookies -------->|                  |
  |<-- 200 { status: "authenticated",                       |                  |
  |          session_id: "...",                             |                  |
  |          cookie_count: 8 } ----------------------------|                  |
```

---

## REST API

All credential login endpoints require the `Authorization: Bearer <token>` header when
`DRAVR_SCIOTTE_API_KEY` is set. Omit the header if no API key is configured.

### `POST /auth/login-with-credentials`

Start a programmatic login.

**Request body:**

```json
{
  "email": "user@example.com",
  "password": "s3cr3t",
  "method": "google"
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `email` | string | yes | Email or username |
| `password` | string | yes | Account password |
| `method` | string | no | `"email"` (default), `"google"`, or `"apple"` |

**Responses:**

`200 OK` — login succeeded:
```json
{
  "status": "authenticated",
  "session_id": "67d2b4c0-1a2b3c",
  "cookie_count": 12
}
```

`200 OK` — OTP code required:
```json
{
  "status": "otp_required",
  "reason": "Provider requires a one-time password or 2FA verification"
}
```

`200 OK` — method selection required:
```json
{
  "status": "two_factor_choice",
  "options": [
    { "id": "otp", "label": "Get a verification code from the Google Authenticator app" },
    { "id": "app", "label": "Tap Yes on your phone or tablet" },
    { "id": "sms", "label": "Get a verification code at (•••) •••-5309" }
  ]
}
```

`401 Unauthorized` — wrong credentials:
```json
{
  "status": "failed",
  "reason": "Wrong password. Try again or click Forgot password to reset it."
}
```

**Example:**

```bash
curl -s -X POST http://localhost:3000/auth/login-with-credentials \
  -H "Authorization: Bearer myapikey" \
  -H "Content-Type: application/json" \
  -d '{"email":"user@example.com","password":"s3cr3t","method":"google"}'
```

---

### `POST /auth/select-2fa`

Choose a 2FA method after receiving `two_factor_choice`. The `option_id` must be one of the `id`
values returned in the previous response.

**Request body:**

```json
{
  "option_id": "otp"
}
```

**Responses:**

`200 OK` — method requires a code:
```json
{
  "status": "otp_required"
}
```

`200 OK` — method completed without a code (e.g., phone tap approved):
```json
{
  "status": "authenticated",
  "session_id": "67d2b4c0-1a2b3c",
  "cookie_count": 12
}
```

`401 Unauthorized` — method was rejected:
```json
{
  "status": "failed",
  "reason": "..."
}
```

**Example:**

```bash
curl -s -X POST http://localhost:3000/auth/select-2fa \
  -H "Authorization: Bearer myapikey" \
  -H "Content-Type: application/json" \
  -d '{"option_id":"otp"}'
```

---

### `POST /auth/submit-otp`

Submit the one-time password after receiving `otp_required`.

**Request body:**

```json
{
  "code": "123456"
}
```

**Responses:**

`200 OK` — code accepted:
```json
{
  "status": "authenticated",
  "session_id": "67d2b4c0-1a2b3c",
  "cookie_count": 12
}
```

`401 Unauthorized` — code rejected:
```json
{
  "status": "failed",
  "reason": "Wrong code. Try again."
}
```

**Example:**

```bash
curl -s -X POST http://localhost:3000/auth/submit-otp \
  -H "Authorization: Bearer myapikey" \
  -H "Content-Type: application/json" \
  -d '{"code":"123456"}'
```

---

## Library Usage (Rust)

The following example demonstrates the complete credential login flow with 2FA using the
`ActivityScraper` trait directly.

```rust
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte::ActivityScraper;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scraper = ChromeScraper::default_config();

    let result = scraper
        .credential_login("user@example.com", "s3cr3t", "google")
        .await?;

    let session = match result {
        LoginResult::Success(session) => {
            println!("Logged in, session: {}", session.session_id);
            session
        }

        LoginResult::TwoFactorChoice(options) => {
            println!("Choose a 2FA method:");
            for opt in &options {
                println!("  [{}] {}", opt.id, opt.label);
            }

            // Select the authenticator app option
            let result = scraper.select_two_factor("otp").await?;

            match result {
                LoginResult::OtpRequired => {
                    // Prompt the user for the code from their authenticator app
                    let code = read_otp_from_user();
                    match scraper.submit_otp(&code).await? {
                        LoginResult::Success(session) => session,
                        LoginResult::Failed(reason) => {
                            eprintln!("OTP rejected: {reason}");
                            return Ok(());
                        }
                        other => {
                            eprintln!("Unexpected result after OTP: {other:?}");
                            return Ok(());
                        }
                    }
                }
                LoginResult::Success(session) => session,
                LoginResult::Failed(reason) => {
                    eprintln!("2FA method failed: {reason}");
                    return Ok(());
                }
                other => {
                    eprintln!("Unexpected 2FA result: {other:?}");
                    return Ok(());
                }
            }
        }

        LoginResult::OtpRequired => {
            // Provider went straight to OTP (no method selection)
            let code = read_otp_from_user();
            match scraper.submit_otp(&code).await? {
                LoginResult::Success(session) => session,
                LoginResult::Failed(reason) => {
                    eprintln!("OTP rejected: {reason}");
                    return Ok(());
                }
                other => {
                    eprintln!("Unexpected result: {other:?}");
                    return Ok(());
                }
            }
        }

        LoginResult::Failed(reason) => {
            eprintln!("Login failed: {reason}");
            return Ok(());
        }
    };

    println!(
        "Session captured ({} cookies)",
        session.cookies.len()
    );
    Ok(())
}

fn read_otp_from_user() -> String {
    use std::io::{self, Write};
    print!("Enter 2FA code: ");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_owned()
}
```

---

## Provider Configuration

The TOML fields below are required for credential login to work. All fields live under
`[provider]`.

### Required for all `method` values

| Field | Purpose |
|---|---|
| `login_url` | URL the browser navigates to before filling any form |
| `login_success_patterns` | URL substrings that indicate the user is now logged in |
| `login_failure_patterns` | URL substrings that indicate the user is still on a login/error page |

### Required for `method = "email"`

| Field | Purpose |
|---|---|
| `login_email_selector` | CSS selector for the email input field |
| `login_password_selector` | CSS selector for the password input field |
| `login_button_selector` | CSS selector for the submit button |

### Optional but recommended for `method = "email"`

| Field | Purpose |
|---|---|
| `login_error_selector` | CSS selector for the error message element; enables `Failed` detection |
| `login_otp_selector` | CSS selector for the OTP code input field; required for `submit_otp` |

### Required for `method = "google"` or `method = "apple"`

| Field | Purpose |
|---|---|
| `login_oauth_buttons.google` | Selector for the "Sign in with Google" button on the provider's page |
| `login_oauth_buttons.apple` | Selector for the "Sign in with Apple" button on the provider's page |
| `login_otp_selector` | CSS selector for the OTP input field when 2FA requires a code |

### Strava example (`providers/strava.toml`)

```toml
[provider]
name = "strava"
login_url = "https://www.strava.com/login"
login_success_patterns = ["/dashboard", "/athlete", "/onboarding", "strava.com/feed"]
login_failure_patterns = ["/login", "/session", "/oauth"]
login_email_selector = '#email, input[name="email"]'
login_password_selector = '#password, input[name="password"]'
login_button_selector = 'button[type="submit"], #login-button'
login_error_selector = '.alert-error, .alert-danger, [class*="error-message"]'
login_otp_selector = 'input[name="code"], input[type="tel"], input[autocomplete="one-time-code"]'

[provider.login_oauth_buttons]
google = 'text:Sign In With Google'
apple = 'text:Sign In With Apple'
```

Selectors support comma-separated fallbacks. The engine tries each selector in order and uses the
first match. The `text:` prefix matches by button text content rather than CSS attribute, which is
useful for localized or dynamically generated button elements.

---

## Error Handling

| Condition | Return type | HTTP status |
|---|---|---|
| Wrong credentials detected via `login_error_selector` | `LoginResult::Failed(reason)` | 401 |
| No pending session when `submit_otp` or `select_two_factor` is called | `ScraperError::Auth` | 500 |
| Provider config missing a required selector | `ScraperError::Config` | 500 |
| Browser fails to launch or page fails to navigate | `ScraperError::Browser` | 500 |
| Login polling times out | `ScraperError::Auth` | 500 |
| OTP option id not found on page | `ScraperError::Auth` | 500 |

When a timeout occurs, the scraper saves a debug screenshot to the system temp directory
(`sciotte-login-timeout.png` or `sciotte-step-timeout.png`) and logs the path at `WARN` level.

`ScraperError::Browser` and `ScraperError::Network` are considered transient
(`is_transient() == true`) and may succeed on retry. `ScraperError::Auth` and
`ScraperError::Config` are not transient.
