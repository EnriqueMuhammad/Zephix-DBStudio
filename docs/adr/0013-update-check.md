# ADR 0013 — Update check against GitHub releases

Status: accepted (2026-11)

## Context

Releases are published on GitHub (`EnriqueMuhammad/Zephix-DBStudio`) by the tag-triggered
`release.yml`: a per-user installer, a portable executable and `SHA256SUMS.txt`. Users had no
way to learn about a new version from inside the app. The workspace had no HTTP client; the
database drivers use rustls with the `ring` provider.

## Decision

- **Help > Check for Updates…** and, unless turned off, one check at start-up. The start-up
  check is silent unless a newer version exists; then a toast appears for a few seconds and a
  "New version X available" item stays in the status bar. Both open a dialog with the release
  page. **Skip This Version** hides the toast and the status-bar item for that version only.
- The check reads the releases list (`/repos/…/releases`), ignores drafts, pre-releases and
  tags that are not `vX.Y.Z`, and takes the highest version. The latest-release endpoint is not
  used because it does not filter the tag format or order by version. Parsing and comparison
  live in `rdbs_core::update` and are unit-tested.
- Network I/O follows ADR 0003: the UI sends `Command::CheckForUpdates`, the worker runs the
  request on Tokio's blocking pool and answers with `Event::UpdateChecked`. A frame never waits.
- HTTP client: `ureq` 3 with `default-features = false` and `rustls` (ring provider, the same
  rustls as the drivers). It adds `ureq`, `ureq-proto`, `http`, `httparse` and `utf8-zero`,
  far fewer crates than `reqwest` + `hyper`, and one blocking GET needs nothing more. Roots come
  from the OS store (so a corporate TLS proxy is trusted like in the browser), falling back to
  the bundled Mozilla roots. The request sends a `Zephix/<version>` User-Agent, which the
  GitHub API requires, and gives up after 15 seconds.
- The app does not download or install updates. It opens the release page, which has the
  installer, the portable executable and their checksums. Replacing a running executable and
  elevating an installer are platform work with little gain over one click in the browser.
- Settings (`check on start-up`, `skipped version`) are stored with the other UI preferences in
  eframe's storage (`app.ron`).

## Consequences

- One anonymous request per start (60 per hour per IP is GitHub's limit). A rate-limit or
  network error is logged at start-up and only shown for a manual check.
- No account, token or telemetry is involved; GitHub sees the IP address and the version in
  the User-Agent.
