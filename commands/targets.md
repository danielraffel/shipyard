---
name: targets
description: Show configured targets, their backends, and reachability
---

Show all Shipyard targets with their current status.

```bash
shipyard --json targets
```

This shows which targets are configured, what backend each uses (local, ssh, cloud), whether they're reachable, and what fallback chain is configured. Report the active profile if profiles are configured.
