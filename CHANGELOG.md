# Changelog

All notable changes to iced-mcp. The three crates share one version.

## Unreleased

### Added

- **Layout diagnostics**: `diagnose` reports overlapping interactive widgets,
  content clipped by its container or the window (or squeezed to nothing),
  text that does not fit its widget, and duplicate widget ids. The flow step
  `expect_no_diagnostics` fails a flow on them. The bridge measures iced's
  `text` paragraphs and reports `text_fit` on text nodes.

### Changed

- Protocol version 6 (see *Upgrading* in the guide): rebuild the app and the
  server together.
- The workspace version is 0.2.0 (unpublished): the wire changed, and
  `cargo package` must verify against these sources, not the published 0.1.0.

## 0.1.0

First release.
