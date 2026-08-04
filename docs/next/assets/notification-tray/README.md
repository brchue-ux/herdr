# Notification tray — capture evidence

Captures of the tray in the states it ships with, produced from the real render
path: `render_sidebar` into a ratatui `TestBackend`, with the real badge artwork
from `ui::sidebar::tray_art` composited at the exact rect
`ui::signal_tray_graphics_rect` resolves — which is what a Kitty-graphics host
does with the same image. Cell metrics are 9 × 18 px, the size measured on the
terminal these were designed for.

| Capture | What it shows |
| --- | --- |
| `tray-states.png` | the eight badges idle, active and demanding attention, in a 42-column panel |
| `badge-sheet.png` | the eight marks alone, three states, at 64 px |
| `popup-ask-push.png` | `ask` showing the agent's real question; `push` naming its branch, count and exact command |
| `popup-sync.png` | `sync` refusing on a dirty tree, and offering its command on a clean one |
| `popup-checks-legend.png` | a jump-only badge, and the legend behind the `···` button |
| `tray-narrow-short.png` | a 26-column panel, and a panel short enough to drop the tray to its 8-row tier |
