# Sciotte — Sport Activity Scraper

**Headless Chrome scraper** for fitness platform login flows (Strava, Garmin). Core capabilities:
- `chromiumoxide` 0.9 browser automation with cookie/session capture
- TOML-configurable provider pipelines (selectors, flows, 2FA handling)
- In-memory session caching with `moka` TTL
- Exposed as library, MCP server, and REST server binaries

See [README.md](README.md) for provider setup.

---

## Git Hooks - MANDATORY for ALL AI Agents


## Mandatory Session Setup (ALL AI Agents)

**Run these commands at the START OF EVERY SESSION:**

```bash
# 1. Initialize shared build config (required for validation)
git submodule update --init --recursive

# 2. Set git hooks
git config core.hooksPath .build/hooks
```

## Mandatory Pre-Push Validation

**Before EVERY push, run:**

```bash
# 1. Format
cargo fmt --all

# 2. Clippy with warnings as errors
cargo clippy --workspace --all-targets -- -D warnings

# 3. Architectural validation (MUST exit 0)
.build/validation/validate.sh
```

**DO NOT push if `.build/validation/validate.sh` fails.** Fix all reported issues first.

The validation checks: placeholder code, forbidden anyhow usage, problematic unwraps/expects/panics,
underscore-prefixed names, unauthorized clippy allows, dead code annotations, test integrity, and more.

```bash
git config core.hooksPath .githooks
```

This enables commit-msg hooks. Sessions get archived/revived, so this must run EVERY time you start working.

**NEVER use `--no-verify` when committing or pushing.** The hooks enforce:
- Commit message format (max 2 lines, conventional commits)
- No AI-generated commit signatures (🤖, "Generated with", "Co-Authored-By: Claude", etc.)

## Commit Messages

- **Maximum 2 lines** — line 1: summary, line 2: optional detail
- **Conventional commit format**: `type(scope): description`
- **NEVER add AI attribution** — no `Co-Authored-By:`, no `Generated with`, no 🤖
- **NEVER add AI-generated commit text** in commit messages
- Keep line 1 under 72 characters (max 100)

## Git Workflow: NO Pull Requests

**CRITICAL: NEVER create Pull Requests. All merges happen locally via squash merge.**

### Rules
- **NEVER use `gh pr create`** or any PR creation command
- **NEVER suggest creating a PR**
- Feature branches are merged via **local squash merge**

### Workflow for Features
1. Create feature branch: `git checkout -b feature/my-feature`
2. Make commits, push to remote: `git push -u origin feature/my-feature`
3. When ready, squash merge locally (from main worktree):
   ```bash
   git checkout main
   git fetch origin
   git merge --squash origin/feature/my-feature
   git commit
   git push
   ```

### Bug Fixes
- Bug fixes go directly to `main` branch (no feature branch needed)
- Commit and push directly: `git push origin main`

## Release Safety — MANDATORY

Before triggering a release:
1. **CI must pass** — never release without green CI (`gh run list --limit 1`)
2. **Verify `cargo package`** — run `cargo package -p dravr-sciotte --list --allow-dirty` and confirm all `include_str!` / `include_bytes!` files are present
3. **Check `exclude` in Cargo.toml** — if you add files referenced by `include_str!()` or `include_bytes!()`, ensure they are NOT in the `exclude` list
4. **Never swallow publish errors** — the `|| echo "already published"` fallback in release.yml can mask real failures. Check crates.io after release to confirm the version is live

## Rust Workspace Architecture

The backend is a Cargo workspace with 3 crates:

| Crate | Description |
|-------|-------------|
| `dravr-sciotte` | Core library with traits, models, scraping engine, and cache |
| `dravr-sciotte-mcp` | MCP server exposing scraping via Model Context Protocol |
| `dravr-sciotte-server` | Unified REST API + MCP server + CLI binary |

# Writing code

