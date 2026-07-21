# macOS signing and notarization

Port Zero's macOS release binaries (`portzero`, `portzero-tray`,
`portzero-app`) are currently shipped **unsigned**. The release workflow
(`.github/workflows/release.yml`) contains optional codesigning and
notarization steps that are **off by default** and turn on only when the
required secrets are present. Until then, CI behavior is unchanged: the darwin
tarball is the same unsigned artifact it has always been.

For the user-facing consequence (Gatekeeper quarantine on browser-downloaded
tarballs) and the `xattr -d com.apple.quarantine` workaround, see the install
sections in `README.md` and the Homebrew formula caveats
(`packaging/homebrew/Formula/portzero.rb`).

## How the gate works

The darwin legs of the `build` matrix run on GitHub-hosted `macos-15` runners
(not the self-hosted runner), so `codesign` / `xcrun notarytool` run as the
runner user with no `sudo` and no host mutation.

GitHub Actions does not allow `secrets.*` in a step `if:`. The workflow uses
the standard pattern instead:

1. **Check macOS signing secrets** — a shell step (id `macos_signing`) that
   reads the secrets via `env:` and writes two outputs: `sign` and `notarize`
   (`true`/`false`).
2. **Codesign macOS binaries** — runs only when `sign == 'true'`. Imports the
   Developer ID Application certificate into an ephemeral keychain and signs
   each binary with `--options runtime` (hardened runtime) and a secure
   timestamp, then deletes the keychain.
3. **Notarize macOS binaries** — runs only when `notarize == 'true'`. Zips the
   signed binaries and submits them with `xcrun notarytool submit --wait`.
   Bare CLI binaries can't be stapled, so notarization registers the signed
   hashes with Apple; Gatekeeper verifies online on first launch.

All three steps sit between **Build** and **Package (Unix)**, so the tarball
that ships contains the signed (and notarization-registered) binaries.

With the secrets unset, step 1 outputs `false`/`false` and steps 2–3 are
skipped entirely — a strict no-op.

## Secrets that enable it

Set these as repository (or environment) secrets to opt in. Codesigning turns
on when the first two are present; notarization additionally needs the last
three.

| Secret | Purpose |
|--------|---------|
| `MACOS_SIGNING_CERT_P12` | Base64-encoded Developer ID Application certificate (`.p12`), including its private key. |
| `MACOS_SIGNING_CERT_PASSWORD` | Password that protects the `.p12`. |
| `MACOS_NOTARY_APPLE_ID` | Apple ID used for notary submissions. |
| `MACOS_NOTARY_TEAM_ID` | Apple Developer Team ID. |
| `MACOS_NOTARY_PASSWORD` | App-specific password for the notary Apple ID. |

To produce `MACOS_SIGNING_CERT_P12`, export the Developer ID Application
certificate + key from Keychain Access as a `.p12`, then
`base64 -i cert.p12 | pbcopy` and paste the result into the secret.

Codesigning without the notary secrets is valid: binaries are signed but not
notarized. Notarization requires signing, so setting only the notary secrets
does nothing until the signing cert is also present.

## Enabling notes

- Use a **Developer ID Application** certificate (not "Mac App Distribution")
  — that is the identity Gatekeeper checks for software distributed outside the
  App Store.
- An **app-specific password** (appleid.apple.com → Sign-In and Security) is
  required for `MACOS_NOTARY_PASSWORD`; a normal Apple ID password will be
  rejected by the notary service.
- Notarization is a network round-trip to Apple and can take a few minutes;
  `--wait` blocks the step until it resolves so a rejection fails the build.
