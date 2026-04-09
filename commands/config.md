---
name: config
description: Show or switch Shipyard configuration and profiles
---

Manage Shipyard configuration. Supports several subcommands:

**Show effective config:**
```bash
shipyard config show
```

**Switch profile:**
```bash
shipyard config use <profile_name>
```
Common profiles: `local` (Mac only), `normal` (Mac + cloud), `full` (Mac + VMs + cloud fallback).

**List available profiles:**
```bash
shipyard config profiles
```

**Set a config value:**
```bash
shipyard config set <key>=<value>
```

When the user asks "switch to local" or "go cloud" or "what profile am I on", use the appropriate subcommand. Always report the current profile and active targets after switching.
