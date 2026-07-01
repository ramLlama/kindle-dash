#!/bin/sh
# KUAL launcher for kindle-dash.
#
# KUAL execs this script (an action that is a bare command line is not run reliably)
# and then returns to Home. The binary is started in its own session via `setsid` so
# that when kindle-dash stops the `framework` UI job — to take over the e-ink
# framebuffer — the launcher's teardown can't take the dashboard down with it.
setsid /mnt/us/kindle-dash/kindle-dash </dev/null >/dev/null 2>&1 &
