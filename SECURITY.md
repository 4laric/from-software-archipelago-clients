# Security and release provenance

## What the Bloodborne client does

`bb-ap-client` opens the running shadPS4 process so it can read Bloodborne's
event flags and inventory state. It writes game memory only through the
documented, image-validated native item-delivery path. An unknown executable,
game build, runtime contract, or save context fails closed before delivery is
armed or an Archipelago item is acknowledged.

The companion launcher is maintained in the `bb-archipelago` repository. It
builds a seed-owned game overlay, records every managed file in a manifest, and
provides an undo path. This client repository does not silently install files
into the game directory.

## Verifying a build

The Windows CI job publishes `bb-ap-client.exe` together with
`bb-ap-client.exe.sha256`. The workflow summary records the same SHA-256 and
links to the exact source commit and run. Trusted push builds also receive a
GitHub/Sigstore build-provenance attestation. Verify a downloaded executable
with:

```text
gh attestation verify bb-ap-client.exe --repo 4laric/from-software-archipelago-clients
```

The packaged Bloodborne playtest and release remain owned by the
`4laric/bb-archipelago` release workflow. Use the hashes, attestation, and scan
links on that release rather than treating this repository's short-lived CI
artifact as a packaged release.

Process-memory clients and unsigned game tooling can trigger heuristic
antivirus detections. A detection is not dismissed solely as a false positive:
compare the file's hash, verify its attestation, and check that scan results are
consistent with previous releases before running it.

## Reporting a vulnerability

Please use this repository's **Security** tab to submit a private GitHub
security advisory. Include the affected commit or release, logs with secrets
removed, and the smallest reproduction you can provide. Do not publish an
unpatched memory-safety or item-duplication exploit in a public issue.
