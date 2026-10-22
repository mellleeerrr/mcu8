# ADR-0028: Убийство codec-воркера по process group через `libc::kill`

**Статус:** accepted
**Дата:** 2026-10-23
**Связанные:** 0016 (Sans-I/O, изоляция), T06b (изолированные FFmpeg-воркеры)

## Контекст

`vks-media-mcu` запускает долгоживущий дочерний FFmpeg на кодек/направление.
При таймауте, отравлении pipe или teardown комнаты нужно убить **всю** группу
процессов: `kill_on_drop`/`Child::start_kill()` сигналят только лидеру, а
future FFmpeg может породить helper и пережить воркер. Вызов внешнего
`/usr/bin/kill` порождает процесс в hot-пути и зависит от PATH/пакета procps.

## Решение

На Unix воркер создаётся с `process_group(0)`, а группа убивается одним
syscall:

```rust
// SAFETY: pid — дочерний процесс этого воркера; отрицательный pid в kill()
// означает группу процессов; ESRCH/EPERM игнорируются.
unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
```

На не-Unix остаётся fallback через команду `kill`. `unsafe` локализован в
`vks-media-mcu/src/codec_worker/ffmpeg.rs`, имеет `// SAFETY:` и не затрагивает
`vks-core`.

## Последствия

- Нет блокирующего spawn в hot-пути, вся группа (включая forked helper) умирает.
- Добавлена прямая зависимость `libc` (уже была в workspace).
- Требует Unix для гарантии group-kill; на других платформах — best effort.

## Альтернативы

- `Child::start_kill()` — не убивает потомков (отклонено).
- Внешний `kill -KILL -- -pid` — PATH/procps-зависим, лишний процесс (отклонено).
- `PR_SET_PDEATHSIG` — не покрывает группу и требует unsafe в момент spawn
  (отложено до отдельного ADR).
