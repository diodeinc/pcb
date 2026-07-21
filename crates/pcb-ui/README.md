# pcb-ui

`pcb-ui` provides the terminal spinners, progress bars, status styles, and text
utilities used by the PCB CLI.

Spinners and progress bars write to standard error. Completion methods consume
the active indicator, and `suspend` hides it while the application prompts for
input. Text helpers provide width-aware truncation, padding, and alignment.

The crate exposes its common types through `pcb_ui::prelude`.

```bash
cargo test -p pcb-ui
```