- CRITICAL: NEVER USE --no-verify WHEN COMMITTING CODE
- We prefer simple, clean, maintainable solutions over clever or complex ones, even if the latter are more concise or performant. Readability and maintainability are primary concerns.
- Make the smallest reasonable changes to get to the desired outcome. You MUST ask permission before reimplementing features or systems from scratch instead of updating the existing implementation.
- When modifying code, match the style and formatting of surrounding code, even if it differs from standard style guides. Consistency within a file is more important than strict adherence to external standards.
- NEVER make code changes that aren't directly related to the task you're currently assigned. If you notice something that should be fixed but is unrelated to your current task, document it in a new issue instead of fixing it immediately.
- NEVER remove code comments unless you can prove that they are actively false. Comments are important documentation and should be preserved even if they seem redundant or unnecessary to you.
- All code files should start with a brief 2 line comment explaining what the file does. Each line of the comment should start with the string "ABOUTME: " to make it easy to grep for.
- When writing comments, avoid referring to temporal context about refactors or recent changes. Comments should be evergreen and describe the code as it is, not how it evolved or was recently changed.
- When you are trying to fix a bug or compilation error or any other issue, YOU MUST NEVER throw away the old implementation and rewrite without explicit permission from the user. If you are going to do this, YOU MUST STOP and get explicit permission from the user.
- NEVER name things as 'improved' or 'new' or 'enhanced', etc. Code naming should be evergreen. What is new today will be "old" someday.
- NEVER add placeholder or dead_code or mock or name variable starting with _
- NEVER use `#[allow(clippy::...)]` attributes EXCEPT for type conversion casts (`cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss`) when properly validated - Fix the underlying issue instead of silencing warnings
- Be RUST idiomatic
- Do not hard code magic value
- Do not leave implementation with "In future versions" or "Implement the code" or "Fall back". Always implement the real thing.
- Commit without AI assistant-related commit messages. Do not reference AI assistance in git commits.
- Do not add AI-generated commit text in commit messages
- Always create a branch when adding new features. Bug fixes go directly to main branch.
- Always run validation after making changes: cargo fmt, then clippy on changed crates, then TARGETED tests
- avoid #[cfg(test)] in the src code. Only in tests

## Required Pre-Commit Validation

### Tiered Validation Approach

#### Tier 1: Quick Iteration (during development)
Run after each code change to catch errors fast:
```bash
# 1. Format code
cargo fmt

# 2. Compile check only (fast - no linting)
cargo check --quiet

# 3. Run ONLY tests related to your changes
cargo test --test <test_file> <test_name_pattern> -- --nocapture
```

#### Tier 2: Pre-Commit (before committing)
Run before creating a commit:
```bash
# 1. Format code
cargo fmt

# 2. Clippy — ONLY the crate(s) you actually changed
# Cargo.toml defines all lint levels - no CLI flags needed
cargo clippy -p dravr-sciotte              # core library
cargo clippy -p dravr-sciotte-mcp          # MCP server
cargo clippy -p dravr-sciotte-server       # REST+MCP server
# Add --all-targets ONLY if test files in that crate changed

# 3. Run TARGETED tests for changed modules
cargo test --test <test_file> <test_pattern> -- --nocapture
```

#### Tier 3: Full Validation (before merge only)
```bash
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
```

## Error Handling Requirements

### Acceptable Error Handling
- `?` operator for error propagation
- `Result<T, E>` for all fallible operations
- `Option<T>` for values that may not exist
- Custom error types implementing `std::error::Error`

### Prohibited Error Handling
- `unwrap()` except in:
  - Test code with clear failure expectations
  - Static data known to be valid at compile time
  - Binary main() functions where failure should crash the program
- `expect()` - Acceptable ONLY for documenting invariants that should never fail
- `panic!()` - Only in test assertions or unrecoverable binary errors

# RUST IDIOMATIC CODE GENERATION

## Memory Management and Ownership
- PREFER borrowing `&T` over cloning when possible
- PREFER `&str` over `String` for function parameters (unless ownership needed)
- PREFER `&[T]` over `Vec<T>` for function parameters (unless ownership needed)
- NEVER clone Arc contents - clone the Arc itself: `arc.clone()` not `(*arc).clone()`

