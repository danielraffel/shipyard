---
name: bump
description: Change the priority of a pending job (low, normal, high)
---

Change the priority of a pending Shipyard job. The user should provide a job ID and a priority level.

```bash
shipyard --json bump <job_id> <priority>
```

Priority must be one of: `low`, `normal`, `high`. Only pending jobs can be bumped — running or completed jobs cannot.

If the user says "make it urgent" or "prioritize this", use `high`. If they say "deprioritize" or "move it down", use `low`.
