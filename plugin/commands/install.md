---
description: Install or update the btp CLI that the Balance ton port hooks use
---

The user asked to install or update `btp` for the Balance ton port plugin. Run this with the Bash tool:

```bash
bash "${CLAUDE_PLUGIN_ROOT}/scripts/install-btp.sh" --data-dir "${CLAUDE_PLUGIN_DATA}" --clear-decline
```

It clears an earlier "no" to the install, downloads the latest btp release, checks its sha256 and its Developer ID signature, and installs it to `~/.local/bin/btp` (never with sudo, and it only replaces a btp this plugin installed). Tell the user what it printed. If it refused, give the reason from its last line and do not try to install btp another way.
