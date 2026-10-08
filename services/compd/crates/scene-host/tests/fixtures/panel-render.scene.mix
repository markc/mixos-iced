---
scene: 1
name: panel
citizen: test
window: {"kind":"edge","edge":"bottom","h":52}
---
```mix
root: {widget: "row", fill: true, align: "center", padding: 4, gap: 4, children: ["pager", "tasks", "fill", "clock", "peek"]}
pager: {widget: "list", flow: "horizontal", align: "center", gap: 3, row: "ws_btn", row_height: 30, rows: [{id: "1", cells: ["1"]}, {id: "2", cells: ["2"]}, {id: "3", cells: ["3"]}, {id: "4", cells: ["4"]}]}
ws_btn: {widget: "row", height: 30, padding: 8, align: "center", justify: "center", children: ["ws_label"]}
ws_label: {widget: "text", text: "{cells[0]}", size: 12}
tasks: {widget: "list", flow: "horizontal", row: "task_btn", row_height: 40, rows: [{id: "task", cells: ["Terminal"]}]}
task_btn: {widget: "row", height: 40, padding: 8, gap: 8, align: "center", children: ["task_icon", "task_label"]}
task_icon: {widget: "image", w: 24, h: 24, src: ""}
task_label: {widget: "text", size: 13, width: 170, text: "{cells[0]}", elide: true}
fill: {widget: "spacer"}
clock: {widget: "row", height: 44, padding: 10, align: "center", children: ["clock_col"]}
clock_col: {widget: "column", align: "center", children: ["clock_time", "clock_date"]}
clock_time: {widget: "text", size: 19, bold: true, text: "11:54 am"}
clock_date: {widget: "text", size: 11, text: "Sun 4 Oct"}
peek: {widget: "row", height: 44, padding: 6, align: "center", children: ["peek_i"]}
peek_i: {widget: "image", w: 20, h: 20, src: ""}
```
