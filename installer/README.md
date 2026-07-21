# Installer getting-started manifest

This directory holds the checked-in JSON that feeds the local
"Getting Started" section.

- `getting-started.json` is generated from `../portzero-examples`
  by `just examples-docs` in this repo.
- The installer and local UI should read this file directly instead of
  duplicating example metadata.