## Collection and Iterator Patterns
- PREFER iterator chains over manual loops
- PREFER `filter_map()` over `filter().map()`
- PREFER `and_then()` over nested match statements for Options/Results
- PREFER `Vec::with_capacity()` when size is known

## Async/Await Patterns
- PREFER `async fn` over `impl Future`
- USE `tokio::spawn()` for concurrent background tasks
- PREFER structured concurrency with `tokio::join!()` and `tokio::select!()`

## Function Design
- PREFER small, focused functions (max 50 lines)
- PREFER composition over inheritance
- USE builder pattern for complex construction

## Code Organization
- PREFER flat module hierarchies over deep nesting
- GROUP related functionality in modules
- PREFER re-exports at crate root for public APIs

# Testing

- Tests MUST cover the functionality being implemented.
- NEVER ignore the output of the system or the tests - Logs and messages often contain CRITICAL information.
- NO EXCEPTIONS POLICY: Under no circumstances should you mark any test type as "not applicable". Every change MUST have tests.

## Test Integrity: No Skipping, No Ignoring

### Forbidden Patterns
- **Rust**: NEVER use `#[ignore]` attribute on tests
- **CI Workflows**: NEVER use `continue-on-error: true` on test jobs

### If a Test Fails
1. **Fix the code** - not the test
2. **Fix the test** - only if the test itself is wrong
3. **Ask for help** - if you're stuck, don't skip

# Getting help

- ALWAYS ask for clarification rather than making assumptions.
- If you're having trouble with something, it's ok to stop and ask for help.

## Mandatory Session Startup Checklist

Before touching any code in a new session, run in this order:

```bash
# 1. Pull shared build config (provides .build/hooks, .build/validation, etc.)
git submodule update --init --recursive

# 2. Set canonical git hooks path — ALWAYS .build/hooks, NEVER .githooks
git config core.hooksPath .build/hooks

# 3. Scan recent history for context
git log --oneline -10

# 4. Check CI health on main
gh run list --branch main --limit 10 --json workflowName,conclusion

# 5. See uncommitted work
git status
```

**If any workflow on main has been red for 2+ runs, STOP and surface it to the user** before starting the requested task. Ask: "Should I investigate CI before doing X?"

The canonical hooks/validation live in the `.build/` git submodule from
https://github.com/dravr-ai/dravr-build-config — never use a local `.githooks/`.

## Architectural Discipline

### Single Source of Truth (SSOT)
Before adding a new abstraction (registry, manager, factory, handler, schema module):
1. Grep for existing abstractions with similar purposes
2. If one exists, USE IT or DOCUMENT WHY it's being replaced + DELETE the old in the same commit
3. Never leave two systems doing the same job "for compat"

### No Orphan Migrations
If you introduce a "v2" of something:
- Migrate ALL callers in the same session, OR
- Record remaining work in memory (`type: project`) with explicit list of what's left
- NEVER leave "for compat" code without a tracked deletion date

### When Adding, Remove
Every commit that adds a new abstraction must identify what it replaces and delete that. If nothing is replaced, the commit message must justify why the new abstraction is needed.

### Complete Deletion, Not Deprecation
Don't mark code `// DEPRECATED` or `// TODO remove later`. Delete it. If deletion is blocked, file an issue and link it from the code.

## Pushback Triggers — When to Stop and Ask

STOP and ask the user before proceeding when you find:

1. **Duplication** — two systems/modules doing similar things
   → "Is this intentional? Should I consolidate before adding my feature?"
2. **Stale state** — `TODO`, `FIXME`, `for compat`, `temporary`, `v2` comments in code you're touching
   → "Is this still needed? Should I resolve it first?"
3. **Red CI** — workflows failing on main
   → "Should I fix CI first before doing the task?"
4. **Version drift** — two versions of the same dependency in Cargo.lock
   → "Is this intentional or should it be consolidated?"
5. **Request conflicts with architecture** — user asks you to add X but X exists differently
   → Surface the existing thing, ask which to use
6. **Half-finished migrations** — both old and new paths still live
   → "Finish migration first, or add feature on top?"

Default behavior is to complete the requested task. These triggers override that.
