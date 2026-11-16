#!/bin/sh
# Fetches and shows a single frame without stopping the Kindle UI; for checking SERVER_URL.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=SCRIPTDIR/../miqat.conf
. "$DIR/miqat.conf"
# The UI repaints the screen as KUAL's menu closes, so draw only after that settles.
# Backgrounded so KUAL can close its menu without waiting on us.
(
  sleep 3
  if curl -fsS -m 30 -o /var/tmp/miqat-once.png "$SERVER_URL"; then
    eips -f -g /var/tmp/miqat-once.png
  else
    eips 1 2 "miqat: cannot reach $SERVER_URL"
  fi
) >/dev/null 2>&1 &
