# Cloud drop

Cloud drop sends a project through the sender's own Google Drive instead of a
direct connection - useful when the two computers can't reach each other.
The receiver downloads it whenever they're ready, and only after the sender
approves their Google account.

## Who needs what

- **Both people need a Google account linked in LocalSync** (Settings → Link
  Google account). The sender's account holds the upload; the receiver's
  account is what the sender shares the file with - Drive shares a file with
  one specific email, not with "anyone with the link".
- P2P (Local network) and Remote relay transfers don't need Google at all.
- If the receiver picks Cloud drop without a linked account, the receive form
  says so and offers **Link Google account** right there; after signing in the
  receive continues with the code they already typed. Nothing is created until
  then, so a receive that can't start leaves nothing behind.
- Nothing has to be configured by hand. (The Google sign-in client is built
  into release builds - see [google-drive-setup.md](google-drive-setup.md) if
  you build LocalSync yourself.)

## How it goes

1. **Sender:** in a session, choose Cloud drop and send. LocalSync uploads the
   project to the sender's Drive and shows a code (with Copy code / Copy link
   and a 5-minute countdown) as soon as the upload finishes.
2. **Receiver:** New session → Receive → Cloud drop, enter the code. This asks
   the sender for access with the receiver's Google email.
3. **Sender:** the transfer card (and a banner) shows "<email> asks for
   access" - **Grant access** shares the file with that one email.
4. **Receiver:** the download starts; the project then opens for review like
   any other receive, and Run brings it up - database included.

On the sender's session page the receiver then appears under Devices by
email, with "Last sent" and its transfer speed, like a device reached P2P.

If nobody enters the code within 5 minutes, the card says so; the file stays
in Drive and **Retry** gets a new code for it (without uploading again, as long
as the project hasn't changed).

## What is sent

The same package a P2P send carries - the session's prepared snapshot:

- the project as committed in git, plus uncommitted edits to tracked files
  (untracked files are left out unless you `git add` them);
- the run instructions LocalSync's Setup made for a project without its own
  `docker-compose.yml` (a generated compose file and Dockerfile), or the
  project's own `docker-compose.yml`;
- the database dumps chosen in Setup.

It's signed by the sender; the receiver checks the signature and shows what
changed before anything runs. The Drive file follows the retention chosen when
sending (delete once downloaded, after 24 hours, or at a set time).

## Push update to a Cloud drop receiver

A device reached by Cloud drop is updated the same way it was reached: Push
update uploads the new version (the whole project, not just what changed),
shows a new code to share, and the sender approves the receiver's access
again.

## When something goes wrong

- **"Your Google sign-in expired. Link your account again to use Cloud drop."**
  Google no longer accepts the saved sign-in (revoked, expired, or a test
  consent screen's 7-day limit). Use the **Link Google account** button shown
  with the message; the transfer can then be retried.
- **"This build's Google client is misconfigured…"** The app's built-in Google
  client was rejected. Signing in again won't help; it needs a fixed build.
- **"No one entered this code within 5 minutes…"** Retry for a new code.
