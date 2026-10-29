# T15/L0 host artifacts

Versioned host-tuning profile for `vks-mcu`:

- `sysctl/90-vks-mcu.conf` — UDP `rmem/wmem`, softnet backlog/budget,
  ephemeral port range + reserved media ports.
- `systemd/vks.slice` — cgroup v2 slice (`MemoryHigh/MemoryMax/CPUWeight/
  IOWeight/TasksMax`).
- `systemd/vks-mcu-prod.service.d/10-l0-resources.conf` — drop-in for the
  production system unit.
- `systemd/user/vks-mcu.service.d/10-l0-resources.conf` — drop-in for the dev
  user unit on the local stand.

Apply / verify / roll back with `scripts/host-tune.sh`; check *effective*
values (sysctl, cgroup, NOFILE, UDP `SO_RCVBUF`) with
`scripts/host-doctor.sh`.

Full guide: [`docs/operations/host-l0-tuning.md`](../../docs/operations/host-l0-tuning.md).

Root is required for sysctl and system units (no passwordless sudo on the
current stand); the T15 delivery does **not** apply anything to the live host —
that is a T17 restart-window operation.
