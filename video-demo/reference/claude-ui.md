# Claude Code palette

Measured from a native capture of Claude Code 2.1.289 at 120 × 40 with the DeLM board open.

| Element | Color |
| --- | --- |
| Terminal background | `#181818` |
| Board background | `#262626` |
| Default text | `#d5d5d5` |
| Bold text | `#ffffff` |
| Secondary text, section labels, Available | `#999999` |
| DeLM, Working, imported contributions | `#6b8fff` |
| Command menu and key hints | `#b1b9f9` |
| Sent message background | `#373737` |
| Prompt rules | `#888888` |
| Clawd | `#d77757`, with `#000000` eyes |

## Layout

The film's terminal is 129 × 30 characters of SF Mono at 21 px (12.96 px advance, 28 px rows). The conversation uses the first 71 columns. The board starts at column 71 with a `│` divider, spans the top 26 rows, and leaves the prompt, its two rules, and the footer across the full width. Board text starts two columns after the divider; `✕` sits two columns from the right edge.

The board lists its phase beside DeLM, then AGENTS, TASK QUEUE, SHARED CONTEXT, and `d: Details   h: Hide`. Agent rows read `1  #1 Physics + level · Working`. Task rows align status at a fixed column: `Available`, `Claimed · Agent 1`, or `Done · Agent 1`. Each shared entry shows its title, `Agent 1 published · 4s ago`, its summary, and `Agent 2 imported this contribution.` Phase and status labels match the plugin's renderer.
