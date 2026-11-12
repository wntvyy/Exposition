#set page(width: 14cm, height: 18cm, margin: 1.5cm)
#set par(justify: true)

= Exposition Basics

The first paragraph explains the setting in plain prose. It is long enough to wrap across several lines on the page, so that inserting text reflows the following words onto new lines. Offices file their reports with the \#tag marker.

The second paragraph has inline math such as $a^2 + b^2 = c^2$ and a footnote.#footnote[A footnote with its own text.] The display equation follows:

$ sum_(k=1)^n k = (n (n + 1)) / 2 $

#figure(
  rect(width: 60%, height: 2cm, fill: luma(230)),
  caption: [A gray rectangle standing in for a figure.],
)
