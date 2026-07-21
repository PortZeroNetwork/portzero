# Windows signing runbook

This document tracks the Windows signing setup for public Port Zero releases.

## Current state

- Windows Defender is deleting the unsigned Windows release artifacts on Windows
  11.
- The old MSI installed into `C:\Program Files (x86)\Port Zero` because the WiX
  package was not marked as x64.
- The installer has been updated locally to install into 64-bit Program Files,
  add the install folder to machine `PATH`, and package `portzero.exe` plus
  `wintun.dll`.
- Windows autostart has been updated locally to run:

  ```text
  portzero.exe start --foreground
  ```

  instead of the nonexistent `daemon` subcommand.

- Azure Artifact Signing is the chosen signing path.
- Loum Technologies has completed Microsoft identity validation.
- The release workflow signs `portzero.exe` before creating the Windows ZIP,
  then signs the MSI before preparing the winget manifest. Winget is coming
  soon; release artifacts are uploaded separately for now.

## Desired release flow

The Windows release pipeline should do this in order:

1. Build the Windows ZIP payload containing `portzero.exe` and `wintun.dll`.
2. Sign `portzero.exe`.
3. Verify the `portzero.exe` Authenticode signature.
4. Build the MSI from the signed payload.
5. Sign the MSI.
6. Verify the MSI Authenticode signature.
7. Upload the signed ZIP and signed MSI as release artifacts.
8. Submit the first signed artifacts to Microsoft as false positives if Defender
   still flags them.

The EXE must be signed before MSI packaging so the installed binary is signed.
The MSI must also be signed so Windows trusts the installer container.

## Azure Artifact Signing resources

Fill these in after verification completes:

```text
Azure subscription:
Resource group:
Artifact Signing account:
Region:
Endpoint:
Identity validation name: Loum Technologies, LLC
Identity validation ID: b09e34f4-c92a-410e-a085-753a19b81ee9
Certificate profile name:
Certificate profile type: Public Trust
GitHub Actions principal / managed identity:
```

Use a **Public** identity validation and a **Public Trust** certificate profile
for public releases. Private trust is only for internal enterprise policy
signing and will not solve public Windows install/download trust.

## Required Azure roles

For identity validation in the Azure Portal, the interactive user needs:

```text
Artifact Signing Identity Verifier
Reader
```

For signing from GitHub Actions, the workflow identity needs:

```text
Artifact Signing Certificate Profile Signer
```

Assign the signing role at the certificate profile scope if possible:

```text
/subscriptions/<subscription-id>/resourceGroups/<resource-group>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/<profile>
```

Azure role assignments may take several minutes to propagate.

## GitHub Actions values

Prefer GitHub OIDC / Azure federated credentials over long-lived client secrets.
Store these as GitHub Actions variables:

```text
AZURE_CLIENT_ID
AZURE_TENANT_ID
AZURE_SUBSCRIPTION_ID
ARTIFACT_SIGNING_ENDPOINT
ARTIFACT_SIGNING_ACCOUNT
ARTIFACT_SIGNING_PROFILE
```

The endpoint is region-specific, for example:

```text
https://eus.codesigning.azure.net
```

The Azure app registration used by GitHub Actions needs a federated credential
for this repository and the release branch/ref used to publish releases. The
release workflow grants `id-token: write`, authenticates with `azure/login`,
and signs with `azure/artifact-signing-action`.

## Signing command shape

Artifact Signing integrates with `signtool.exe` through the Azure Code Signing
dlib. The final workflow should generate a metadata file like:

```json
{
  "Endpoint": "<ARTIFACT_SIGNING_ENDPOINT>",
  "CodeSigningAccountName": "<ARTIFACT_SIGNING_ACCOUNT>",
  "CertificateProfileName": "<ARTIFACT_SIGNING_PROFILE>"
}
```

Then sign files with:

```powershell
signtool sign `
  /v `
  /fd SHA256 `
  /tr "http://timestamp.acs.microsoft.com" `
  /td SHA256 `
  /dlib "<path-to-Azure.CodeSigning.Dlib.dll>" `
  /dmdf ".\metadata.json" `
  "<file-to-sign>"
```

Verify with:

```powershell
signtool verify /pa /v "<signed-file>"
Get-AuthenticodeSignature "<signed-file>"
```

Expected verification result:

```text
Status: Valid
```

## Microsoft Defender submission

If Defender still detects the first signed build, submit the exact signed files
to Microsoft as a software developer false positive:

```text
https://www.microsoft.com/en-us/wdsi/filesubmission
```

Submit:

- signed `portzero.exe`
- signed MSI
- signed ZIP payload, if the ZIP is also flagged

Suggested context:

```text
Port Zero is a local development networking CLI. It creates a local overlay and
uses wintun.dll for Windows TUN support. These are official signed release
artifacts built from https://github.com/PortZeroNetwork/portzero-local.
```

## Local diagnostic commands

Check Defender detections:

```powershell
Get-MpThreatDetection |
  Sort-Object InitialDetectionTime -Descending |
  Select-Object -First 10 InitialDetectionTime,ThreatName,Resources
```

Check signatures:

```powershell
Get-AuthenticodeSignature '.\portzero.exe'
Get-AuthenticodeSignature '.\portzero-*.msi'
```

Check installed files:

```powershell
Get-ChildItem 'C:\Program Files\Port Zero','C:\Program Files (x86)\Port Zero' -ErrorAction SilentlyContinue
```
