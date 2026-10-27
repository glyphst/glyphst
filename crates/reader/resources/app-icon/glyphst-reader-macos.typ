// The macOS app icon: the same g▌ on a rounded square 824 wide on Apple's
// 1024 grid, with a transparent margin and a faint inner hairline
// (design/reader/main.pen, frame 11 · App icon).
#let side = 32pt * 824 / 1024
#let inset = (32pt - side) / 2
#let k = side / 32pt
#let hair = 32pt / 512
#set page(width: 32pt, height: 32pt, margin: 0pt, fill: none)
#place(dx: inset, dy: inset, rect(width: side, height: side, radius: side * 0.2237, fill: rgb("#0E0F11")))
#place(dx: inset + hair / 2, dy: inset + hair / 2, rect(width: side - hair, height: side - hair, radius: side * 0.2237 - hair / 2, stroke: hair + rgb("#FFFFFF14")))
#place(dx: inset + 3.2pt * k, dy: inset + 1.5pt * k, text(font: "JetBrains Mono", weight: "bold", size: 27pt * k, fill: rgb("#E8EBEF"))[g])
#place(dx: inset + 20.5pt * k, dy: inset + 7pt * k, rect(width: 8.5pt * k, height: 17pt * k, fill: rgb("#FFB000")))
