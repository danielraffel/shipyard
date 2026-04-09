---
name: cancel
description: Cancel a pending or running job
---

Cancel a Shipyard job. The user should provide a job ID.

```bash
shipyard --json cancel <job_id>
```

If the user doesn't specify a job ID, run `shipyard --json queue` first to show them what's running and let them pick.
