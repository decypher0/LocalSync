# Troubleshooting

## Every Run fails: "Podman on this computer can't apply the memory limit..."

LocalSync's sandbox gives every container a 1 GB memory limit. If Podman
can't apply it, no container can start, and Run (and the compose wizard's
test run) fails with this message. The setup screen's last check ("A test container
starts") fails the same way, and Run's error has a "Fix setup" button that
reopens it - after fixing Podman, use Recheck there. The raw error underneath looks like:

    crun: open `memory.max` for writing: No such file or directory

**Windows, WSL 3.0.1:** the WSL 3.0.1 update (kernel 6.18, released
September 30, 2026) changed how WSL sets up cgroups. Distro processes now
land in a WSL-created `/non-systemd` group that doesn't pass the memory
controller down, so rootless Podman inside the Podman machine can't set
memory limits at all. Restarting the Podman machine or running
`wsl --shutdown` does not help.

Check your version with `wsl --version`. The fix is rolling WSL back to a
2.x release (install the 2.x `.msi` from the WSL GitHub releases page),
then `wsl --shutdown` and `podman machine start`. Confirm it works with:

    podman run --rm --memory 1g docker.io/library/busybox echo ok

LocalSync does not drop the memory limit to work around this; the limit
is part of the receiver's sandbox.
