# Cloud drop (Google Drive transport): what you need to obtain

**Status as of round 23: the app-side integration is real and tested, but
inert without a Google OAuth Client ID.** No such credential exists anywhere
this project has touched — Claude Code cannot create one, since it requires
registering a real project in Google Cloud Console under a real Google
account. Without it, Settings → **Link Google account** shows a clear message
pointing back at this file instead of doing anything, and Cloud drop mode
isn't selectable. Adding the real credential is "set one environment
variable," not "figure out the OAuth flow from scratch" — that part is done.

## What you need to obtain

1. **A Google Cloud project.** [console.cloud.google.com](https://console.cloud.google.com) → create a new project (or reuse an existing one) — this is free, no billing account required for what this app uses.
2. **Enable the Google Drive API** for that project: APIs & Services → Library → search "Google Drive API" → Enable.
3. **Configure the OAuth consent screen** (APIs & Services → OAuth consent screen):
   - User type: **External** (unless everyone who'll ever use this app has a Google Workspace account in the same organization as you — unlikely for an open-source tool, so External is almost certainly what you want).
   - App name, support email, developer contact — anything real; Google shows this on the consent screen users see.
   - **Scopes**: add `openid`, `.../auth/userinfo.email`, `.../auth/userinfo.profile`, `.../auth/drive.file`, and `.../auth/drive.readonly`. See "A note on scopes and verification" below before assuming you need to add anything broader.
   - **Test users**: while your app is in "Testing" publishing status (the default, and almost certainly what you want to stay in — see below), add the Google accounts of everyone who'll actually test or use Cloud drop mode here. Google caps this at 100 users.
4. **Create an OAuth Client ID** (APIs & Services → Credentials → Create Credentials → OAuth client ID):
   - Application type: **Desktop app** (not "Web application" — this app doesn't have a server to hold a client secret, and Google's own guidance for installed/native apps is the Desktop app type + PKCE, no secret at all — see below).
   - Give it a name, create it. Copy the **Client ID** it shows you (a string ending in `.apps.googleusercontent.com`) — that's the one value this app needs. There's a "Client secret" shown too; **this app never uses it** (see below) — you don't need to save it, and it should never be pasted into anything this app reads, since a "secret" hardcoded into a distributed desktop binary isn't actually secret.

## Configuring the app to use it

Set the `GOOGLE_OAUTH_CLIENT_ID` environment variable before launching LocalSync, to the Client ID from step 4:

```
GOOGLE_OAUTH_CLIENT_ID=123456789-abc...xyz.apps.googleusercontent.com ./localsync-desktop
```

(On Windows, set it as a normal environment variable before launching the
`.exe` — System Properties → Environment Variables, or `$env:GOOGLE_OAUTH_CLIENT_ID = "..."`
in the same PowerShell session you launch from.) A real, permanent build
would bake a real Client ID into the app at build time instead of requiring
this to be set by hand every launch — not done here since no real Client ID
exists yet to bake in; whoever adds one should also decide where it's read
from long-term (an env var read at startup is the simplest thing that works
today, and is what `crates/ls-clouddrop` actually implements).

## Why PKCE, no client secret, and a loopback redirect (not the old copy-paste code flow)

Google **deprecated the `urn:ietf:wg:oauth:2.0:oob` "copy this code into the
app" flow in 2022** — it's no longer available for new OAuth clients. The
current, Google-documented approach for a native desktop app is:

- **Authorization Code flow with PKCE** (RFC 7636) instead of a client
  secret. A secret baked into a distributed desktop binary isn't actually
  secret (anyone can extract it from the binary) — PKCE proves the app that
  requests the token is the same one that started the flow, using a
  freshly-generated, single-use `code_verifier`/`code_challenge` pair
  instead. This is exactly why the Client ID page above doesn't ask you to
  wire up a client secret anywhere.
- **A loopback IP redirect** (`http://127.0.0.1:<a locally-chosen free port>`)
  instead of the old out-of-band code. `crates/ls-clouddrop` opens the
  system's real default browser to Google's real consent screen, and starts
  a short-lived local HTTP listener on an OS-assigned free port (the same
  "bind port 0, let the OS pick" trick `crates/ls-net`'s LAN detection
  already uses, for a different purpose) to catch the redirect once the user
  approves. Nothing about this needs a "Web application"-type client or a
  registered redirect URI list on Google's side beyond what "Desktop app"
  clients already allow by default for loopback addresses.

## A note on scopes and verification

`drive.file` (non-sensitive, "basic" verification only) is what the
**sender** needs — it only ever touches files this app itself creates.
`drive.readonly` (a **sensitive** scope, one tier up) is what the
**receiver** needs, and that's a real, confirmed finding worth understanding
before you're surprised by it later: **`drive.file` alone cannot read a file
someone else's app shared with you via a Drive permission grant** — it only
covers files your own app created, or files you explicitly pick through your
app's own file dialog or Google's separate Picker *API* widget. Confirmed
against Google's current documentation
([Choose Google Drive API scopes](https://developers.google.com/workspace/drive/api/guides/api-specific-auth)),
not assumed. Since either side of LocalSync can act as sender or receiver
(every instance is symmetric, per round 8), this app requests the union —
`drive.file` **and** `drive.readonly` — in one linking flow, rather than
having two different "link as sender" / "link as receiver" buttons.

**What this means for you in practice:** while your app is in "Testing"
publishing status (the default — see step 3 above), sensitive scopes like
`drive.readonly` work exactly the same as non-sensitive ones, for the up-to-100
test users you've explicitly added. You do **not** need to submit for
Google's full OAuth verification review just to use this yourself or share
it with a small group of testers. Full verification (a demo video, a privacy
policy URL, possibly a third-party security assessment) only becomes
necessary if you want to publish the consent screen for the general public
beyond 100 named test users — a real, separate decision for later, not
something this round needed to solve.

## A note on per-file expiration ("auto-expire after 24h" / a custom date)

Google Drive's Permission resource has a real `expirationTime` field — but
**it's a Google Workspace feature, not available on personal (consumer
Gmail) accounts**, per Google's own Workspace Updates announcements (this
feature was only ever announced in a Workspace context) and consistent
independent reporting. Google's own API reference for the field doesn't
spell this restriction out explicitly, so don't take that reference page
alone as proof either way — this is a real-world-behavior finding, not
something stated plainly in one canonical place. `crates/ls-clouddrop`
**always attempts** to pass `expirationTime` when granting access (harmless,
and gives real, native, Drive-enforced expiration for free if the account on
either end happens to be a Workspace account), but never assumes it worked —
it checks Drive's actual response and falls back to the app doing the
deletion itself (checked whenever LocalSync is running) if Drive didn't
honor it. If you personally have a Workspace account and want to confirm
which case you're in, check the real `Permission` object Drive's API returns
right after granting access — an `expirationTime` field present in the
response means it was honored; absent means it was silently dropped.

## Verifying it actually works, once you've added a real Client ID

1. Set `GOOGLE_OAUTH_CLIENT_ID` and launch the app. Settings → **Link Google
   account** should open your real default browser to a real Google consent
   screen listing the real scopes above (with a "Google hasn't verified this
   app" warning, expected while in Testing status — click **Continue** if
   you're signed in as one of the test users you added in step 3).
2. After approving, the browser tab should show a plain confirmation page and
   the app should show your linked email in Settings.
3. Send something via **Cloud drop** mode on one linked account, and receive
   it on a second linked (test-user) Google account on another machine —
   this is the one part of this round that genuinely needs two real accounts
   and two real machines/browsers; nothing in this sandbox could simulate it.
4. Try rejecting an access request once, and confirm (via
   [drive.google.com](https://drive.google.com)'s own sharing UI on the
   sender's account, not just trusting the app) that the file still shows no
   one else has access — that's the real, independent confirmation this
   round's whole design point (Drive-native visibility into who has access)
   is meant to give you.
