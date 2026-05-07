---
name: config
description: Inspect Shipyard configuration files and effective cloud defaults
---

Shipyard does not have a `shipyard config` subcommand yet.

Use these entry points instead:

- Project config: `.shipyard/config.toml`
- Machine-local overrides: `.shipyard.local/config.toml`
- Environment and tool health: `shipyard doctor --json`
- Effective cloud workflow/provider resolution: `shipyard cloud defaults --json`
- Active job and target state: `shipyard status --json`

If the user asks to "switch profiles", "go local", or "go cloud", use
`shipyard config profiles` to inspect options and `shipyard config use <profile>`
to switch an existing profile.

Examples:

**Inspect cloud defaults:**
```bash
shipyard cloud defaults --json
```

**Inspect current job and target state:**
```bash
shipyard status --json
```
