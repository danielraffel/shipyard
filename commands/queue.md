---
name: queue
description: Show all jobs in the queue with priorities and status
---

Show the Shipyard job queue — running, pending, and recent jobs.

```bash
shipyard --json queue
```

Report the results to the user: what's running, what's pending, and what recently completed. Include job IDs so the user can bump, cancel, or check logs.
