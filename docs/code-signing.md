# Code-signing: what's wired up, and what's still needed

**Status as of round 16: infrastructure only.** `.github/workflows/release.yml`
has real, conditional signing steps for Windows and macOS, but neither has
ever run against a real certificate — there is no code-signing credential
anywhere in this project, on any machine that has touched it. Every
installer this pipeline has produced so far is unsigned. Nothing here
changes that by itself; it only means adding real credentials later is
"create these GitHub secrets," not "figure out the workflow from scratch."

Read this before assuming an installer is signed just because this file
exists. The only way to know is to check the Actions log for the run that
built it: look for `Imported certificate, signing identity: ...` /
`Imported certificate, thumbprint ...` (signed) versus `No ... secret
configured - shipping unsigned build` (not signed) — both messages are
printed explicitly and unconditionally by the relevant step, on purpose,
so this is never a guess.

## Windows

### What you need to obtain

A code-signing certificate from a recognized Certificate Authority (CA) —
DigiCert, Sectigo, SSL.com, GlobalSign are all real, commonly-used options.
Two things worth knowing before buying one:

- **OV (Organization Validation) vs EV (Extended Validation)**: EV
  certificates get Windows SmartScreen reputation faster (sometimes
  immediately); OV certificates still have to build reputation over time
  (downloads without triggering the warning) even though the file is
  signed. Either removes the "unknown publisher" wording and shows your
  verified organization name instead, but only EV reliably skips the
  SmartScreen click-through immediately.
- **Exportable `.pfx` files are increasingly not what you get.** Since June
  2023, CAs are required to issue OV certificates on hardware (a USB
  token or an HSM), not as an exportable file — the classic
  `.pfx`-in-a-GitHub-secret approach this workflow currently implements
  only works for a certificate you can actually export as a `.pfx`. If
  your CA only offers HSM-backed issuance, look into **Azure Trusted
  Signing** (Microsoft's own cloud HSM signing service, built for exactly
  this CI scenario) instead — it needs a different GitHub Actions setup
  than what's below (Azure credentials, not a certificate file), not
  covered by this round's implementation. Ask your CA directly which kind
  they're issuing before buying, so you don't end up with a certificate
  this specific workflow step can't use.

### GitHub secrets this workflow expects

| Secret name | What it is |
|---|---|
| `WINDOWS_CERTIFICATE` | Your `.pfx` certificate file, base64-encoded (`base64 -w0 your-cert.pfx` on Linux/macOS, `certutil -encode your-cert.pfx cert.b64` on Windows then strip the header/footer lines) |
| `WINDOWS_CERTIFICATE_PASSWORD` | The password protecting that `.pfx` file |

Add both under **Settings → Secrets and variables → Actions → New repository secret** in this repo. Nothing else to configure — `build-windows` in `release.yml` already checks for `WINDOWS_CERTIFICATE` and imports it if present, computes the real certificate thumbprint, and passes it to `tauri build` via a `--config` override (so the checked-in `tauri.conf.json` never needs a real-or-placeholder thumbprint baked into it). Absent, it logs "No WINDOWS_CERTIFICATE secret configured - shipping unsigned build" and the NSIS installer builds exactly as it always has.

## macOS: signing + notarization

### What you need to obtain

1. **Enroll in the [Apple Developer Program](https://developer.apple.com/programs/)** — $99/year, requires an Apple ID. This is a hard requirement; there's no free tier that produces a certificate Gatekeeper will accept.
2. **Generate a "Developer ID Application" certificate** (not "Apple Development" or "Mac App Store" — those are for different distribution channels). In Xcode: Settings → Accounts → your Apple ID → Manage Certificates → "+" → "Developer ID Application". Or via the [developer portal](https://developer.apple.com/account/resources/certificates/list) directly.
3. **Export it as a `.p12` file** from Keychain Access (right-click the certificate → Export), setting a password when prompted — that password is a separate secret from the file itself.
4. **Create an app-specific password** for notarization, at [appleid.apple.com](https://appleid.apple.com) → Sign-In and Security → App-Specific Passwords. (An App Store Connect API key is the alternative Tauri also supports, but needs a different set of secrets than what this workflow currently wires up — the app-specific-password path below is what's implemented.)
5. **Find your Team ID** — in the developer portal, top-right, or via `xcrun altool --list-providers -u your@email.com -p your-app-specific-password` in a terminal.

### GitHub secrets this workflow expects

| Secret name | What it is |
|---|---|
| `APPLE_CERTIFICATE` | The `.p12` certificate file, base64-encoded (`base64 -w0 your-cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting the `.p12` |
| `APPLE_ID` | The Apple ID email address enrolled in the Developer Program |
| `APPLE_PASSWORD` | The app-specific password from step 4 above (**not** your real Apple ID password) |
| `APPLE_TEAM_ID` | Your Team ID from step 5 |

Add all five the same way as the Windows secrets above. `build-macos` in `release.yml` imports the certificate into a fresh, ephemeral keychain (the standard pattern for unattended CI signing — a GitHub-hosted runner has no keychain unlocked by default), extracts the real "Developer ID Application" signing identity from it, and passes everything through as environment variables to `tauri build` — which is itself where Tauri's own signing/notarization logic lives; nothing here re-implements `codesign`/`notarytool` by hand. Absent `APPLE_CERTIFICATE`, it logs "No APPLE_CERTIFICATE secret configured - shipping unsigned, non-notarized build" and the `.dmg` builds exactly as it always has.

Notarization specifically only runs once signing itself succeeds — a signed-but-not-notarized build is possible (if only the certificate secrets are set, not the Apple ID ones) and still triggers Gatekeeper's warning, just a different one than a fully unsigned build. Set all five secrets together for the full effect.

## What this project will never do as a substitute

**No self-signed certificate, ever, as a stand-in for the above.** A
self-signed Windows certificate or an ad-hoc macOS signature does not
remove SmartScreen's or Gatekeeper's warnings for anyone except the exact
machine that generated it — shipping one to real users would misrepresent
what's actually been achieved (a real, CA-issued or Apple-issued identity
behind the signature) while looking superficially "signed" in a way that
could mislead someone into trusting it more than an honestly-unsigned
build. If you see `self-signed` or `New-SelfSignedCertificate` proposed
anywhere in this codebase's history for release builds, that's a mistake
to revert, not a legitimate interim step.

## Verifying it actually worked, once you've added real secrets

1. Trigger the release workflow (Actions tab → Release → Run workflow, or push a `v*` tag).
2. Open the `build-windows` and `build-macos` jobs' logs. Confirm you see the real "Imported certificate..." lines, not the "No ... secret configured" ones.
3. Download the resulting installer and check it yourself before telling anyone else it's signed:
   - **Windows**: right-click the `.exe` → Properties → Digital Signatures tab. A real certificate chain should be listed.
   - **macOS**: `codesign -dv --verbose=4 /Applications/LocalSync.app` and `spctl -a -vvv /Applications/LocalSync.app` (should say "accepted" and name your Developer ID, not "rejected"). For notarization specifically: `spctl -a -t open --context context:primary-signature -vvv LocalSync.dmg` should mention "source=Notarized Developer ID".
4. Only once you've seen that real output yourself is it accurate to say installers are signed — this document existing, or the workflow steps being present, is not that confirmation.
