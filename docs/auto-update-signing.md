# Auto-update signing: what's wired up, and what's still needed

**Status as of round 26: infrastructure only, still.** Round 16 wired the
whole auto-update mechanism (the `tauri-plugin-updater` plugin, the
`latest.json` manifest format, the release workflow's manifest-generation
step) — but no `TAURI_SIGNING_PRIVATE_KEY` secret has ever actually been
added to this repo. Confirmed directly, not assumed: the one real GitHub
Release this project has produced so far has installer assets but no
`latest.json` at all, and the release workflow's own manifest-generation
step explicitly and correctly skips itself whenever any of the three
platforms' `.sig` files are missing — which they always are without that
secret, since `createUpdaterArtifacts` gets turned off for the whole build
in that case (see `release.yml`'s own comments). This is a **different**
credential from code-signing (`docs/code-signing.md`) — that's about the
OS trusting the installer's publisher identity; this is about the *app*
trusting that an update it downloads later really came from whoever built
it, an independent mechanism Tauri's updater plugin requires regardless of
whether the installer itself is also OS-signed.

## What you need to obtain

Nothing external — this key pair is generated locally with Tauri's own
tooling, not issued by a CA or Apple. From `apps/desktop`:

```bash
npm run tauri signer generate -- -w ~/.tauri/localsync-updater.key
```

This prints a public key (also written to `~/.tauri/localsync-updater.key.pub`)
and writes the private key to `~/.tauri/localsync-updater.key`. It'll ask
whether to protect the private key with a password — optional, but if you
set one you'll need it as a second secret below.

**One real constraint worth knowing before you generate a new one**:
`apps/desktop/src-tauri/tauri.conf.json`'s `plugins.updater.pubkey` is
already set to a specific public key from round 16
(`dW50cnVzdGVkIGNvbW1lbnQ6...`) — if that key's matching private key
still exists somewhere (ask whoever ran round 16), use it rather than
generating a fresh pair, since a fresh pair's public half would need this
checked-in config value updated to match, and any previously-published
`latest.json`/installer would become permanently unverifiable against the
new key. If that private key is genuinely lost, generating a fresh pair is
fine — just update `pubkey` in `tauri.conf.json` in the same PR that adds
the new secret, so the two never drift apart.

## GitHub secrets this workflow expects

| Secret name | What it is |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | The **contents** of the private key file (`cat ~/.tauri/localsync-updater.key`), not a path — a GitHub Actions secret has no filesystem to point at |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password you set when generating the key, if any. If you didn't set one, add this secret anyway with an empty value — an entirely absent env var and an empty one behave the same way here (Tauri accepts an unprotected key either way) |

Add both under **Settings → Secrets and variables → Actions → New repository
secret** in this repo. Nothing else to configure — every one of `release.yml`'s
three build jobs already checks for `TAURI_SIGNING_PRIVATE_KEY` and signs the
platform's update-capable artifact (the `.exe`/AppImage/`.app.tar.gz`) with it
when present; the final `release` job's manifest-generation step picks up all
three resulting `.sig` files automatically and produces a real `latest.json`
once they're all there. Absent, every build step logs "No
TAURI_SIGNING_PRIVATE_KEY secret - building unsigned with
createUpdaterArtifacts disabled" and the installers still build exactly as
they always have — just without update-capable artifacts, and the manifest
step logs which platform's signature was missing and skips itself.

## Round 26: this alone isn't the whole fix

Even with this secret added and a real `latest.json` generated, both
consumers (the app's own "Check for updates" and the magic-link fallback
page) were separately broken by a second, unrelated issue: GitHub's
`/releases/latest` alias (and the `.../releases/latest/download/...` URL
form) only ever resolves to a **non-prerelease, non-draft** release, and
every release this project's workflow produces via `workflow_dispatch` is
deliberately marked prerelease (see `release.yml`'s own comment on why).
Round 26 fixed that independently by publishing a second, fixed `latest`
tag on every run that both consumers now address directly — see this
round's own report / commit messages for the details. Both fixes are
needed together: this document's secret makes `latest.json` exist at all;
round 26's tag fix makes it (and the release assets) actually reachable by
either consumer regardless of whether any release has ever been marked
non-prerelease.

## Verifying it actually worked, once you've added the secret

1. Trigger the release workflow (Actions tab → Release → Run workflow, or push a `v*` tag).
2. Open each build job's log (`build-windows`/`build-linux`/`build-macos`) and confirm you see "TAURI_SIGNING_PRIVATE_KEY is set" rather than the "No ... secret" line.
3. Open the `release` job's "Generate latest.json" step log — confirm it prints `generated=true` and shows the real manifest content (a `version`, three `platforms` entries, each with a non-empty `signature`), not the "skipping latest.json this run" message.
4. Confirm the resulting release (both the versioned tag and the `latest` tag — see round 26) actually has a `latest.json` asset attached, via the Releases page or `curl -s https://api.github.com/repos/decypher0/LocalSync/releases/tags/latest | jq '.assets[].name'`.
5. In a real installed build older than this one, open Settings → **Check for updates** and confirm it reports a real update available rather than an error — the actual install-and-restart click-through still needs real hardware per round 16's own original scope note; see `docs/round5-manual-test-checklist.md`.
