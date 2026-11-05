# Changelog

All notable changes to iced-mcp. The three crates share one version.

## Unreleased

### Added

- **Layout diagnostics**: `diagnose` reports overlapping interactive widgets,
  content clipped by its container or the window (or squeezed to nothing),
  text that does not fit its widget, and duplicate widget ids. The flow step
  `expect_no_diagnostics` fails a flow on them. The bridge measures iced's
  `text` paragraphs and reports `text_fit` on text nodes.

- **Suite runner**: `iced-mcp run-suite [--only GLOB] [--repeat N]
  [--until-fail N] [--fail-fast] [--update-baselines] [--launch CMD]` runs
  every flow with PASS/FAIL per flow, a summary and exit status 0/1/2.
- **Failure bundles**: a failing flow step writes
  `.iced-mcp/failures/<flow>-<timestamp>/` with the tree, diagnostics,
  screenshot, step log, app output and recent messages; `flow_run` and the
  CLIs report its path.

### Changed

- Protocol version 6 (see *Upgrading* in the guide): rebuild the app and the
  server together.
- The workspace version is 0.2.0 (unpublished): the wire changed, and
  `cargo package` must verify against these sources, not the published 0.1.0.

## 0.1.0

First release.
