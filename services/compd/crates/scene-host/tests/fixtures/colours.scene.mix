---
scene: 1
name: colours
citizen: scene-calendar
window: {"chrome":false,"edge":"right","kind":"edge","title":"Calendar","w":360}
---
```mix
root: {widget: "column", fill: true, children: ["cal_open", "panel", "cal_today"]}
# The calendar's actual cal_open node: no authored fill or text colour.
cal_open: {"label":"Open calendar app","on_click":"open_calendar","tone":"primary","widget":"button"}
# The bottom panel's root fill, painted over the same PANEL page token.
panel: {widget: "row", background: "#202326f2", children: []}
# Calendar navigation uses scene row fills and a separate text node.
cal_today: {widget: "row", background: "#ffffff0f", hover: "#ffffff1a", on_click: "cal_today", children: ["cal_today_t"]}
cal_today_t: {widget: "text", text: "Today", color: "#fcfcfc"}
```
